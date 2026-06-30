//! TUI を起動せずに worktree とセッションを操作する CLI サブコマンド群。
//!
//! 各サブコマンドは TUI 状態に依存せず、`config` / `git` / `hooks` の純粋関数を
//! 薄く呼び出す。`main.rs` のディスパッチから `cli::cmd_*(&args[2..])` で呼ばれる。
//!
//! 引数不足時は run / new / rm が対話セレクタ（[`prompt`]）で補完する 2 モード方式。
//! `path` / `list` はスクリプト連携のため非対話。

pub mod args;
pub mod prompt;

use anyhow::{bail, Result};
use args::ArgScan;
use prompt::SelectItem;
use std::path::{Path, PathBuf};

use crate::{config, git, hooks};

/// 解決済みプロジェクト（`discover_projects` の 1 要素に相当）。
struct ResolvedProject {
    name: String,
    /// メインリポジトリのパス。
    path: PathBuf,
}

impl ResolvedProject {
    fn from_cfg(cfg: &config::ProjectConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            path: PathBuf::from(&cfg.path),
        }
    }
}

/// プロジェクト一覧から名前で完全一致のインデックスを探す（純粋関数）。
/// 見つからない場合は利用可能なプロジェクト名を列挙したエラーを返す。
fn find_project_index(projects: &[config::ProjectConfig], name: &str) -> Result<usize> {
    projects.iter().position(|p| p.name == name).ok_or_else(|| {
        let avail: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let avail = if avail.is_empty() {
            "なし".to_string()
        } else {
            avail.join(", ")
        };
        anyhow::anyhow!("プロジェクトが見つかりません: {} （候補: {}）", name, avail)
    })
}

/// プロジェクトを解決する。`None` の場合は対話セレクタで選択する。
fn resolve_project_cfg(name: Option<&str>) -> Result<config::ProjectConfig> {
    let mut projects = config::discover_projects();
    let idx = match name {
        Some(n) => find_project_index(&projects, n)?,
        None => {
            if projects.is_empty() {
                bail!("プロジェクトが見つかりません（~/.siki/workspaces 配下に git リポジトリがありません）");
            }
            let items: Vec<SelectItem> = projects
                .iter()
                .map(|p| SelectItem::new(p.name.clone(), Some(p.path.clone())))
                .collect();
            prompt::select_one("プロジェクトを選択", &items)?
                .ok_or_else(|| anyhow::anyhow!("中止しました"))?
        }
    };
    Ok(projects.swap_remove(idx))
}

/// 対話で選んだ worktree。
enum WtChoice {
    Existing(String),
    New(String),
}

/// worktree を対話で選ぶ。`allow_new=true` のとき先頭に「＋ 新規作成…」を出す。
fn pick_worktree(cfg: &config::ProjectConfig, allow_new: bool) -> Result<WtChoice> {
    let mut items: Vec<SelectItem> = Vec::new();
    if allow_new {
        items.push(SelectItem::new("＋ 新規作成…", None));
    }
    for w in &cfg.worktrees {
        items.push(SelectItem::new(w.name.clone(), Some(format!("[{}]", w.branch))));
    }
    if items.is_empty() {
        bail!("worktree がありません: {}", cfg.name);
    }

    let idx = prompt::select_one(&format!("worktree を選択  ({})", cfg.name), &items)?
        .ok_or_else(|| anyhow::anyhow!("中止しました"))?;

    if allow_new && idx == 0 {
        let name = prompt::input_line("新しい worktree 名", None)?;
        if name.is_empty() {
            bail!("worktree 名が空です");
        }
        Ok(WtChoice::New(name))
    } else {
        let offset = usize::from(allow_new);
        Ok(WtChoice::Existing(cfg.worktrees[idx - offset].name.clone()))
    }
}

/// base branch を決める。`--base` 指定があればそれ、無ければ対話で入力（既定値あり）。
fn interactive_base(base: Option<&str>, proj: &ResolvedProject) -> Result<String> {
    if let Some(b) = base {
        return Ok(b.to_string());
    }
    let default = config::resolve_base_branch(&proj.path, &proj.name);
    prompt::input_line("base branch", Some(&default))
}

/// worktree を指定パスに作成するテスト可能なコア。
///
/// 既存パスはエラー。base を起点に no_track でブランチを切る（誤 push 防止）。
fn create_worktree_at(
    project_path: &Path,
    wt_path: &Path,
    branch: &str,
    base: &str,
    shared_dirs: &[String],
) -> Result<()> {
    if wt_path.exists() {
        bail!("worktree が既に存在します: {}", wt_path.display());
    }
    git::WorktreeManager::create_worktree_from_ref(
        project_path,
        wt_path,
        branch,
        Some(base),
        true,
        shared_dirs,
    )?;
    Ok(())
}

/// setup スクリプトに渡す `SIKI_*` 環境変数を組み立てる純粋関数（TUI の run_siki_script と同セット）。
/// worktree 名は wt_path の末尾要素から取る。
fn setup_env(proj: &ResolvedProject, wt_path: &Path) -> Vec<(&'static str, String)> {
    let wt_name = wt_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    vec![
        ("SIKI_PROJECT_PATH", proj.path.to_string_lossy().into_owned()),
        ("SIKI_WORKTREE_PATH", wt_path.to_string_lossy().into_owned()),
        ("SIKI_WORKTREE_NAME", wt_name),
    ]
}

/// siki.json の setup スクリプトがあれば worktree dir で実行する（inherited stdio）。
/// 失敗しても worktree 作成自体は成功扱いとし、警告のみ表示する。
/// TUI と同様に `SIKI_*` 環境変数を注入する。エラー文にはスクリプト全文を載せない
/// （setup に直書きされたトークン等が CI ログ/スクロールバックに漏れるのを防ぐ）。
fn run_setup_script(proj: &ResolvedProject, wt_path: &Path) {
    let Some(sj) = config::load_effective_siki_json(&proj.path, &proj.name) else {
        return;
    };
    let Some(setup) = sj.scripts.setup else {
        return;
    };
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&setup)
        .current_dir(wt_path)
        .envs(setup_env(proj, wt_path))
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("setup スクリプトが非ゼロ終了しました（{}）。内容は siki.json を確認してください", s),
        Err(e) => eprintln!("setup スクリプトの実行に失敗しました（{}）。内容は siki.json を確認してください", e),
    }
}

/// worktree 名を検証する。パス区切り・親参照・先頭ハイフン・空文字を弾く。
///
/// CLI は引数経由で `../x` のような値を渡しやすく、`worktree_path` の `join` で
/// workspaces 外を指し得るため、TUI より一歩踏み込んで早期に弾く。
fn validate_worktree_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("worktree 名が空です");
    }
    if name.starts_with('-') {
        bail!("worktree 名はハイフンで始められません: {}", name);
    }
    if name.contains('/') || name.contains('\\') || name.split(std::path::MAIN_SEPARATOR).any(|c| c == "..") || name == ".." {
        bail!("worktree 名にパス区切りや親参照（.. /）は使えません: {}", name);
    }
    // 単一ドット '.' は worktree_path(<proj>, ".") が <proj>/ 自体に解決され、
    // rm の fallback remove_dir_all がプロジェクト配下を一掃しうるため弾く。
    if name == "." {
        bail!("worktree 名にカレント参照（.）は使えません: {}", name);
    }
    Ok(())
}

/// プロジェクトと worktree 名から worktree を作成し、作成パスを返す。
fn create_worktree(proj: &ResolvedProject, name: &str, base: Option<&str>) -> Result<PathBuf> {
    validate_worktree_name(name)?;
    let wt_path = config::worktree_path(&proj.name, name);
    let base = base
        .map(|s| s.to_string())
        .unwrap_or_else(|| config::resolve_base_branch(&proj.path, &proj.name));
    let shared_dirs = config::load_effective_shared_dirs(&proj.name);
    create_worktree_at(&proj.path, &wt_path, name, &base, &shared_dirs)?;
    run_setup_script(proj, &wt_path);
    Ok(wt_path)
}

/// `siki new [project] [name] [--base <ref>]`（不足分は対話補完）
pub fn cmd_new(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &["--base"], &[])?;
    let pos = scan.positionals_opt(2)?;
    let cfg = resolve_project_cfg(pos.first().map(|s| s.as_str()))?;
    let proj = ResolvedProject::from_cfg(&cfg);
    let base = scan.value("--base");

    let path = match pos.get(1).map(|s| s.as_str()) {
        Some(name) => create_worktree(&proj, name, base)?,
        None => {
            // 対話: worktree 名と base を入力。
            let name = prompt::input_line("新しい worktree 名", None)?;
            if name.is_empty() {
                bail!("worktree 名が空です");
            }
            let base = interactive_base(base, &proj)?;
            create_worktree(&proj, &name, Some(&base))?
        }
    };
    println!("{}", path.display());
    Ok(())
}

/// worktree を削除するテスト可能なコア（不在はエラー）。
fn remove_worktree_at(project_path: &Path, wt_path: &Path) -> Result<()> {
    if !wt_path.exists() {
        bail!("worktree が見つかりません: {}", wt_path.display());
    }
    git::WorktreeManager::remove_worktree(project_path, wt_path)?;
    Ok(())
}

/// `siki rm [project] [name] [--yes]`（不足分は対話補完。既定で削除確認、`--yes` で省略）
pub fn cmd_rm(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &[], &["--yes"])?;
    let pos = scan.positionals_opt(2)?;
    let cfg = resolve_project_cfg(pos.first().map(|s| s.as_str()))?;
    let proj = ResolvedProject::from_cfg(&cfg);

    let name = match pos.get(1).map(|s| s.as_str()) {
        Some(n) => n.to_string(),
        None => match pick_worktree(&cfg, false)? {
            WtChoice::Existing(n) => n,
            WtChoice::New(_) => unreachable!("allow_new=false"),
        },
    };
    validate_worktree_name(&name)?;
    let wt_path = config::worktree_path(&proj.name, &name);

    // 既定で削除確認（誤削除防止）。--yes でスキップ。非対話/パイプでは空入力→中止。
    if !scan.has("--yes")
        && !prompt::confirm(&format!("worktree '{}' を削除しますか？", name), false)?
    {
        eprintln!("中止しました");
        return Ok(());
    }

    remove_worktree_at(&proj.path, &wt_path)?;
    println!("worktree を削除しました: {}", wt_path.display());
    Ok(())
}

/// `siki path <project> <name>` — worktree の絶対パスを stdout に出力（非対話）。
pub fn cmd_path(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &[], &[])?;
    let pos = scan.positionals(2)?;
    let cfg = resolve_project_cfg(Some(&pos[0]))?;
    let proj = ResolvedProject::from_cfg(&cfg);
    let name = &pos[1];
    validate_worktree_name(name)?;
    let wt_path = config::worktree_path(&proj.name, name);
    if !wt_path.exists() {
        bail!("worktree が見つかりません: {}", wt_path.display());
    }
    println!("{}", wt_path.display());
    Ok(())
}

/// プロジェクト/worktree 一覧を整形する純粋関数。`filter` 指定時はそのプロジェクトのみ。
fn format_listing(projects: &[config::ProjectConfig], filter: Option<&str>) -> String {
    let mut out = String::new();
    for p in projects {
        if filter.is_some_and(|f| p.name != f) {
            continue;
        }
        out.push_str(&format!("{} ({})\n", p.name, p.path));
        for w in &p.worktrees {
            out.push_str(&format!("  └ {} [{}]\n", w.name, w.branch));
        }
    }
    out
}

/// `siki list [project]` — プロジェクト/worktree 一覧（非対話）。
pub fn cmd_list(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &[], &[])?;
    let pos = scan.positionals_opt(1)?;
    let filter = pos.first().map(|s| s.as_str());

    let projects = config::discover_projects();
    let listing = format_listing(&projects, filter);
    if listing.is_empty() {
        match filter {
            Some(f) => bail!("プロジェクトが見つかりません: {}", f),
            None => println!("No projects found."),
        }
    } else {
        print!("{}", listing);
    }
    Ok(())
}

/// worktree を解決する。不在かつ `create_if_missing` なら作成する。
fn resolve_or_create_worktree(
    proj: &ResolvedProject,
    name: &str,
    base: Option<&str>,
    create_if_missing: bool,
) -> Result<PathBuf> {
    // 既存パスにヒットする経路でも name を検証する（cmd_new との一貫性／workspaces 外参照防止）。
    validate_worktree_name(name)?;
    let wt_path = config::worktree_path(&proj.name, name);
    if wt_path.exists() {
        return Ok(wt_path);
    }
    if create_if_missing {
        create_worktree(proj, name, base)
    } else {
        bail!(
            "worktree が見つかりません: {} （`siki new {} {}` で作成できます）",
            wt_path.display(),
            proj.name,
            name
        )
    }
}

/// `--resume` を有効化してよいか判定する。`-r` は claude 固有の再開フラグなので、
/// claude 以外の LLM で `--resume` を指定された場合はサイレントに壊さずエラーにする。
fn check_resume(resume: bool, llm: &str) -> Result<bool> {
    if resume && llm != "claude" {
        bail!("--resume は claude 専用です（現在の LLM: {}）", llm);
    }
    Ok(resume)
}

/// `siki run` の LLM 起動 argv（プログラム名を除く）を組み立てる純粋関数。
/// `--resume` は claude の再開フラグ `-r` に対応し、`--` 以降の passthrough を続ける。
fn build_run_argv(resume: bool, passthrough: &[String]) -> Vec<String> {
    let mut argv = Vec::new();
    if resume {
        argv.push("-r".to_string());
    }
    argv.extend(passthrough.iter().cloned());
    argv
}

/// 設定からデフォルト LLM を解決する。設定が読めなければ "claude"。
fn resolve_llm() -> String {
    config::load_config(&config::default_config_path())
        .map(|c| config::resolve_llm(&c))
        .unwrap_or_else(|_| "claude".to_string())
}

/// worktree dir で LLM を exec で起動する。
///
/// 正常時はプロセスが LLM に置換されるため**この関数は戻らない**。
/// 戻り値（常に `Err`）が返るのは exec 失敗・hook 解決前の検証エラー時のみ。
fn exec_llm(proj: &ResolvedProject, wt_path: &Path, resume: bool, passthrough: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let llm = resolve_llm();
    let resume = check_resume(resume, &llm)?;

    // hook 注入は claude のときのみ（TUI の launch_llm_with_args と同条件）。
    // 失敗してもセッション起動自体は止めず、警告に留める。
    if llm == "claude" {
        if let Err(e) = hooks::ensure_hooks_configured(wt_path, &proj.name) {
            eprintln!("hook 注入に失敗しました（監視なしで続行）: {}", e);
        }
    }

    let argv = build_run_argv(resume, passthrough);
    // exec はプロセスを置き換えるため、戻ってくるのは失敗時のみ。
    let err = std::process::Command::new(&llm)
        .args(&argv)
        .current_dir(wt_path)
        .exec();
    bail!("{} の起動に失敗しました: {}", llm, err)
}

/// `siki run [project] [name] [--base <ref>] [--resume] [-- <llm args>]`（不足分は対話補完）
pub fn cmd_run(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &["--base"], &["--resume"])?;
    let pos = scan.positionals_opt(2)?;
    let cfg = resolve_project_cfg(pos.first().map(|s| s.as_str()))?;
    let proj = ResolvedProject::from_cfg(&cfg);
    let base = scan.value("--base");

    let wt_path = match pos.get(1).map(|s| s.as_str()) {
        Some(name) => resolve_or_create_worktree(&proj, name, base, true)?,
        None => match pick_worktree(&cfg, true)? {
            // 対話で既存を選んだ場合も、外部で削除済み等に備えて存在を確認する
            // （非対話パスと挙動を揃え、exec の current_dir で不明瞭な OS エラーを出さない）。
            WtChoice::Existing(name) => resolve_or_create_worktree(&proj, &name, base, false)?,
            WtChoice::New(name) => {
                let base = interactive_base(base, &proj)?;
                create_worktree(&proj, &name, Some(&base))?
            }
        },
    };
    exec_llm(&proj, &wt_path, scan.has("--resume"), scan.rest())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "t@t.com"],
            vec!["config", "user.name", "T"],
        ] {
            Command::new("git").args(&args).current_dir(p).output().unwrap();
        }
        std::fs::write(p.join("README.md"), "# t").unwrap();
        Command::new("git").args(["add", "."]).current_dir(p).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(p).output().unwrap();
        dir
    }

    fn proj_cfg(name: &str) -> config::ProjectConfig {
        config::ProjectConfig {
            name: name.to_string(),
            path: "/tmp/x".to_string(),
            display_name: None,
            worktrees: vec![],
        }
    }

    #[test]
    fn create_worktree_at_creates_branch() {
        let repo = init_repo();
        let holder = TempDir::new().unwrap();
        let wt = holder.path().join("kyoto");
        create_worktree_at(repo.path(), &wt, "kyoto", "HEAD", &[]).unwrap();

        let out = Command::new("git")
            .args(["worktree", "list"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(listing.contains("kyoto"), "worktree list に kyoto が無い: {}", listing);
    }

    #[test]
    fn create_worktree_at_errors_when_exists() {
        let repo = init_repo();
        let holder = TempDir::new().unwrap();
        let wt = holder.path().join("dup");
        create_worktree_at(repo.path(), &wt, "dup", "HEAD", &[]).unwrap();
        // 2 回目は既存パスでエラー
        assert!(create_worktree_at(repo.path(), &wt, "dup", "HEAD", &[]).is_err());
    }

    #[test]
    fn find_project_index_matches_and_errors() {
        let projects = vec![proj_cfg("alpha"), proj_cfg("beta")];
        assert_eq!(find_project_index(&projects, "beta").unwrap(), 1);
        assert!(find_project_index(&projects, "gamma").is_err());
    }

    #[test]
    fn remove_worktree_at_removes_and_errors() {
        let repo = init_repo();
        let holder = TempDir::new().unwrap();
        let wt = holder.path().join("osaka");
        create_worktree_at(repo.path(), &wt, "osaka", "HEAD", &[]).unwrap();
        remove_worktree_at(repo.path(), &wt).unwrap();

        let out = Command::new("git")
            .args(["worktree", "list"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(!listing.contains("osaka"), "削除後も worktree が残存: {}", listing);

        // 不在パスの削除はエラー
        assert!(remove_worktree_at(repo.path(), &holder.path().join("none")).is_err());
    }

    #[test]
    fn format_listing_filters_by_project() {
        let mut a = proj_cfg("alpha");
        a.worktrees = vec![config::WorktreeConfig {
            name: "tokyo".to_string(),
            branch: "feature/x".to_string(),
        }];
        let projects = vec![a, proj_cfg("beta")];

        let all = format_listing(&projects, None);
        assert!(all.contains("alpha (/tmp/x)"));
        assert!(all.contains("  └ tokyo [feature/x]"));
        assert!(all.contains("beta"));

        let only = format_listing(&projects, Some("alpha"));
        assert!(only.contains("alpha"));
        assert!(!only.contains("beta"));

        assert!(format_listing(&projects, Some("zzz")).is_empty());
    }

    #[test]
    fn check_resume_claude_only() {
        assert_eq!(check_resume(false, "claude").unwrap(), false);
        assert_eq!(check_resume(true, "claude").unwrap(), true);
        assert_eq!(check_resume(false, "codex").unwrap(), false);
        assert!(check_resume(true, "codex").is_err());
    }

    #[test]
    fn build_run_argv_variants() {
        assert!(build_run_argv(false, &[]).is_empty());
        assert_eq!(build_run_argv(true, &[]), vec!["-r".to_string()]);
        let pt = vec!["--model".to_string(), "opus".to_string()];
        assert_eq!(build_run_argv(false, &pt), pt);
        assert_eq!(
            build_run_argv(true, &pt),
            vec!["-r".to_string(), "--model".to_string(), "opus".to_string()]
        );
    }

    #[test]
    fn validate_worktree_name_rejects_bad_names() {
        assert!(validate_worktree_name("kyoto").is_ok());
        assert!(validate_worktree_name("feature-auth").is_ok());
        assert!(validate_worktree_name("").is_err());
        assert!(validate_worktree_name("..").is_err());
        assert!(validate_worktree_name("../etc").is_err());
        assert!(validate_worktree_name("a/b").is_err());
        assert!(validate_worktree_name("-x").is_err());
        // sec-f1: 単一ドット '.' は worktree_path で <proj>/ に解決され、
        // rm の fallback remove_dir_all がプロジェクト配下を一掃するため必ず弾く。
        assert!(validate_worktree_name(".").is_err());
    }

    #[test]
    fn setup_env_provides_siki_vars() {
        // wt-f1: setup スクリプトに TUI と同じ SIKI_* を渡す
        let proj = ResolvedProject {
            name: "myproj".to_string(),
            path: PathBuf::from("/tmp/myproj"),
        };
        let wt = PathBuf::from("/tmp/myproj-wt/tokyo");
        let env = setup_env(&proj, &wt);
        let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SIKI_PROJECT_PATH"), Some("/tmp/myproj"));
        assert_eq!(get("SIKI_WORKTREE_PATH"), Some("/tmp/myproj-wt/tokyo"));
        assert_eq!(get("SIKI_WORKTREE_NAME"), Some("tokyo"));
    }
}
