-- ========================================
-- エージェント監視システム データベーススキーマ（SQLite）
-- ========================================
--
-- 作成日: 2026-06-23
-- 関連設計: architecture.md / interfaces.rs
-- 対象: ~/.siki/siki.db（rusqlite, bundled SQLite, WAL モード）
--
-- 信頼性レベル:
-- - 🔵 青信号: EARS要件定義書・設計文書・既存DBスキーマを参考にした確実な定義
-- - 🟡 黄信号: 上記から妥当な推測による定義
-- - 🔴 赤信号: 上記にない推測による定義
--
-- 注意: 本機能で必要なスキーマ変更は sessions テーブルへの activity 列追加の
--       「1点のみ」。新規テーブル・新規インデックスは追加しない（YAGNI）。
--       UI はインメモリ SessionRegistry をライブ参照するため、activity 列は
--       主に永続化（再起動耐性）と将来の MCP 連携余地のために置く。

-- ========================================
-- マイグレーション（src/db.rs の init 内・既存パターン踏襲）
-- ========================================

-- sessions テーブルに activity 列を追加する。
-- 🔵 信頼性: REQ-402 + 既存 `db.rs:49-60` の冪等 ALTER パターン
--   （claude_session_id / alert / alert_message と同じ流儀）
-- 実装イメージ（Rust）:
--   let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN activity TEXT;");
--   ※ 既に列が存在する再起動時はエラーを握りつぶす（冪等）
ALTER TABLE sessions ADD COLUMN activity TEXT; -- 🔵 REQ-001/402: 現在/直前のツール活動（NULL 許容）

-- ========================================
-- 変更後の sessions テーブル定義（参考: 全列）
-- ========================================
-- 🔵 信頼性: 既存 `db.rs:21-32` + 本機能の activity 追加
--
-- CREATE TABLE IF NOT EXISTS sessions (
--     session_id TEXT PRIMARY KEY,                          -- 🔵 既存
--     role TEXT NOT NULL DEFAULT 'default',                 -- 🔵 既存
--     worktree_name TEXT NOT NULL DEFAULT 'unknown',        -- 🔵 既存
--     project_name TEXT NOT NULL DEFAULT 'unknown',         -- 🔵 既存
--     cwd TEXT NOT NULL DEFAULT '',                         -- 🔵 既存
--     state TEXT NOT NULL DEFAULT 'idle',                   -- 🔵 既存（idle|working|waiting|dead）
--     summary TEXT,                                         -- 🔵 既存（set_summary 由来・REQ-005 で表示）
--     claude_session_id TEXT,                               -- 🔵 既存（マイグレーションで追加済）
--     alert INTEGER NOT NULL DEFAULT 0,                     -- 🔵 既存（REQ-104 で赤強調）
--     alert_message TEXT,                                   -- 🔵 既存
--     activity TEXT,                                        -- 🟢 本機能で追加（REQ-001）
--     last_heartbeat INTEGER NOT NULL,                      -- 🔵 既存（REQ-005 経過時間の永続側）
--     created_at INTEGER NOT NULL                           -- 🔵 既存
-- );

-- ========================================
-- 更新クエリ（src/db.rs に新設する関数）
-- ========================================

-- working（PreToolUse）受信時に broker から呼ぶ activity 更新。
-- 🔵 信頼性: 既存 `update_session_state` / `update_session_summary` に準拠
-- fn update_session_activity(conn, session_id, activity):
UPDATE sessions SET activity = ?1 WHERE session_id = ?2; -- 🔵 REQ-102

-- 既存の状態更新（参考・変更なし）:
-- UPDATE sessions SET state = ?1, last_heartbeat = ?2 WHERE session_id = ?3; -- 🔵 既存

-- ========================================
-- インデックス
-- ========================================
-- 🔵 信頼性: 既存設計より
-- 追加インデックスは不要。
--   - UI は SessionRegistry（インメモリ HashMap）を参照するため DB 検索は走らない。
--   - sessions の主キー（session_id）と既存アクセスパターンで十分。
--   （NFR-002 のとおり描画は O(セッション数) のメモリ走査で完結する）

-- ========================================
-- トリガー / 初期データ
-- ========================================
-- 🔵 不要。
--   - updated_at 相当は既存の last_heartbeat をアプリ側で更新しており、SQLite
--     トリガーは未使用（既存方針を踏襲）。
--   - マスターデータ・初期投入なし。

-- ========================================
-- 後方互換・マイグレーション安全性
-- ========================================
-- 🔵 信頼性: REQ-401/402
--   - activity は NULL 許容。旧バージョンが作成した既存行は activity=NULL のまま
--     維持され、UI 側は None として「-」表示する（破壊的変更なし）。
--   - ALTER 文は冪等運用（再起動で列が既存でもエラーを無視）。

-- ========================================
-- 信頼性レベルサマリー
-- ========================================
-- - 🔵 青信号: 12件 (92%)
-- - 🟡 黄信号: 1件 (8%)   ※活動列の運用想定（再起動復元の位置づけ）
-- - 🔴 赤信号: 0件 (0%)
--
-- 品質評価: 高品質（変更は activity 列1点。既存マイグレーション/アクセスパターンに完全準拠）
