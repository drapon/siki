use serde::Deserialize;
use std::collections::HashMap;
use std::time::Instant;

/// セッションの状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// ツール実行中
    Working,
    /// 許可入力待ち
    Waiting,
    /// 待機中
    Idle,
    /// 放置中（タイムアウト間近）
    Stale,
    /// セッション終了
    Dead,
}

impl SessionState {
    /// DB 保存用の文字列表現を返す
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Stale => "stale",
            Self::Dead => "dead",
        }
    }
}

/// セッション情報
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub worktree_name: String,
    pub project_name: String,
    pub cwd: String,
    pub role: String,
    pub state: SessionState,
    pub last_seen: Instant,
    /// idle 遷移の遅延（working/waiting を最低限の時間表示するため）
    pub idle_pending_since: Option<Instant>,
}

/// Hook から送信されるイベント
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum HookEvent {
    Register {
        session_id: String,
        cwd: String,
        role: String,
    },
    Working {
        session_id: String,
    },
    Waiting {
        session_id: String,
    },
    Idle {
        session_id: String,
    },
    Dead {
        session_id: String,
    },
    Refresh {
        session_id: String,
    },
}

impl HookEvent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Register { session_id, .. }
            | Self::Working { session_id }
            | Self::Waiting { session_id }
            | Self::Idle { session_id }
            | Self::Dead { session_id }
            | Self::Refresh { session_id } => session_id,
        }
    }
}

/// セッションレジストリ
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, Session>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// セッションを登録する
    pub fn register(&mut self, session_id: String, cwd: String, role: String) {
        // cwd からプロジェクト名・worktree名を推定
        // パス形式: ~/.siki/workspaces/<project>/<worktree>/...
        let (project_name, worktree_name) = guess_names_from_cwd(&cwd);

        self.sessions.insert(
            session_id.clone(),
            Session {
                session_id,
                worktree_name,
                project_name,
                cwd,
                role,
                state: SessionState::Idle,
                last_seen: Instant::now(),
                idle_pending_since: None,
            },
        );
    }

    /// セッション状態を更新する
    ///
    /// 描画は WorktreeStatus 側で管理するため、遅延なしで即座に遷移する。
    pub fn update_state(&mut self, session_id: &str, state: SessionState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.last_seen = Instant::now();
            session.idle_pending_since = None;
            session.state = state;
        }
    }

    /// セッションを取得する
    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    /// 全セッションを返す
    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// 指定プロジェクト・worktree名に属するセッション一覧
    pub fn by_worktree(&self, project: &str, worktree: &str) -> Vec<&Session> {
        self.sessions
            .values()
            .filter(|s| s.project_name == project && s.worktree_name == worktree)
            .collect()
    }

    /// idle 遅延の最小表示時間
    /// タイムアウトしたセッションを段階的に劣化させる
    ///
    /// - `stale_timeout`（15秒）を超えたら Stale
    /// - `dead_timeout`（30秒）を超えたら Dead
    pub fn expire_stale_sessions(
        &mut self,
        stale_timeout: std::time::Duration,
        dead_timeout: std::time::Duration,
    ) {
        let now = Instant::now();
        for session in self.sessions.values_mut() {
            let elapsed = now.duration_since(session.last_seen);
            match session.state {
                SessionState::Dead => {}
                _ if elapsed > dead_timeout => {
                    session.state = SessionState::Dead;
                }
                SessionState::Working | SessionState::Waiting | SessionState::Idle
                    if elapsed > stale_timeout =>
                {
                    session.state = SessionState::Stale;
                }
                _ => {}
            }
        }
    }

    /// HookEvent を処理してレジストリを更新する。変更があれば true を返す。
    pub fn handle_event(&mut self, event: HookEvent) -> bool {
        match event {
            HookEvent::Register {
                session_id,
                cwd,
                role,
            } => {
                self.register(session_id, cwd, role);
                true
            }
            HookEvent::Working { session_id } => {
                if self.sessions.contains_key(&session_id) {
                    self.update_state(&session_id, SessionState::Working);
                    true
                } else {
                    false
                }
            }
            HookEvent::Waiting { session_id } => {
                if self.sessions.contains_key(&session_id) {
                    self.update_state(&session_id, SessionState::Waiting);
                    true
                } else {
                    false
                }
            }
            HookEvent::Idle { session_id } => {
                if self.sessions.contains_key(&session_id) {
                    self.update_state(&session_id, SessionState::Idle);
                    true
                } else {
                    false
                }
            }
            HookEvent::Dead { session_id } => {
                if self.sessions.contains_key(&session_id) {
                    self.update_state(&session_id, SessionState::Dead);
                    true
                } else {
                    false
                }
            }
            HookEvent::Refresh { .. } => false,
        }
    }
}

/// cwd パスからプロジェクト名とworktree名を推定する
///
/// 期待パス形式: `~/.siki/workspaces/<project>/<worktree>/...`
pub fn guess_names_from_cwd(cwd: &str) -> (String, String) {
    let workspaces = crate::config::workspaces_dir();
    let workspaces_str = workspaces.to_string_lossy();

    if let Some(rest) = cwd.strip_prefix(workspaces_str.as_ref()) {
        let rest = rest.trim_start_matches('/');
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() >= 2 {
            return (parts[0].to_string(), parts[1].to_string());
        }
    }
    ("unknown".to_string(), "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_state_as_str() {
        assert_eq!(SessionState::Working.as_str(), "working");
        assert_eq!(SessionState::Waiting.as_str(), "waiting");
        assert_eq!(SessionState::Idle.as_str(), "idle");
        assert_eq!(SessionState::Stale.as_str(), "stale");
        assert_eq!(SessionState::Dead.as_str(), "dead");
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut reg = SessionRegistry::new();
        reg.register("sess-1".into(), "/tmp/ws/proj/wt".into(), "frontend".into());
        let s = reg.get("sess-1").unwrap();
        assert_eq!(s.role, "frontend");
        assert_eq!(s.state, SessionState::Idle);
    }

    #[test]
    fn test_registry_update_state() {
        let mut reg = SessionRegistry::new();
        reg.register("sess-1".into(), "/tmp".into(), "default".into());
        reg.update_state("sess-1", SessionState::Working);
        assert_eq!(reg.get("sess-1").unwrap().state, SessionState::Working);
    }

    #[test]
    fn test_registry_update_nonexistent() {
        let mut reg = SessionRegistry::new();
        // 存在しないセッションの更新はパニックしない
        reg.update_state("nonexistent", SessionState::Working);
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_handle_event_register() {
        let mut reg = SessionRegistry::new();
        let changed = reg.handle_event(HookEvent::Register {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            role: "testing".into(),
        });
        assert!(changed);
        assert_eq!(reg.get("s1").unwrap().role, "testing");
    }

    #[test]
    fn test_registry_handle_event_working_unknown() {
        let mut reg = SessionRegistry::new();
        let changed = reg.handle_event(HookEvent::Working {
            session_id: "unknown".into(),
        });
        assert!(!changed);
    }

    #[test]
    fn test_hook_event_deserialize() {
        let json = r#"{"event":"register","session_id":"abc","cwd":"/tmp","role":"default"}"#;
        let ev: HookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.session_id(), "abc");

        let json = r#"{"event":"working","session_id":"abc"}"#;
        let ev: HookEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(ev, HookEvent::Working { .. }));
    }

    #[test]
    fn test_hook_event_session_id() {
        let events = vec![
            HookEvent::Register { session_id: "a".into(), cwd: "/tmp".into(), role: "x".into() },
            HookEvent::Working { session_id: "b".into() },
            HookEvent::Waiting { session_id: "c".into() },
            HookEvent::Idle { session_id: "d".into() },
            HookEvent::Dead { session_id: "e".into() },
            HookEvent::Refresh { session_id: "f".into() },
        ];
        let ids: Vec<&str> = events.iter().map(|e| e.session_id()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn test_hook_event_deserialize_refresh() {
        let json = r#"{"event":"refresh","session_id":"abc"}"#;
        let ev: HookEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(ev, HookEvent::Refresh { .. }));
        assert_eq!(ev.session_id(), "abc");
    }

    #[test]
    fn test_handle_event_refresh_returns_false() {
        let mut reg = SessionRegistry::new();
        let changed = reg.handle_event(HookEvent::Refresh {
            session_id: "test".into(),
        });
        assert!(!changed);
    }
}
