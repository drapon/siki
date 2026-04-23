use serde::Deserialize;
use std::collections::HashMap;
use std::time::Instant;

/// セッションの状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// ツール実行中（点滅表示）
    Working,
    /// 許可入力待ち
    Waiting,
    /// タスク完了
    Done,
    /// 待機中（セッション登録直後のデフォルト）
    Idle,
}

impl SessionState {
    /// 点滅フェーズに応じたバッジ文字を返す
    pub fn badge_char(&self, blink_on: bool) -> &'static str {
        match self {
            Self::Working => if blink_on { "●" } else { " " },
            Self::Waiting => "●",
            Self::Done => "●",
            Self::Idle => "○",
        }
    }

    /// 状態バッジの色を返す
    pub fn badge_color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            Self::Working => Color::White,
            Self::Waiting => Color::Yellow,
            Self::Done => Color::Green,
            Self::Idle => Color::DarkGray,
        }
    }

    /// 集約表示用の優先度（大きいほど優先）
    pub fn priority(&self) -> u8 {
        match self {
            Self::Waiting => 4,
            Self::Working => 3,
            Self::Done => 2,
            Self::Idle => 1,
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
    /// 人間の確認が必要なアラート（SessionState とは独立）
    pub alert: bool,
    /// アラートの理由メッセージ
    pub alert_message: Option<String>,
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
    ///
    /// 既に同じ session_id が登録されている場合はメタデータのみ更新し、
    /// 現在の state は保持する（レースコンディション対策）。
    pub fn register(&mut self, session_id: String, cwd: String, role: String) {
        // cwd からプロジェクト名・worktree名を推定
        // パス形式: ~/.siki/workspaces/<project>/<worktree>/...
        let (project_name, worktree_name) = guess_names_from_cwd(&cwd);

        if let Some(existing) = self.sessions.get_mut(&session_id) {
            existing.worktree_name = worktree_name;
            existing.project_name = project_name;
            existing.cwd = cwd;
            existing.role = role;
            existing.last_seen = Instant::now();
        } else {
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
                    alert: false,
                    alert_message: None,
                },
            );
        }
    }

    /// セッション状態を更新する
    ///
    /// working/waiting → done の遷移は即座に行わず、idle_pending_since を記録して
    /// Tick で一定時間後に遷移させる（バッジが一瞬で消えるのを防ぐ）。
    /// working/waiting への遷移時は pending をキャンセルする。
    pub fn update_state(&mut self, session_id: &str, state: SessionState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.last_seen = Instant::now();

            match state {
                SessionState::Done => {
                    // working/waiting 中なら即座に done にせず遅延
                    if matches!(session.state, SessionState::Working | SessionState::Waiting) {
                        session.idle_pending_since = Some(Instant::now());
                    } else {
                        session.state = state;
                    }
                }
                SessionState::Working | SessionState::Waiting => {
                    // アクティブ状態への遷移: pending をキャンセル
                    session.idle_pending_since = None;
                    session.state = state;
                }
                _ => {
                    session.idle_pending_since = None;
                    session.state = state;
                }
            }
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
    ///
    /// Idle セッションはバッジ表示対象外のため除外する。
    pub fn aggregate_state(&self, project: &str, worktree: &str) -> Option<SessionState> {
        self.by_worktree(project, worktree)
            .iter()
            .map(|s| s.state)
            .filter(|s| *s != SessionState::Idle)
            .max_by_key(|s| s.priority())
    }

    /// Done 遷移の遅延時間（Working/Waiting を最低限表示するため）
    const DONE_HOLD_DURATION: std::time::Duration = std::time::Duration::from_secs(3);

    /// アクティブ状態（Working/Waiting）のスタル判定タイムアウト
    ///
    /// Working/Waiting 中は PreToolUse/PostToolUse で数秒おきに Refresh が来る。
    /// この時間を超えて Refresh が途絶えた場合、セッションは異常終了したと判断し
    /// Done に遷移させる（Esc や切断で SessionEnd hook が発火しないケースの対策）。
    const ACTIVE_STALE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// 期限切れセッションをクリーンアップする
    ///
    /// - Working/Waiting で last_seen から 30秒経過 → Done に遷移（スタル検知）
    /// - idle_pending_since から 3秒経過 → Done に遷移
    /// - `expire_timeout`（5分）を超えたセッションをレジストリから削除
    pub fn cleanup_expired_sessions(&mut self, expire_timeout: std::time::Duration) {
        let now = Instant::now();

        for session in self.sessions.values_mut() {
            // アクティブ状態のスタル検知: 30秒 Refresh が来なければ Done に遷移
            if matches!(session.state, SessionState::Working | SessionState::Waiting)
                && session.idle_pending_since.is_none()
                && now.duration_since(session.last_seen) >= Self::ACTIVE_STALE_TIMEOUT
            {
                session.state = SessionState::Done;
            }

            // Done 遅延の解消: 3秒経過したら実際に Done に遷移
            if let Some(pending_since) = session.idle_pending_since {
                if now.duration_since(pending_since) >= Self::DONE_HOLD_DURATION {
                    session.state = SessionState::Done;
                    session.idle_pending_since = None;
                }
            }
        }

        // 一定時間応答なしのセッションを削除
        self.sessions
            .retain(|_, session| now.duration_since(session.last_seen) <= expire_timeout);
    }

    /// セッションのアラートを設定/解除する
    pub fn set_alert(&mut self, session_id: &str, alert: bool, message: Option<String>) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.alert = alert;
            session.alert_message = if alert { message } else { None };
        }
    }

    /// 指定 worktree にアラートがあるセッションが存在するか
    pub fn has_alert(&self, project: &str, worktree: &str) -> bool {
        self.by_worktree(project, worktree)
            .iter()
            .any(|s| s.alert)
    }

    /// 指定 worktree のアラートをクリアする
    pub fn clear_alerts(&mut self, project: &str, worktree: &str) {
        for session in self.sessions.values_mut() {
            if session.project_name == project
                && session.worktree_name == worktree
                && session.alert
            {
                session.alert = false;
                session.alert_message = None;
            }
        }
    }

    /// 指定 worktree の Done セッションを Idle にリセットする（既読クリア）
    ///
    /// セッションを削除するとメタデータ（cwd, project_name, worktree_name）が失われ、
    /// 同じセッションが再度 Working になった際に自動登録で "unknown" に紐づいてしまう。
    /// 状態のみリセットすることでメタデータを保持する。
    pub fn clear_done_sessions(&mut self, project: &str, worktree: &str) {
        for session in self.sessions.values_mut() {
            if session.project_name == project
                && session.worktree_name == worktree
                && session.state == SessionState::Done
            {
                session.state = SessionState::Idle;
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
                // 未登録セッションは自動登録（レースコンディション対策）
                if !self.sessions.contains_key(&session_id) {
                    self.register(session_id.clone(), String::new(), "default".into());
                }
                self.update_state(&session_id, SessionState::Working);
                true
            }
            HookEvent::Waiting { session_id } => {
                if !self.sessions.contains_key(&session_id) {
                    self.register(session_id.clone(), String::new(), "default".into());
                }
                self.update_state(&session_id, SessionState::Waiting);
                true
            }
            HookEvent::Idle { session_id } => {
                // Stop イベント → Done 状態に遷移（タスク完了を示す）
                if !self.sessions.contains_key(&session_id) {
                    self.register(session_id.clone(), String::new(), "default".into());
                }
                self.update_state(&session_id, SessionState::Done);
                true
            }
            HookEvent::Dead { session_id } => {
                // SessionEnd → セッションを削除
                self.sessions.remove(&session_id);
                true
            }
            HookEvent::Refresh { session_id } => {
                // PostToolUse → last_seen を更新（heartbeat）
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.last_seen = Instant::now();
                }
                false
            }
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
    fn test_session_state_badge_char() {
        assert_eq!(SessionState::Working.badge_char(true), "●");
        assert_eq!(SessionState::Working.badge_char(false), " ");
        assert_eq!(SessionState::Waiting.badge_char(true), "●");
        assert_eq!(SessionState::Waiting.badge_char(false), "●");
        assert_eq!(SessionState::Done.badge_char(true), "●");
        assert_eq!(SessionState::Idle.badge_char(true), "○");
    }

    #[test]
    fn test_session_state_badge_color() {
        use ratatui::style::Color;
        assert_eq!(SessionState::Working.badge_color(), Color::White);
        assert_eq!(SessionState::Waiting.badge_color(), Color::Yellow);
        assert_eq!(SessionState::Done.badge_color(), Color::Green);
        assert_eq!(SessionState::Idle.badge_color(), Color::DarkGray);
    }

    #[test]
    fn test_session_state_priority() {
        assert!(SessionState::Waiting.priority() > SessionState::Working.priority());
        assert!(SessionState::Working.priority() > SessionState::Done.priority());
        assert!(SessionState::Done.priority() > SessionState::Idle.priority());
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
    fn test_registry_register_preserves_state() {
        let mut reg = SessionRegistry::new();
        reg.register("sess-1".into(), "/tmp".into(), "default".into());
        reg.update_state("sess-1", SessionState::Working);
        // 再登録でメタデータは更新されるが state は保持
        reg.register("sess-1".into(), "/new/path".into(), "frontend".into());
        let s = reg.get("sess-1").unwrap();
        assert_eq!(s.state, SessionState::Working);
        assert_eq!(s.role, "frontend");
        assert_eq!(s.cwd, "/new/path");
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
    fn test_registry_handle_event_working_auto_registers() {
        let mut reg = SessionRegistry::new();
        // 未登録セッションでも自動登録される
        let changed = reg.handle_event(HookEvent::Working {
            session_id: "unknown".into(),
        });
        assert!(changed);
        assert_eq!(reg.get("unknown").unwrap().state, SessionState::Working);
    }

    #[test]
    fn test_registry_handle_event_idle_becomes_done() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.update_state("s1", SessionState::Working);
        // Idle イベント（Stop hook）は Done に遷移
        // ただし working→done は遅延されるので idle_pending_since が設定される
        reg.handle_event(HookEvent::Idle { session_id: "s1".into() });
        let s = reg.get("s1").unwrap();
        // 遅延中は Working のまま
        assert_eq!(s.state, SessionState::Working);
        assert!(s.idle_pending_since.is_some());
    }

    #[test]
    fn test_registry_handle_event_dead_removes_session() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.handle_event(HookEvent::Dead { session_id: "s1".into() });
        // Dead イベント（SessionEnd hook）はセッションを削除
        assert!(reg.get("s1").is_none());
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
    fn test_handle_event_refresh_updates_last_seen() {
        let mut reg = SessionRegistry::new();
        reg.register("test".into(), "/tmp".into(), "default".into());
        let before = reg.get("test").unwrap().last_seen;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let changed = reg.handle_event(HookEvent::Refresh {
            session_id: "test".into(),
        });
        assert!(!changed); // 状態変更なし
        assert!(reg.get("test").unwrap().last_seen > before); // last_seen は更新
    }

    #[test]
    fn test_cleanup_expired_sessions() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        // 即座にはクリーンアップされない
        reg.cleanup_expired_sessions(std::time::Duration::from_secs(300));
        assert!(reg.get("s1").is_some());
    }

    #[test]
    fn test_cleanup_stale_working_session() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.update_state("s1", SessionState::Working);

        // last_seen を 31秒前に巻き戻す
        reg.sessions.get_mut("s1").unwrap().last_seen =
            Instant::now() - std::time::Duration::from_secs(31);

        reg.cleanup_expired_sessions(std::time::Duration::from_secs(300));
        // Working → Done に遷移しているはず
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Done);
    }

    #[test]
    fn test_cleanup_stale_waiting_session() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.update_state("s1", SessionState::Waiting);

        reg.sessions.get_mut("s1").unwrap().last_seen =
            Instant::now() - std::time::Duration::from_secs(31);

        reg.cleanup_expired_sessions(std::time::Duration::from_secs(300));
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Done);
    }

    #[test]
    fn test_cleanup_active_session_not_stale_within_timeout() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.update_state("s1", SessionState::Working);

        // last_seen を 10秒前に巻き戻す（30秒以内なのでスタルにならない）
        reg.sessions.get_mut("s1").unwrap().last_seen =
            Instant::now() - std::time::Duration::from_secs(10);

        reg.cleanup_expired_sessions(std::time::Duration::from_secs(300));
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Working);
    }

    #[test]
    fn test_stale_session_recovers_on_new_event() {
        let mut reg = SessionRegistry::new();
        reg.register("s1".into(), "/tmp".into(), "default".into());
        reg.update_state("s1", SessionState::Working);

        // スタルにする
        reg.sessions.get_mut("s1").unwrap().last_seen =
            Instant::now() - std::time::Duration::from_secs(31);
        reg.cleanup_expired_sessions(std::time::Duration::from_secs(300));
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Done);

        // 再開: Working イベントが来たら復帰する
        reg.handle_event(HookEvent::Working { session_id: "s1".into() });
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Working);
    }

    /// テスト用: 指定 project/worktree のセッションを直接挿入する
    fn insert_session(
        reg: &mut SessionRegistry,
        id: &str,
        project: &str,
        worktree: &str,
        state: SessionState,
    ) {
        reg.sessions.insert(
            id.to_string(),
            Session {
                session_id: id.to_string(),
                worktree_name: worktree.to_string(),
                project_name: project.to_string(),
                cwd: String::new(),
                role: "default".to_string(),
                state,
                last_seen: Instant::now(),
                idle_pending_since: None,
                alert: false,
                alert_message: None,
            },
        );
    }

    #[test]
    fn test_clear_done_sessions_resets_only_done_to_idle() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Done);
        insert_session(&mut reg, "s2", "proj", "wt1", SessionState::Working);
        insert_session(&mut reg, "s3", "proj", "wt2", SessionState::Done);

        reg.clear_done_sessions("proj", "wt1");

        // s1 (Done, wt1) は Idle にリセットされる
        assert_eq!(reg.get("s1").unwrap().state, SessionState::Idle);
        // s2 (Working, wt1) は変わらない
        assert_eq!(reg.get("s2").unwrap().state, SessionState::Working);
        // s3 (Done, wt2) は別 worktree なので変わらない
        assert_eq!(reg.get("s3").unwrap().state, SessionState::Done);
    }

    #[test]
    fn test_aggregate_state_excludes_idle() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Idle);

        // Idle のみの場合は None
        assert!(reg.aggregate_state("proj", "wt1").is_none());

        // Working を追加すると Working が返る
        insert_session(&mut reg, "s2", "proj", "wt1", SessionState::Working);
        assert_eq!(reg.aggregate_state("proj", "wt1").unwrap(), SessionState::Working);
    }

    #[test]
    fn test_clear_done_sessions_no_done() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Working);
        insert_session(&mut reg, "s2", "proj", "wt1", SessionState::Idle);

        reg.clear_done_sessions("proj", "wt1");

        // 何も削除されない
        assert!(reg.get("s1").is_some());
        assert!(reg.get("s2").is_some());
    }

    #[test]
    fn test_set_alert() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Working);

        reg.set_alert("s1", true, Some("CI failed".into()));
        let s = reg.get("s1").unwrap();
        assert!(s.alert);
        assert_eq!(s.alert_message.as_deref(), Some("CI failed"));

        // 解除
        reg.set_alert("s1", false, None);
        let s = reg.get("s1").unwrap();
        assert!(!s.alert);
        assert!(s.alert_message.is_none());
    }

    #[test]
    fn test_set_alert_nonexistent() {
        let mut reg = SessionRegistry::new();
        // パニックしない
        reg.set_alert("nonexistent", true, Some("test".into()));
    }

    #[test]
    fn test_has_alert() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Working);
        insert_session(&mut reg, "s2", "proj", "wt1", SessionState::Idle);

        assert!(!reg.has_alert("proj", "wt1"));

        reg.set_alert("s1", true, Some("CI failed".into()));
        assert!(reg.has_alert("proj", "wt1"));

        // 別 worktree には影響しない
        assert!(!reg.has_alert("proj", "wt2"));
    }

    #[test]
    fn test_clear_alerts() {
        let mut reg = SessionRegistry::new();
        insert_session(&mut reg, "s1", "proj", "wt1", SessionState::Working);
        insert_session(&mut reg, "s2", "proj", "wt1", SessionState::Idle);
        insert_session(&mut reg, "s3", "proj", "wt2", SessionState::Working);

        reg.set_alert("s1", true, Some("CI failed".into()));
        reg.set_alert("s3", true, Some("test failed".into()));

        reg.clear_alerts("proj", "wt1");

        // wt1 のアラートはクリア
        assert!(!reg.get("s1").unwrap().alert);
        assert!(reg.get("s1").unwrap().alert_message.is_none());
        // wt2 は影響なし
        assert!(reg.get("s3").unwrap().alert);
    }
}
