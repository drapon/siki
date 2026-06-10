use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

use crate::session_start::{
    read_stdin_with_timeout, send_line_to_broker, STDIN_READ_MAX, STDIN_READ_TIMEOUT,
};

/// Claude Code が hook 入力 JSON で渡してくる状態イベント。
/// broker 側の `HookEvent`（serde tag = "event"）に対応する。
const KNOWN_STATES: &[&str] = &["working", "waiting", "refresh", "idle", "dead"];

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
        eprintln!("siki hook-event: unknown state {:?}", state);
        return Ok(());
    }

    let input = read_stdin_with_timeout(STDIN_READ_TIMEOUT, STDIN_READ_MAX);
    let input_json: Value = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));

    let session_id = input_json
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // session_id 無しでは更新対象が定まらない。フォールバックで PID 由来の幽霊 ID を
    // 作ると本物のセッションとは別行を更新してしまうため、ここで黙って終了する。
    if session_id.is_empty() {
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
