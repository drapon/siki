use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::session_start::{read_stdin_with_timeout, send_line_to_broker, STDIN_READ_MAX};

/// Claude Code が hook 入力 JSON で渡してくる状態イベント。
/// broker 側の `HookEvent`（serde tag = "event"）に対応する。
///
/// `register` は含まない。セッション登録は `siki session-start` サブコマンドが
/// 専用経路で担当するため（broker の `HookEvent::Register` はそちらから送られる）。
/// broker 側の `HookEvent` に状態を追加した場合は、この配列にも追記すること
/// （未追記だと送信側でここで弾かれ、状態イベントが取りこぼされる）。
const KNOWN_STATES: &[&str] = &["working", "waiting", "refresh", "idle", "dead"];

/// 状態系 hook の stdin 読み取りタイムアウト。
///
/// 状態系 hook は `is_async=true` / hook timeout=5000ms（`hooks.rs`）で起動される。
/// `run` は stdin 読み取り（このタイムアウト）→ broker 送信（`BROKER_CONNECT_TIMEOUT`=2s）を
/// 直列に実行するため、両者の合計が hook timeout を超えると Claude Code がプロセスを
/// 強制終了し、状態イベントを取りこぼす。1s なら最悪でも 1+2=3s に収まり、
/// hook timeout(5s) に対して 2s のマージンを残せる。
/// （SessionStart の `STDIN_READ_TIMEOUT`=3s は `is_async=false` で hook timeout を持たない
/// 前提の値なので、状態系 hook には流用しない。）
/// Claude Code は hook 起動と同時に stdin を書き込むため、通常は 1ms 未満で読み終わる。
///
/// このタイムアウトを変更する場合は `BROKER_CONNECT_TIMEOUT` との合計が hook timeout
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
    if !KNOWN_STATES.contains(&state) {
        // 未知の状態は broker 側でも捨てられるが、ここで弾いて送信自体を避ける。
        eprintln!(
            "siki hook-event: unknown state {:?}; for session registration use `siki session-start`",
            state
        );
        return Ok(());
    }

    let input = read_stdin_with_timeout(HOOK_EVENT_STDIN_TIMEOUT, STDIN_READ_MAX);
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

    let payload = json!({
        "event": state,
        "session_id": session_id,
    })
    .to_string();
    send_line_to_broker(sock_path, payload, state);
    Ok(())
}
