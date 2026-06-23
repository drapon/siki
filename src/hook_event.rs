use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::session::VALID_HOOK_STATES;
use crate::session_start::{read_stdin_with_timeout, send_line_to_broker, STDIN_READ_MAX};

/// 状態系 hook の stdin 読み取りタイムアウト。
///
/// 状態系 hook は `is_async=true` / hook timeout=5000ms（`hooks.rs`）で起動される。
/// `run` は stdin 読み取り（このタイムアウト）→ broker 送信（connect 最大
/// `BROKER_CONNECT_TIMEOUT`=2s + write 最大 `BROKER_WRITE_TIMEOUT`=1s）を直列に実行する。
/// これらの合計が hook timeout を超えると Claude Code がプロセスを強制終了し、状態イベントを
/// 取りこぼす。1s なら最悪でも 1+2+1=4s に収まり hook timeout(5s) にマージンを残せる
/// （write は数十バイトなので connect 成功後ほぼ即時で、現実的な所要は stdin+connect が支配的）。
/// （SessionStart の `STDIN_READ_TIMEOUT`=3s は `is_async=false` で hook timeout を持たない
/// 前提の値なので、状態系 hook には流用しない。）
/// Claude Code は hook 起動と同時に stdin を書き込むため、通常は 1ms 未満で読み終わる。
///
/// このタイムアウトを変更する場合は broker 側タイムアウトとの合計が hook timeout
/// （`hooks.rs` の 5000ms）を下回ることを必ず確認すること。
const HOOK_EVENT_STDIN_TIMEOUT: Duration = Duration::from_secs(1);

/// 状態系 hook（PreToolUse / PermissionRequest / PostToolUse / Stop / SessionEnd）の実装。
///
/// 従来は各 hook を `echo '{...}' | nc -U <sock>` で実装していたが、これには
/// 環境依存の不安定要因が 2 つあった:
///
/// 1. `nc`(netcat) が無い／`-U`(Unix ソケット) 非対応の変種だと、全状態イベントが
///    無言で消える（セッションが Working のまま固まる／状態が更新されない）。
/// 2. session_id を `sed` 正規表現で stdin から抜くため、Claude Code 側の payload 整形の
///    差異で抽出に失敗し、フォールバックの `siki-$$`（PID 由来）という幽霊セッションを
///    更新してしまう。本物のセッションは永遠に状態遷移しない。
///
/// siki バイナリ自身が stdin の JSON を serde で解釈し、broker に 1 行 JSON を送ることで
/// 両方を解消する（`session-start` hook と同じ堅牢な経路を使う）。
pub fn run(sock_path: &Path, state: &str) -> Result<()> {
    if !VALID_HOOK_STATES.contains(&state) {
        // 未知の状態は broker 側でも捨てられるが、ここで弾いて送信自体を避ける。
        eprintln!(
            "siki hook-event: unknown state {:?}; for session registration use `siki session-start`",
            state
        );
        return Ok(());
    }

    let input = read_stdin_with_timeout(HOOK_EVENT_STDIN_TIMEOUT, STDIN_READ_MAX, "siki hook-event");
    let input_json: Value = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));

    let session_id = input_json
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // session_id 無しでは更新対象が定まらない。フォールバックで PID 由来の幽霊 ID を
    // 作ると本物のセッションとは別行を更新してしまうため、ここで終了する。
    // stdin タイムアウト・payload 変更・JSON パース失敗のいずれでもここに来るため、
    // 全状態遷移が無言で止まるのを避けるべく stderr に診断を残す（stdout は使わない）。
    if session_id.is_empty() {
        eprintln!(
            "siki hook-event: missing session_id for state {:?}; dropping event",
            state
        );
        return Ok(());
    }

    // working(PreToolUse) のときだけ、実行ツールから人間可読な activity を抽出して同梱する。
    // それ以外の状態系イベントは従来どおり session_id のみ（後方互換）。
    let payload = if state == "working" {
        let activity = input_json
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(|tool_name| {
                format_activity(tool_name, input_json.get("tool_input").unwrap_or(&Value::Null))
            })
            .filter(|a| !a.is_empty());
        match activity {
            Some(act) => json!({ "event": state, "session_id": session_id, "activity": act }),
            None => json!({ "event": state, "session_id": session_id }),
        }
    } else {
        json!({ "event": state, "session_id": session_id })
    }
    .to_string();
    send_line_to_broker(sock_path, payload, &format!("siki hook-event ({state})"));
    Ok(())
}

/// PreToolUse の `tool_name` / `tool_input` から、監視ビュー表示用の1行 activity を生成する。
///
/// ツール別ルール:
/// - `Bash` → `description` 優先、無ければ `command`
/// - `Edit`/`Write`/`MultiEdit`/`Read`/`NotebookEdit` → `file_path` のベース名
/// - `Task` → `Task({subagent_type}): {description}`
/// - `Grep`/`Glob` → `pattern`
/// - `WebFetch` → `url`
/// - その他 → ツール名のみ
///
/// いずれも `normalize_one_line` で改行・制御文字を1行に正規化する。
/// 表示幅に応じた省略は呼び出し側（UI）で行う。
fn format_activity(tool_name: &str, tool_input: &Value) -> String {
    // 空文字列は「未指定」とみなす（Bash の description="" → command フォールバック、
    // ファイル系の末尾コロン化を防ぐ）。
    let field = |key: &str| {
        tool_input
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };

    let raw = match tool_name {
        "Bash" => match field("description").or_else(|| field("command")) {
            Some(v) => format!("Bash: {v}"),
            None => "Bash".to_string(),
        },
        "Edit" | "Write" | "MultiEdit" | "Read" | "NotebookEdit" => match field("file_path") {
            Some(p) => {
                let base = Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p);
                format!("{tool_name}: {base}")
            }
            None => tool_name.to_string(),
        },
        "Task" => {
            let desc = field("description").unwrap_or("");
            match (field("subagent_type"), desc.is_empty()) {
                (Some(t), false) => format!("Task({t}): {desc}"),
                (Some(t), true) => format!("Task({t})"),
                (None, false) => format!("Task: {desc}"),
                (None, true) => "Task".to_string(),
            }
        }
        "Grep" | "Glob" => match field("pattern") {
            Some(p) => format!("{tool_name}: {p}"),
            None => tool_name.to_string(),
        },
        "WebFetch" => match field("url") {
            Some(u) => format!("WebFetch: {u}"),
            None => "WebFetch".to_string(),
        },
        other => other.to_string(),
    };

    normalize_one_line(&raw)
}

/// 改行・タブ・制御文字を空白に置換し、連続空白を1つに圧縮してトリムする（表示崩れ防止）。
fn normalize_one_line(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_activity_bash_prefers_description() {
        let input = json!({"command": "bun test", "description": "テスト実行"});
        assert_eq!(format_activity("Bash", &input), "Bash: テスト実行");
    }

    #[test]
    fn test_format_activity_bash_falls_back_to_command() {
        let input = json!({"command": "bun test"});
        assert_eq!(format_activity("Bash", &input), "Bash: bun test");
    }

    #[test]
    fn test_format_activity_edit_uses_basename() {
        let input = json!({"file_path": "/Users/x/proj/src/session.rs"});
        assert_eq!(format_activity("Edit", &input), "Edit: session.rs");
    }

    #[test]
    fn test_format_activity_read_uses_basename() {
        let input = json!({"file_path": "src/db.rs"});
        assert_eq!(format_activity("Read", &input), "Read: db.rs");
    }

    #[test]
    fn test_format_activity_task_with_subagent() {
        let input = json!({"subagent_type": "Explore", "description": "コード調査"});
        assert_eq!(format_activity("Task", &input), "Task(Explore): コード調査");
    }

    #[test]
    fn test_format_activity_unknown_tool_uses_name_only() {
        // 想定キーが無いツールは tool_name のみ（EDGE-001）
        let input = json!({});
        assert_eq!(format_activity("SomeMcpTool", &input), "SomeMcpTool");
    }

    #[test]
    fn test_format_activity_missing_field_no_panic() {
        // Bash で command/description が無くても panic せずツール名にフォールバック
        let input = json!({});
        assert_eq!(format_activity("Bash", &input), "Bash");
    }

    #[test]
    fn test_format_activity_normalizes_control_chars() {
        // 改行・タブを含む command を1行に正規化（EDGE-002）
        let input = json!({"command": "echo a\nb\tc   d"});
        assert_eq!(format_activity("Bash", &input), "Bash: echo a b c d");
    }

    #[test]
    fn test_normalize_one_line_trims_and_collapses() {
        assert_eq!(normalize_one_line("  a\n\n b \t c  "), "a b c");
        assert_eq!(normalize_one_line(""), "");
    }

    #[test]
    fn test_format_activity_bash_empty_description_falls_back() {
        // 空文字列 description は「無し」とみなし command にフォールバック
        let input = json!({"command": "ls", "description": ""});
        assert_eq!(format_activity("Bash", &input), "Bash: ls");
    }

    #[test]
    fn test_format_activity_grep_and_glob() {
        assert_eq!(
            format_activity("Grep", &json!({"pattern": "fn main"})),
            "Grep: fn main"
        );
        assert_eq!(
            format_activity("Glob", &json!({"pattern": "**/*.rs"})),
            "Glob: **/*.rs"
        );
    }

    #[test]
    fn test_format_activity_webfetch_and_task_variants() {
        assert_eq!(
            format_activity("WebFetch", &json!({"url": "https://example.com"})),
            "WebFetch: https://example.com"
        );
        // subagent_type なし・description あり
        assert_eq!(
            format_activity("Task", &json!({"description": "調査"})),
            "Task: 調査"
        );
        // どちらも無し
        assert_eq!(format_activity("Task", &json!({})), "Task");
    }
}
