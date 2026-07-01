use anyhow::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::app::WorktreeId;
use crate::event::{AppEvent, ClaudeStreamEvent};

/// ClaudeSession のグローバル一意id採番用カウンタ。
/// terminal.rs::NEXT_TERMINAL_ID とは独立（TerminalEmulator と ClaudeSession は無関係な概念）。
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Claude Code CLI セッションの管理
pub struct ClaudeSession {
    id: u64,
    process: Child,
    stdin_writer: ChildStdin,
}

#[cfg(test)]
impl Drop for ClaudeSession {
    fn drop(&mut self) {
        // new_for_test が起動するダミープロセス(sleep 30)がテスト終了後も残存し続けない
        // よう、Drop時に即座にkillする（terminal.rs::TerminalEmulator::Dropと同様の意図）。
        // 本番用 ClaudeSession(実際のclaude CLI)は spawn() 側のライフサイクル管理に
        // 委ねるため、この Drop はテストビルドのみに限定する。
        let _ = self.process.start_kill();
    }
}

impl ClaudeSession {
    /// Claude Code CLI をサブプロセスとして起動する
    ///
    /// worktree ディレクトリで `claude -p --output-format stream-json --input-format stream-json`
    /// を起動し、stdout を非同期で読み取る背景タスクをスポーンする。
    /// `resume_session_id` が指定された場合は `-r <session_id>` で前回のセッションを再開する。
    pub async fn spawn(
        worktree_path: &Path,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        worktree_id: WorktreeId,
        resume_session_id: Option<&str>,
    ) -> Result<Self> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
        ];
        if let Some(session_id) = resume_session_id {
            args.push("-r".to_string());
            args.push(session_id.to_string());
        }
        let mut process = Command::new("claude")
            .args(&args)
            .current_dir(worktree_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Claude Code CLI の起動に失敗")?;

        let stdin_writer = process
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdin の取得に失敗"))?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout の取得に失敗"))?;

        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);

        // stdout 読み取り用の背景タスク
        tokio::spawn(read_stdout_task(stdout, event_tx, worktree_id, id));

        Ok(Self {
            id,
            process,
            stdin_writer,
        })
    }

    /// 生存期間中不変の一意id（TerminalEmulator::id() と同形）
    pub fn id(&self) -> u64 {
        self.id
    }

    /// テスト専用: 実際の `claude` CLI の代わりに軽量なダミープロセスを起動し、
    /// 指定した id を持つ ClaudeSession を構築する。main.rs 側のイベントルーティング
    /// テストで `sessions` に投入するために使う（terminal.rs の spawn_dummy_term と同様のパターン）。
    #[cfg(test)]
    pub fn new_for_test(id: u64) -> Self {
        let mut process = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn dummy process for ClaudeSession test");
        let stdin_writer = process.stdin.take().expect("dummy process stdin");
        Self {
            id,
            process,
            stdin_writer,
        }
    }

    /// Claude Code にメッセージを送信する
    ///
    /// NDJSON 形式で stdin に書き込む。
    pub async fn send_message(&mut self, message: &str) -> Result<()> {
        let json = serde_json::json!({
            "type": "user_message",
            "content": message,
        });
        let mut line = serde_json::to_string(&json)?;
        line.push('\n');
        self.stdin_writer
            .write_all(line.as_bytes())
            .await
            .context("Claude Code への書き込みに失敗")?;
        self.stdin_writer
            .flush()
            .await
            .context("Claude Code stdin のフラッシュに失敗")?;
        Ok(())
    }

    /// Claude Code プロセスを終了する
    pub async fn kill(&mut self) -> Result<()> {
        self.process
            .kill()
            .await
            .context("Claude Code プロセスの終了に失敗")?;
        Ok(())
    }
}

/// stdout を非同期で読み取り、NDJSON をパースしてイベントチャネルに送信する
async fn read_stdout_task(
    stdout: tokio::process::ChildStdout,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    worktree_id: WorktreeId,
    session_id: u64,
) {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(event) = parse_stream_event(&line) {
            let is_result = matches!(event, ClaudeStreamEvent::Result { .. });

            if let ClaudeStreamEvent::Error { ref message } = event {
                let _ = event_tx.send(AppEvent::ClaudeError {
                    worktree_id,
                    session_id,
                    error: message.clone(),
                });
            } else {
                let _ = event_tx.send(AppEvent::ClaudeOutput {
                    worktree_id,
                    session_id,
                    event,
                });
            }

            if is_result {
                let _ = event_tx.send(AppEvent::ClaudeComplete {
                    worktree_id,
                    session_id,
                });
                return;
            }
        }
    }

    // stdout が閉じた = プロセス終了
    let _ = event_tx.send(AppEvent::ClaudeError {
        worktree_id,
        session_id,
        error: "Claude Code プロセスが予期せず終了しました".to_string(),
    });
}

/// NDJSON 行をパースして ClaudeStreamEvent に変換する
///
/// Claude Code CLI の stream-json 出力フォーマットに対応:
/// - `{"type": "system", "subtype": "init", "session_id": "..."}` → Init
/// - `{"type": "content_block_delta", "delta": {"type": "text_delta", "text": "..."}}` → ContentDelta
/// - `{"type": "content_block_start", "content_block": {"type": "tool_use", "name": "...", "input": {...}}}` → ToolUse
/// - `{"type": "assistant", "message": {"content": [...]}}` → ContentDelta / ToolUse
/// - `{"type": "result", ...}` → Result
/// - `{"type": "error", ...}` → Error
pub fn parse_stream_event(line: &str) -> Option<ClaudeStreamEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = value.as_object()?;
    let event_type = obj.get("type")?.as_str()?;

    match event_type {
        "system" => {
            let session_id = obj
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ClaudeStreamEvent::Init { session_id })
        }

        "content_block_delta" => {
            let delta = obj.get("delta")?;
            let text = delta.get("text")?.as_str()?.to_string();
            Some(ClaudeStreamEvent::ContentDelta { text })
        }

        "content_block_start" => {
            let block = obj.get("content_block")?;
            let block_type = block.get("type")?.as_str()?;
            if block_type == "tool_use" {
                let tool = block.get("name")?.as_str()?.to_string();
                let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                Some(ClaudeStreamEvent::ToolUse { tool, input })
            } else {
                None
            }
        }

        "assistant" => {
            let message = obj.get("message")?;
            let content = message.get("content")?.as_array()?;
            for block in content {
                let block_type = block.get("type")?.as_str()?;
                match block_type {
                    "text" => {
                        let text = block.get("text")?.as_str()?.to_string();
                        return Some(ClaudeStreamEvent::ContentDelta { text });
                    }
                    "tool_use" => {
                        let tool = block.get("name")?.as_str()?.to_string();
                        let input =
                            block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        return Some(ClaudeStreamEvent::ToolUse { tool, input });
                    }
                    _ => continue,
                }
            }
            None
        }

        "result" => {
            let text = obj
                .get("result")
                .or_else(|| obj.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ClaudeStreamEvent::Result { text })
        }

        "error" => {
            let message = obj
                .get("error")
                .and_then(|v| {
                    // error が文字列の場合
                    v.as_str().map(|s| s.to_string()).or_else(|| {
                        // error がオブジェクトで message フィールドを持つ場合
                        v.get("message").and_then(|m| m.as_str()).map(|s| s.to_string())
                    })
                })
                .or_else(|| obj.get("message").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| "Unknown error".to_string());
            Some(ClaudeStreamEvent::Error { message })
        }

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ClaudeSession id 採番テスト ---

    #[test]
    fn test_session_id_counter_produces_unique_ids() {
        // spawn() は実際の `claude` CLI を要求するため単体テストでは呼べない。
        // ClaudeSession::spawn が使う採番ロジックそのもの（NEXT_SESSION_ID）を直接検証する。
        let id1 = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let id2 = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(id1, id2, "連続採番されたidは一意でなければならない");
    }

    #[tokio::test]
    async fn test_claude_session_drop_kills_dummy_process() {
        // uradori裏取りで確認済みの回帰: ClaudeSession::new_for_test が起動する
        // ダミープロセス(sleep 30)がDrop時にkillされず、テスト完了後も残存し続けていた。
        let session = ClaudeSession::new_for_test(1);
        let pid = session.process.id().expect("dummy process should have a pid");
        drop(session);

        // Drop 内の kill シグナル送出が反映されるまで少し待つ
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "Drop後もpid={}のダミープロセスが生存している", pid);
    }

    // --- parse_stream_event テスト ---

    #[test]
    fn test_parse_system_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"sess-abc-123","tools":[],"model":"claude-sonnet-4-20250514"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Init { session_id } => {
                assert_eq!(session_id, "sess-abc-123");
            }
            _ => panic!("expected Init event"),
        }
    }

    #[test]
    fn test_parse_system_init_without_session_id() {
        let line = r#"{"type":"system","subtype":"init"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Init { session_id } => {
                assert_eq!(session_id, "");
            }
            _ => panic!("expected Init event"),
        }
    }

    #[test]
    fn test_parse_content_block_delta() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::ContentDelta { text } => {
                assert_eq!(text, "Hello");
            }
            _ => panic!("expected ContentDelta event"),
        }
    }

    #[test]
    fn test_parse_content_block_delta_partial() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::ContentDelta { text } => {
                assert_eq!(text, " world");
            }
            _ => panic!("expected ContentDelta event"),
        }
    }

    #[test]
    fn test_parse_content_block_start_tool_use() {
        let line = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool-123","name":"Read","input":{}}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::ToolUse { tool, input } => {
                assert_eq!(tool, "Read");
                assert_eq!(input, serde_json::json!({}));
            }
            _ => panic!("expected ToolUse event"),
        }
    }

    #[test]
    fn test_parse_content_block_start_text_returns_none() {
        let line = r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        assert!(parse_stream_event(line).is_none());
    }

    #[test]
    fn test_parse_assistant_with_text() {
        let line = r#"{"type":"assistant","message":{"id":"msg-1","type":"message","role":"assistant","content":[{"type":"text","text":"こんにちは"}]}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::ContentDelta { text } => {
                assert_eq!(text, "こんにちは");
            }
            _ => panic!("expected ContentDelta event"),
        }
    }

    #[test]
    fn test_parse_assistant_with_tool_use() {
        let line = r#"{"type":"assistant","message":{"id":"msg-1","type":"message","role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"path":"/tmp/test.rs"}}]}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::ToolUse { tool, input } => {
                assert_eq!(tool, "Edit");
                assert_eq!(input, serde_json::json!({"path": "/tmp/test.rs"}));
            }
            _ => panic!("expected ToolUse event"),
        }
    }

    #[test]
    fn test_parse_result_with_result_field() {
        let line = r#"{"type":"result","subtype":"success","result":"タスクが完了しました","session_id":"sess-123","cost_usd":0.01}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Result { text } => {
                assert_eq!(text, "タスクが完了しました");
            }
            _ => panic!("expected Result event"),
        }
    }

    #[test]
    fn test_parse_result_with_text_field() {
        let line = r#"{"type":"result","text":"done"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Result { text } => {
                assert_eq!(text, "done");
            }
            _ => panic!("expected Result event"),
        }
    }

    #[test]
    fn test_parse_result_empty() {
        let line = r#"{"type":"result","subtype":"success"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Result { text } => {
                assert_eq!(text, "");
            }
            _ => panic!("expected Result event"),
        }
    }

    #[test]
    fn test_parse_error_with_error_string() {
        let line = r#"{"type":"error","error":"API rate limit exceeded"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Error { message } => {
                assert_eq!(message, "API rate limit exceeded");
            }
            _ => panic!("expected Error event"),
        }
    }

    #[test]
    fn test_parse_error_with_error_object() {
        let line = r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Error { message } => {
                assert_eq!(message, "Internal server error");
            }
            _ => panic!("expected Error event"),
        }
    }

    #[test]
    fn test_parse_error_with_message_field() {
        let line = r#"{"type":"error","message":"Something went wrong"}"#;
        let event = parse_stream_event(line).unwrap();
        match event {
            ClaudeStreamEvent::Error { message } => {
                assert_eq!(message, "Something went wrong");
            }
            _ => panic!("expected Error event"),
        }
    }

    #[test]
    fn test_parse_unknown_type_returns_none() {
        let line = r#"{"type":"ping"}"#;
        assert!(parse_stream_event(line).is_none());
    }

    #[test]
    fn test_parse_invalid_json_returns_none() {
        assert!(parse_stream_event("not json").is_none());
        assert!(parse_stream_event("").is_none());
        assert!(parse_stream_event("{invalid").is_none());
    }

    #[test]
    fn test_parse_missing_type_returns_none() {
        let line = r#"{"data":"test"}"#;
        assert!(parse_stream_event(line).is_none());
    }

    #[test]
    fn test_parse_content_block_delta_missing_delta_returns_none() {
        let line = r#"{"type":"content_block_delta","index":0}"#;
        assert!(parse_stream_event(line).is_none());
    }

    // --- send_message のシリアライズテスト ---

    #[test]
    fn test_send_message_format() {
        let message = "Hello, Claude!";
        let json = serde_json::json!({
            "type": "user_message",
            "content": message,
        });
        let serialized = serde_json::to_string(&json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["type"], "user_message");
        assert_eq!(parsed["content"], "Hello, Claude!");
    }

    #[test]
    fn test_send_message_unicode() {
        let message = "日本語のメッセージ 🎉";
        let json = serde_json::json!({
            "type": "user_message",
            "content": message,
        });
        let serialized = serde_json::to_string(&json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["content"], "日本語のメッセージ 🎉");
    }

    // --- read_stdout_task の統合テスト ---

    #[tokio::test]
    async fn test_stdout_reader_sends_events() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let worktree_id: WorktreeId = (0, 0);
        let session_id: u64 = 42;

        // パイプを使ってシミュレーション
        let ndjson = [
            r#"{"type":"system","subtype":"init","session_id":"test-sess"}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"result","result":"done"}"#,
        ]
        .join("\n");

        // tokio::io::duplex でストリームをシミュレート
        let (reader, mut writer) = tokio::io::duplex(4096);

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer.write_all(ndjson.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
            // ストリーム終了
            drop(writer);
        });

        // read_stdout_task に渡すため ChildStdout の代わりに duplex reader を使う
        // ただし read_stdout_task は ChildStdout を要求するため、直接テスト用の関数を使う
        let buf_reader = BufReader::new(reader);
        let mut lines = buf_reader.lines();

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(event) = parse_stream_event(&line) {
                    let is_result = matches!(event, ClaudeStreamEvent::Result { .. });

                    if let ClaudeStreamEvent::Error { ref message } = event {
                        let _ = tx_clone.send(AppEvent::ClaudeError {
                            worktree_id,
                            session_id,
                            error: message.clone(),
                        });
                    } else {
                        let _ = tx_clone.send(AppEvent::ClaudeOutput {
                            worktree_id,
                            session_id,
                            event,
                        });
                    }

                    if is_result {
                        let _ = tx_clone.send(AppEvent::ClaudeComplete {
                            worktree_id,
                            session_id,
                        });
                        return;
                    }
                }
            }
        });

        // イベントを受信して検証（session_id が一貫して乗ることも確認する）
        let event1 = rx.recv().await.unwrap();
        assert!(matches!(
            event1,
            AppEvent::ClaudeOutput {
                event: ClaudeStreamEvent::Init { .. },
                session_id: 42,
                ..
            }
        ));

        let event2 = rx.recv().await.unwrap();
        assert!(matches!(
            event2,
            AppEvent::ClaudeOutput {
                event: ClaudeStreamEvent::ContentDelta { .. },
                session_id: 42,
                ..
            }
        ));

        let event3 = rx.recv().await.unwrap();
        assert!(matches!(
            event3,
            AppEvent::ClaudeOutput {
                event: ClaudeStreamEvent::Result { .. },
                session_id: 42,
                ..
            }
        ));

        let event4 = rx.recv().await.unwrap();
        assert!(matches!(event4, AppEvent::ClaudeComplete { session_id: 42, .. }));
    }

    #[tokio::test]
    async fn test_stdout_reader_handles_error_event() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let worktree_id: WorktreeId = (1, 2);
        let session_id: u64 = 7;

        let ndjson = r#"{"type":"error","error":"API error"}"#;

        let (reader, mut writer) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer.write_all(ndjson.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
            drop(writer);
        });

        let buf_reader = BufReader::new(reader);
        let mut lines = buf_reader.lines();

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(event) = parse_stream_event(&line) {
                    if let ClaudeStreamEvent::Error { ref message } = event {
                        let _ = tx_clone.send(AppEvent::ClaudeError {
                            worktree_id,
                            session_id,
                            error: message.clone(),
                        });
                    } else {
                        let _ = tx_clone.send(AppEvent::ClaudeOutput {
                            worktree_id,
                            session_id,
                            event,
                        });
                    }
                }
            }
        });

        let event = rx.recv().await.unwrap();
        match event {
            AppEvent::ClaudeError {
                worktree_id: wt_id,
                session_id: sid,
                error,
            } => {
                assert_eq!(wt_id, (1, 2));
                assert_eq!(sid, 7);
                assert_eq!(error, "API error");
            }
            _ => panic!("expected ClaudeError event"),
        }
    }
}
