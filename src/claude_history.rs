use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 会話内の1メッセージ
pub struct Message {
    pub role: String,
    pub content: String,
}

/// 1セッション分の完全な会話履歴
pub struct ConversationHistory {
    pub session_id: String,
    pub timestamp: String,
    pub git_branch: String,
    pub messages: Vec<Message>,
}

/// Claude Code の projects ディレクトリパスを返す
fn claude_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude")
        .join("projects")
}

/// ファイルシス��ムパスを Claude Code のプロジェクトディレクトリ名にエンコード
/// 例: /Users/hayashi/foo → -Users-hayashi-foo
fn encode_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut encoded = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '/' {
            encoded.push('-');
        } else {
            encoded.push(ch);
        }
    }
    encoded
}

/// JSONL ファイルからセッションIDを抽出（ファ���ル名の拡張子を除いた部分）
fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// 1つの JSONL ファイルを読み込んで全メッセージを返す
fn parse_jsonl(path: &Path) -> Option<ConversationHistory> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let session_id = session_id_from_path(path);
    let mut messages = Vec::new();
    let mut timestamp: Option<String> = None;
    let mut git_branch: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "user" => {
                let content = extract_user_content(&entry);
                if !content.is_empty() {
                    if timestamp.is_none() {
                        timestamp = entry
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if git_branch.is_none() {
                        git_branch = entry
                            .get("gitBranch")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    messages.push(Message {
                        role: "user".to_string(),
                        content,
                    });
                }
            }
            "assistant" => {
                let text = extract_assistant_text(&entry);
                if !text.is_empty() {
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: text,
                    });
                }
            }
            _ => {}
        }
    }

    if messages.is_empty() {
        return None;
    }

    Some(ConversationHistory {
        session_id,
        timestamp: timestamp.unwrap_or_default(),
        git_branch: git_branch.unwrap_or_default(),
        messages,
    })
}

/// ユーザーメッ��ージから content テキストを抽出
fn extract_user_content(entry: &Value) -> String {
    // message.content が文字列の場合
    if let Some(content) = entry
        .pointer("/message/content")
        .and_then(|v| v.as_str())
    {
        return content.to_string();
    }
    // message.content が配列の場合（text ブロックを結合）
    if let Some(blocks) = entry.pointer("/message/content").and_then(|v| v.as_array()) {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    String::new()
}

/// アシスタントメッセージから text ブロックのみを抽出（tool_use, thinking は除外）
fn extract_assistant_text(entry: &Value) -> String {
    if let Some(blocks) = entry.pointer("/message/content").and_then(|v| v.as_array()) {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        return texts.join("\n");
    }
    String::new()
}

/// 指定された worktree の cwd から、Claude Code の全会話履歴を読み込む
/// `exclude_session_ids` に含まれるセッションはスキップする（サマライズ済み）
pub fn load_worktree_history(
    worktree_cwd: &Path,
    exclude_session_ids: &[String],
) -> Vec<ConversationHistory> {
    let encoded = encode_path(worktree_cwd);
    let project_dir = claude_projects_dir().join(&encoded);

    if !project_dir.is_dir() {
        return Vec::new();
    }

    // JSONL ファ���ルを mtime 昇順でソート（古い順）
    let mut jsonl_files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        jsonl_files.push((path, mtime));
                    }
                }
            }
        }
    }

    // 古い順（時系列順）
    jsonl_files.sort_by(|a, b| a.1.cmp(&b.1));

    jsonl_files
        .into_iter()
        .filter(|(path, _)| {
            let sid = session_id_from_path(path);
            !exclude_session_ids.contains(&sid)
        })
        .filter_map(|(path, _)| parse_jsonl(&path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_encode_path() {
        let path = Path::new("/Users/hayashi/Develop/siki");
        assert_eq!(encode_path(path), "-Users-hayashi-Develop-siki");
    }

    #[test]
    fn test_encode_path_with_dots() {
        let path = Path::new("/Users/hayashi/.siki/workspaces");
        assert_eq!(encode_path(path), "-Users-hayashi-.siki-workspaces");
    }

    #[test]
    fn test_parse_jsonl_full_messages() {
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("test-session.jsonl");
        let mut f = fs::File::create(&jsonl_path).unwrap();

        writeln!(f, r#"{{"type":"user","message":{{"content":"Fix the bug"}},"timestamp":"2026-04-20T10:00:00Z","gitBranch":"main"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"I'll fix that bug for you."}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"content":"Thanks, now add tests"}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Sure, here are the tests."}}]}}}}"#).unwrap();

        let history = parse_jsonl(&jsonl_path).unwrap();
        assert_eq!(history.session_id, "test-session");
        assert_eq!(history.timestamp, "2026-04-20T10:00:00Z");
        assert_eq!(history.git_branch, "main");
        assert_eq!(history.messages.len(), 4);
        assert_eq!(history.messages[0].role, "user");
        assert_eq!(history.messages[0].content, "Fix the bug");
        assert_eq!(history.messages[1].role, "assistant");
        assert_eq!(history.messages[1].content, "I'll fix that bug for you.");
        assert_eq!(history.messages[2].content, "Thanks, now add tests");
        assert_eq!(history.messages[3].content, "Sure, here are the tests.");
    }

    #[test]
    fn test_parse_jsonl_skips_tool_use() {
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("test-session.jsonl");
        let mut f = fs::File::create(&jsonl_path).unwrap();

        writeln!(f, r#"{{"type":"user","message":{{"content":"Hello"}},"timestamp":"2026-04-20T10:00:00Z","gitBranch":"main"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{}}}},{{"type":"text","text":"Done."}}]}}}}"#).unwrap();

        let history = parse_jsonl(&jsonl_path).unwrap();
        assert_eq!(history.messages.len(), 2);
        // tool_use は除外され、text のみ抽出される
        assert_eq!(history.messages[1].content, "Done.");
    }

    #[test]
    fn test_load_worktree_history_empty() {
        let dir = TempDir::new().unwrap();
        let result = load_worktree_history(dir.path(), &[]);
        assert!(result.is_empty());
    }
}
