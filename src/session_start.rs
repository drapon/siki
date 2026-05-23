use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::db;

/// stdin から JSON を読むときのタイムアウト。
///
/// SessionStart hook は `is_async=false` で注入されており、Claude Code は本プロセスの
/// 終了を待ってからセッションを開始する。`read_to_end` は EOF まで戻らないため、
/// stdio リダイレクト事故・Claude Code 側のバグ等で EOF が来ないとセッション開始が
/// 止まる（過去 `33719cf` / `75415c9` で同種の hook ブロッキングを 2 回踏んだ実績あり）。
/// 通常のペイロードは数 KB / 1ms 未満で読めるので、3 秒は余裕を持った打ち切り値。
const STDIN_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// stdin から読み取る最大バイト数。
///
/// Claude Code の hook 入力 JSON は典型的に 1 KB 未満。256 KB は安全マージンとして
/// 十分かつ、悪意ある巨大入力でも本プロセスの常駐メモリを 1 MB 以下に抑える。
const STDIN_READ_MAX: usize = 256 * 1024;

/// broker への connect + write を含む全体のタイムアウト。
///
/// 通常 Unix socket への 1 行書き込みは数 ms。broker (siki TUI 内) が panic で
/// accept ループから抜けた場合、`UnixStream::connect` 自体がブロックし得るため、
/// stdin 読み取りと別枠で短めに打ち切る。
const BROKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// SQLite の busy_timeout (ms)。
///
/// `db::init` のデフォルト (5000ms) と非対称だと、TUI / MCP / hook の同時アクセスで
/// hook 側だけ早期に SQLITE_BUSY で落ちる経路ができてしまうので揃える。
const DB_BUSY_TIMEOUT_MS: u32 = 5000;

/// 何かおかしかったときに stderr に短いメッセージを出す。
/// hook の stdout（= Claude のコンテキスト）には絶対に混ぜないこと。
macro_rules! diag {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), "siki session-start: {}", format_args!($($arg)*));
    }};
}

/// SessionStart hook の実装。
///
/// stdin から Claude Code が渡す JSON を読み、broker に register イベントを送信し、
/// `hookSpecificOutput.additionalContext` を stdout に出力する。
///
/// 従来は `.claude/rules/siki.md` が「最初のメッセージの前に list_sessions を呼べ」と
/// 指示していたが、それを廃止して SessionStart hook で直接コンテキストを注入する。
/// MCP ラウンドトリップ1回分を節約でき、レスポンスサイズもこちらで制御できる。
pub fn run(sock_path: &Path, db_path: &Path) -> Result<()> {
    // Claude Code は SessionStart hook に {"session_id": "...", "cwd": "...", ...} を
    // stdin で渡す。EOF が来ない事故（Claude Code バグ・リダイレクト事故）でも
    // hook 全体を止めないよう、別スレッド + channel でタイムアウトを設ける。
    let input = read_stdin_with_timeout(STDIN_READ_TIMEOUT, STDIN_READ_MAX);
    let input_json: Value = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));

    let session_id = input_json
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // session_id 無しでは broadcast 等を引いてしまうため、ここで終わる
    if session_id.is_empty() {
        diag!("missing session_id on stdin; nothing to do");
        return Ok(());
    }

    // Claude Code は stdin の JSON に cwd を含めるのでそれを優先する。
    // 無ければ process の cwd にフォールバック。
    let cwd = input_json
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let role = std::env::var("SIKI_ROLE").unwrap_or_else(|_| "default".to_string());

    // broker への register を送信（siki TUI が立ち上がっていない場合はサイレントにスキップ）
    register_with_broker(sock_path, &session_id, &cwd, &role);

    // DB から additionalContext 用の情報を取得
    // siki TUI が一度も走っていない環境では DB ファイルが存在しないので、その場合は
    // 空ファイルを作らずに何も出力しないで終了する（Claude Code は exit 0 + 空 stdout を
    // 「追加コンテキストなし」として扱う）
    if !db_path.exists() {
        return Ok(());
    }

    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            diag!("failed to open DB at {}: {}", db_path.display(), e);
            return Ok(());
        }
    };
    let _ = conn.execute_batch(&format!("PRAGMA busy_timeout={};", DB_BUSY_TIMEOUT_MS));

    let ctx = match build_additional_context(&conn, &session_id, &cwd) {
        Ok(c) => c,
        Err(e) => {
            diag!("failed to build additional context: {}", e);
            return Ok(());
        }
    };
    if ctx.is_empty() {
        return Ok(());
    }

    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx,
        }
    });
    println!("{}", output);
    Ok(())
}

/// stdin を `timeout` 内に読み切る。タイムアウトした場合は空文字列を返す。
/// 読み取り中の最大バイト数を `max_bytes` で制限する。
fn read_stdin_with_timeout(timeout: Duration, max_bytes: usize) -> String {
    let (tx, rx) = mpsc::channel::<String>();
    // 読み取りスレッド: タイムアウト後もメインは exit するので、スレッドのリーク
    // は許容する（プロセスが死ねば回収される）
    thread::spawn(move || {
        let mut buf = Vec::with_capacity(1024);
        let mut take = std::io::stdin().take(max_bytes as u64);
        let _ = take.read_to_end(&mut buf);
        let s = String::from_utf8_lossy(&buf).to_string();
        let _ = tx.send(s);
    });
    match rx.recv_timeout(timeout) {
        Ok(s) => s,
        Err(_) => {
            diag!("stdin read timed out after {:?}", timeout);
            String::new()
        }
    }
}

/// broker への register を別スレッドで実行し、`BROKER_CONNECT_TIMEOUT` 内に
/// 完了しなければ諦める（broker が固まっている場合への保険）。
fn register_with_broker(sock_path: &Path, session_id: &str, cwd: &str, role: &str) {
    if !sock_path.exists() {
        // siki TUI が起動していないのは正常パス。サイレントに無視。
        return;
    }
    let payload = json!({
        "event": "register",
        "session_id": session_id,
        "cwd": cwd,
        "role": role,
    })
    .to_string();
    let sock_path_owned: PathBuf = sock_path.to_path_buf();

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    thread::spawn(move || {
        let result = (|| -> std::io::Result<()> {
            let mut stream = UnixStream::connect(&sock_path_owned)?;
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
            stream.write_all(payload.as_bytes())?;
            stream.write_all(b"\n")?;
            Ok(())
        })()
        .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    match rx.recv_timeout(BROKER_CONNECT_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            diag!("broker register failed: {}", e);
        }
        Err(_) => {
            diag!("broker register timed out after {:?}", BROKER_CONNECT_TIMEOUT);
        }
    }
}

/// SessionStart 用の追加コンテキスト本文を組み立てる。
///
/// - 未読の pending_messages があれば本文に含める
///   - 単一セッション宛のみ既読化（broadcast/worktree fanout は触らない）
/// - 利用可能な会話サマリ件数と worktree_contexts の件数を「ポインタ」として提示
///   （フル本文はコンテキストを食うので必要なときだけ MCP 経由で取得させる）
/// - 有効な worktree であれば常に set_summary 呼び出しのリマインダーを末尾に付ける
pub fn build_additional_context(conn: &Connection, session_id: &str, cwd: &str) -> Result<String> {
    let (proj, wt) = crate::session::guess_names_from_cwd(cwd);
    let valid_worktree = !proj.is_empty() && !wt.is_empty() && proj != "unknown" && wt != "unknown";

    let mut sections: Vec<String> = Vec::new();

    let pending = db::get_pending_messages(conn, session_id, &wt, &proj).unwrap_or_default();
    if !pending.is_empty() {
        let mut s = String::from(
            "## Pending messages\n\nDeliver these to the user before continuing.\n",
        );
        for m in &pending {
            // 本文に `#` や code fence が含まれても周囲の Markdown 構造を壊さないよう
            // blockquote として埋め込む（複数行も > プレフィックスで安全）
            s.push_str(&format!(
                "\n### from `{}` ({})\n\n{}\n",
                escape_inline(&m.from_session),
                escape_inline(&m.message_type),
                blockquote(&m.content),
            ));
        }
        sections.push(s);

        // 単一セッション宛 (to_session = session_id) のみ既読化する。
        // broadcast/worktree fanout は他セッションにも配信する必要があるので mark しない。
        let ids: Vec<i64> = pending.iter().map(|m| m.id).collect();
        if let Err(e) = db::mark_messages_read_for_session(conn, session_id, &ids) {
            diag!("mark_messages_read_for_session failed: {}", e);
        }
    }

    if valid_worktree {
        let summaries =
            db::get_conversation_logs_by_worktree(conn, &wt, &proj).unwrap_or_default();
        let contexts = crate::config::load_contexts(&proj, &wt);

        let mut lines: Vec<String> = Vec::new();
        if !summaries.is_empty() {
            lines.push(format!(
                "- {} prior conversation summary file(s) available",
                summaries.len()
            ));
        }
        if !contexts.is_empty() {
            let total_bytes: usize = contexts.iter().map(|(_, c)| c.len()).sum();
            lines.push(format!(
                "- {} worktree context file(s) (~{} KB total)",
                contexts.len(),
                (total_bytes + 512) / 1024
            ));
        }
        if !lines.is_empty() {
            let mut s = String::from("## Background available for this worktree\n\n");
            for line in &lines {
                s.push_str(line);
                s.push('\n');
            }
            s.push_str(
                "\nFetch only what the current task needs:\n\
                - `siki:list_sessions { scope: \"worktree\" }` returns summaries and context bodies.\n\
                - `siki:get_context { target: { type: \"worktree\", id: \"<name>\" }, include_conversation_log: true }` is large — call it via an Explore or general-purpose subagent so the full log stays out of your main context.\n",
            );
            sections.push(s);
        }
    }

    // 有効な worktree でなければ何も注入しない（unknown は siki 外の worktree）
    if !valid_worktree && sections.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::from("# siki session context\n\n");
    for s in sections {
        out.push_str(&s);
        out.push('\n');
    }
    if valid_worktree {
        out.push_str(&format!(
            "When you start work, call `siki:set_summary` with a short task description (project `{}`, worktree `{}`).\n",
            proj, wt
        ));
    }
    Ok(out)
}

/// 文字列を Markdown のインラインコード/見出し用にサニタイズする。
/// バッククォートとパイプを取り除く程度の軽い処理にとどめる。
fn escape_inline(s: &str) -> String {
    s.replace('`', "'").replace('\n', " ")
}

/// 本文を Markdown の blockquote として整形する。複数行は各行に `> ` を付ける。
fn blockquote(s: &str) -> String {
    if s.is_empty() {
        return String::from("> (empty)");
    }
    s.lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {}", line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_db() -> Connection {
        db::init(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn empty_db_yields_empty_context() {
        let conn = test_db();
        let ctx = build_additional_context(&conn, "s1", "/tmp/nowhere").unwrap();
        assert!(ctx.is_empty(), "got: {:?}", ctx);
    }

    #[test]
    fn pending_single_target_is_marked_read() {
        let conn = test_db();
        db::insert_message(
            &conn,
            "s1",
            Some("s2"),
            None,
            None,
            "hello there",
            "message",
            None,
        )
        .unwrap();

        let ctx = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        assert!(ctx.contains("Pending messages"), "got: {}", ctx);
        assert!(ctx.contains("hello there"));

        // 単一セッション宛は既読化されているので 2 回目は出ない
        let ctx2 = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        assert!(!ctx2.contains("hello there"));
    }

    #[test]
    fn broadcast_is_not_marked_so_other_sessions_still_receive_it() {
        let conn = test_db();
        // broadcast: to_session/to_worktree/to_project 全 NULL
        db::insert_message(&conn, "s1", None, None, None, "global msg", "message", None).unwrap();

        // s2 が SessionStart で受信
        let ctx_s2 = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        assert!(ctx_s2.contains("global msg"));

        // s3 も SessionStart でちゃんと同じ broadcast を受信できる
        let ctx_s3 = build_additional_context(&conn, "s3", "/tmp/nowhere").unwrap();
        assert!(
            ctx_s3.contains("global msg"),
            "broadcast was consumed by s2; s3 lost it. got: {}",
            ctx_s3
        );
    }

    #[test]
    fn worktree_fanout_is_not_marked() {
        let conn = test_db();
        db::insert_message(
            &conn,
            "s1",
            None,
            Some("wt-x"),
            None,
            "worktree msg",
            "message",
            None,
        )
        .unwrap();

        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces
            .join("proj-x")
            .join("wt-x")
            .to_string_lossy()
            .to_string();

        let ctx_a = build_additional_context(&conn, "sess-a", &cwd).unwrap();
        assert!(ctx_a.contains("worktree msg"));

        let ctx_b = build_additional_context(&conn, "sess-b", &cwd).unwrap();
        assert!(
            ctx_b.contains("worktree msg"),
            "worktree fanout was consumed by sess-a; sess-b lost it. got: {}",
            ctx_b
        );
    }

    #[test]
    fn background_pointers_appear_when_logs_exist() {
        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces
            .join("proj-x")
            .join("wt-x")
            .to_string_lossy()
            .to_string();

        let conn = test_db();
        db::upsert_conversation_log(&conn, "old-sid", "wt-x", "proj-x", None, "[]").unwrap();

        let ctx = build_additional_context(&conn, "new-sid", &cwd).unwrap();
        assert!(ctx.contains("Background available"), "got: {}", ctx);
        assert!(ctx.contains("conversation summary"));
        assert!(ctx.contains("siki:set_summary"));
    }

    #[test]
    fn set_summary_reminder_is_emitted_for_valid_worktree_with_no_other_sections() {
        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces
            .join("proj-y")
            .join("wt-y")
            .to_string_lossy()
            .to_string();

        let conn = test_db();
        let ctx = build_additional_context(&conn, "sid-y", &cwd).unwrap();
        // pending messages も background も無いが、有効な worktree なら
        // set_summary リマインダーだけは出す（L-6）
        assert!(ctx.contains("siki:set_summary"), "got: {}", ctx);
        assert!(ctx.contains("proj-y"));
        assert!(ctx.contains("wt-y"));
    }

    #[test]
    fn message_body_with_markdown_special_chars_is_blockquoted() {
        let conn = test_db();
        // 本文に Markdown 見出しと code fence
        let payload = "# fake header\n```\nrm -rf /\n```\n## another";
        db::insert_message(&conn, "s1", Some("s2"), None, None, payload, "message", None).unwrap();

        let ctx = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        // 各行が `> ` で始まる blockquote として展開されている
        assert!(ctx.contains("> # fake header"), "got:\n{}", ctx);
        assert!(ctx.contains("> ```"));
        assert!(ctx.contains("> ## another"));
        // 我々が書いた構造的見出しはそのまま残る
        assert!(ctx.contains("### from"));
    }

    #[test]
    fn empty_session_id_via_run_does_nothing() {
        // run() の直接実行は stdin/stdout を奪うので、ここでは
        // build_additional_context が空 session_id でもパニックしないことだけ確認
        let conn = test_db();
        db::insert_message(&conn, "s1", None, None, None, "broadcast", "message", None).unwrap();
        let ctx = build_additional_context(&conn, "", "/tmp/nowhere").unwrap();
        // session_id が空でも broadcast にマッチしてしまうのが build_additional_context の素の挙動。
        // run() 側で session_id が空ならここに到達しないようガードしている（L-1）。
        // → 念のため、broadcast を含めるが mark_messages_read_for_session は no-op
        assert!(ctx.contains("broadcast"));
    }

    #[test]
    fn blockquote_handles_empty_and_multiline() {
        assert_eq!(blockquote(""), "> (empty)");
        assert_eq!(blockquote("a\nb"), "> a\n> b");
        assert_eq!(blockquote("a\n\nb"), "> a\n>\n> b");
    }
}
