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
    /// 状態バッジ文字を返す
    pub fn badge_char(&self) -> &'static str {
        match self {
            Self::Working => "●",
            Self::Waiting => "●",
            Self::Idle => "○",
            Self::Stale => "◷",
            Self::Dead => "✕",
        }
    }

    /// 状態バッジの色を返す
    pub fn badge_color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            Self::Working => Color::Yellow,
            Self::Waiting => Color::Red,
            Self::Idle => Color::DarkGray,
            Self::Stale => Color::DarkGray,
            Self::Dead => Color::DarkGray,
        }
    }

    /// 集約表示用の優先度（大きいほど優先）
    pub fn priority(&self) -> u8 {
        match self {
            Self::Waiting => 5,
            Self::Working => 4,
            Self::Stale => 3,
            Self::Idle => 2,
            Self::Dead => 1,
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
}

impl HookEvent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Register { session_id, .. }
            | Self::Working { session_id }
            | Self::Waiting { session_id }
            | Self::Idle { session_id }
            | Self::Dead { session_id } => session_id,
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
            },
        );
    }

    /// セッション状態を更新する
    pub fn update_state(&mut self, session_id: &str, state: SessionState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.state = state;
            session.last_seen = Instant::now();
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

    /// worktreeの集約状態（最も優先度の高い状態）を返す
    pub fn aggregate_state(&self, project: &str, worktree: &str) -> Option<SessionState> {
        self.by_worktree(project, worktree)
            .iter()
            .map(|s| s.state)
            .max_by_key(|s| s.priority())
    }

    /// タイムアウトしたセッションを段階的に劣化させる
    ///
    /// - `stale_timeout`（15秒）を超えたら Stale（◷）
    /// - `dead_timeout`（30秒）を超えたら Dead（✕）
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
                SessionState::Stale if elapsed > dead_timeout => {
                    session.state = SessionState::Dead;
                }
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
        }
    }
}

/// cwd パスからプロジェクト名とworktree名を推定する
///
/// 期待パス形式: `~/.siki/workspaces/<project>/<worktree>/...`
fn guess_names_from_cwd(cwd: &str) -> (String, String) {
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
    fn test_session_state_badge_char() {
        assert_eq!(SessionState::Working.badge_char(), "●");
        assert_eq!(SessionState::Waiting.badge_char(), "●");
        assert_eq!(SessionState::Idle.badge_char(), "○");
        assert_eq!(SessionState::Stale.badge_char(), "◷");
        assert_eq!(SessionState::Dead.badge_char(), "✕");
    }

    #[test]
    fn test_session_state_badge_color() {
        use ratatui::style::Color;
        assert_eq!(SessionState::Working.badge_color(), Color::Yellow);
        assert_eq!(SessionState::Waiting.badge_color(), Color::Red);
        assert_eq!(SessionState::Idle.badge_color(), Color::DarkGray);
        assert_eq!(SessionState::Stale.badge_color(), Color::DarkGray);
        assert_eq!(SessionState::Dead.badge_color(), Color::DarkGray);
    }

    #[test]
    fn test_session_state_priority() {
        assert!(SessionState::Waiting.priority() > SessionState::Working.priority());
        assert!(SessionState::Working.priority() > SessionState::Stale.priority());
        assert!(SessionState::Stale.priority() > SessionState::Idle.priority());
        assert!(SessionState::Idle.priority() > SessionState::Dead.priority());
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
    fn test_registry_aggregate_state() {
        let mut reg = SessionRegistry::new();
        let ws = crate::config::workspaces_dir();
        let cwd = format!("{}/myproj/osaka", ws.display());

        reg.register("s1".into(), cwd.clone(), "frontend".into());
        reg.register("s2".into(), cwd, "testing".into());
        reg.update_state("s1", SessionState::Working);
        reg.update_state("s2", SessionState::Waiting);

        // Waiting が最優先
        let agg = reg.aggregate_state("myproj", "osaka").unwrap();
        assert_eq!(agg, SessionState::Waiting);
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
        ];
        let ids: Vec<&str> = events.iter().map(|e| e.session_id()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d", "e"]);
    }
}
