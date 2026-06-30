// siki 操作系 CLI 型/シグネチャ定義
//
// 作成日: 2026-06-30
// 関連設計: architecture.md / dataflow.md
//
// 注: これは設計用のシグネチャ草案であり、実装ファイルそのものではない。
// 実体は src/cli/{mod.rs,args.rs,prompt.rs} と config.rs への移設で構成する。
//
// 信頼性レベル:
// - 🔵 青信号: 要件定義・既存コード・ユーザヒアリングを参考にした確実な定義
// - 🟡 黄信号: 妥当な推測による定義
// - 🔴 赤信号: 根拠の薄い推測による定義

// ========================================
// src/cli/args.rs — 0依存の小さな引数スキャナ
// ========================================

use std::collections::{HashMap, HashSet};
use anyhow::Result;

/// 位置引数 / 値フラグ / 真偽フラグ / `--` 以降の丸投げ を 1 箇所で仕分けるヘルパ。
/// 🔵 ユーザヒアリング2026-06-30（手書き＋共有ヘルパ）より
pub struct ArgScan {
    positionals: Vec<String>,        // 🔵 project / name 等
    flags: HashSet<String>,          // 🔵 --resume 等の真偽
    values: HashMap<String, String>, // 🔵 --base <ref> 等の値付き
    rest: Vec<String>,               // 🔵 "--" 以降（claude へ丸投げ）
}

impl ArgScan {
    /// `value_keys` に挙げたフラグは「次トークンを値として取る」。
    /// それ以外の `--xxx` は真偽フラグ。`--` 以降は rest。未知の `-x` はエラー。
    /// 🔵
    pub fn parse(args: &[String], value_keys: &[&str]) -> Result<Self> { unimplemented!() }

    /// 位置引数を厳密に n 個取り出す（過不足はエラー）。🔵
    pub fn positionals(&self, n: usize) -> Result<&[String]> { unimplemented!() }

    /// 位置引数を「最大 n 個」取り出す（対話フォールバック用に不足を許容）。🔵
    pub fn positionals_opt(&self, max: usize) -> &[String] { &self.positionals }

    pub fn value(&self, key: &str) -> Option<&str> { self.values.get(key).map(|s| s.as_str()) } // 🔵
    pub fn has(&self, key: &str) -> bool { self.flags.contains(key) }                            // 🔵
    pub fn rest(&self) -> &[String] { &self.rest }                                               // 🔵
}

// ========================================
// src/cli/prompt.rs — crossterm ベースの対話 UI（新依存なし）
// ========================================

/// 矢印キーで 1 件選択（↑↓ 移動 / Enter 決定 / Esc 中止）。
/// 戻り値 None は Esc 中止。raw mode は内部で取得・解除し、端末状態を残さない。
/// 🔵 ユーザヒアリング2026-06-30（矢印キーTUI型）より。crossterm は既存依存。
pub fn select_one(title: &str, items: &[SelectItem]) -> Result<Option<usize>> { unimplemented!() }

/// 1 行テキスト入力（空入力で既定値）。🔵
pub fn input_line(prompt: &str, default: Option<&str>) -> Result<String> { unimplemented!() }

/// y/N 確認（rm の誤削除防止）。🔵
pub fn confirm(prompt: &str, default_yes: bool) -> Result<bool> { unimplemented!() }

/// セレクタの 1 行（ラベル + 補助表示）。例: "tokyo" + "[feature/auth]"。🔵
pub struct SelectItem {
    pub label: String,    // 🔵
    pub note: Option<String>, // 🔵 ブランチ名等の右側補助
}

// ========================================
// src/cli/mod.rs — サブコマンド本体
// ========================================

use std::path::PathBuf;

/// `siki new`：worktree を作成し、作成パスを返す。
/// project/name が不足なら対話で補完（run/new/rm が対話対象）。
/// 🔵 REQ-001/002/403/404 ・ finalize_add_worktree(main.rs:2122) の非TUI部分の再現
pub fn cmd_new(scan: &ArgScan) -> Result<PathBuf> { unimplemented!() }

/// `siki rm`：worktree を削除（対話時は削除確認）。🔵 REQ-005
pub fn cmd_rm(scan: &ArgScan) -> Result<()> { unimplemented!() }

/// `siki path`：worktree の絶対パスを stdout へ。非対話。🔵 REQ-006
pub fn cmd_path(scan: &ArgScan) -> Result<()> { unimplemented!() }

/// `siki list [project]`：プロジェクト/worktree 一覧。非対話。🔵 REQ-007
pub fn cmd_list(scan: &ArgScan) -> Result<()> { unimplemented!() }

/// `siki run`：worktree を解決（無ければ作成）し、LLM を exec で起動する。
/// この関数は正常時は戻らない（プロセスが LLM に置換される）。
/// 🔵 REQ-003/004/401/402 ・ launch_llm_with_args(main.rs:4760) の非TUI再現
pub fn cmd_run(scan: &ArgScan) -> Result<std::convert::Infallible> { unimplemented!() }

// --- 内部共通ヘルパ ---

/// プロジェクト名を discover_projects() から完全一致で解決。
/// project が None なら対話セレクタで選択。未解決時は候補列挙エラー。
/// 🔵 REQ-403 ・ config::discover_projects()(config.rs:468)
fn resolve_project(project: Option<&str>) -> Result<ResolvedProject> { unimplemented!() }

/// worktree を解決。name が None or 存在しないなら、対話で「選択 or ＋新規」。
/// `create_if_missing` が真なら（run/new）不在時に作成。🔵 REQ-004
fn resolve_or_create_worktree(
    proj: &ResolvedProject,
    name: Option<&str>,
    base: Option<&str>,
    create_if_missing: bool,
) -> Result<PathBuf> { unimplemented!() }

/// 解決済みプロジェクト（discover_projects の 1 要素に相当）。🔵
struct ResolvedProject {
    name: String,        // 🔵
    path: PathBuf,       // 🔵 メインリポジトリのパス
    // worktrees は対話セレクタ用に都度取得してもよい
}

// ========================================
// config.rs への移設（既存 main.rs:5330 から）
// ========================================

// pub fn resolve_base_branch(project_path: &std::path::Path, project_name: &str) -> String
//   🔵 main.rs の private 関数を config へ移設し main/cli で共有。
//   siki.json > config.toml > "origin/main" の解決順は不変。

// ========================================
// 既存（再利用・変更なし）
// ========================================
// config::worktree_path(project, name) -> PathBuf                 // 🔵 config.rs:25
// config::load_effective_shared_dirs(project) -> Vec<String>      // 🔵 config.rs:298
// config::load_effective_siki_json(path, project) -> Option<..>   // 🔵 config.rs:706（scripts.setup）
// config::resolve_llm(&Config) -> String                          // 🔵 config.rs:783
// config::discover_projects() -> Vec<ProjectConfig>               // 🔵 config.rs:468
// git::WorktreeManager::create_worktree_from_ref(..)              // 🔵 git.rs:36
// git::WorktreeManager::remove_worktree(project_path, wt_path)    // 🔵 git.rs:190
// hooks::ensure_hooks_configured(wt_path, project)               // 🔵 hooks.rs:9

// ========================================
// 信頼性レベルサマリー
// ========================================
// - 🔵 青信号: 23件 (100%)
// - 🟡 黄信号: 0件
// - 🔴 赤信号: 0件
//
// 品質評価: 高品質（全シグネチャが既存関数 or ヒアリング確定事項に基づく）
