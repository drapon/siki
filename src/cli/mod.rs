//! TUI を起動せずに worktree とセッションを操作する CLI サブコマンド群。
//!
//! 各サブコマンドは TUI 状態に依存せず、`config` / `git` / `hooks` の純粋関数を
//! 薄く呼び出す。`main.rs` のディスパッチから `cli::cmd_*(&args[2..])` で呼ばれる。
//!
//! 引数不足時は run / new / rm が対話セレクタ（[`prompt`]）で補完する 2 モード方式。
//! `path` / `list` はスクリプト連携のため非対話。

// 後続タスク（TASK-0004〜0007）で各コマンドを実装・結線するまでの暫定。
#![allow(dead_code)]

pub mod args;
pub mod prompt;

use anyhow::{bail, Result};
use args::ArgScan;
use std::path::{Path, PathBuf};

use crate::{config, git};

/// 解決済みプロジェクト（`discover_projects` の 1 要素に相当）。
struct ResolvedProject {
    name: String,
    /// メインリポジトリのパス。
    path: PathBuf,
}

/// プロジェクト一覧から名前で完全一致のものを探す（純粋関数）。
/// 見つからない場合は利用可能なプロジェクト名を列挙したエラーを返す。
fn find_project<'a>(
    projects: &'a [config::ProjectConfig],
    name: &str,
) -> Result<&'a config::ProjectConfig> {
    projects.iter().find(|p| p.name == name).ok_or_else(|| {
        let avail: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let avail = if avail.is_empty() {
            "なし".to_string()
        } else {
            avail.join(", ")
        };
        anyhow::anyhow!("プロジェクトが見つかりません: {} （候補: {}）", name, avail)
    })
}

/// プロジェクト名を解決する。`None` の場合は対話補完（TASK-0007 で実装）。
fn resolve_project(name: Option<&str>) -> Result<ResolvedProject> {
    let projects = config::discover_projects();
    match name {
        Some(n) => {
            let p = find_project(&projects, n)?;
            Ok(ResolvedProject {
                name: p.name.clone(),
                path: PathBuf::from(&p.path),
            })
        }
        // TASK-0007 で prompt::select_one による選択に置き換える。
        None => bail!("プロジェクト名を指定してください"),
    }
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

/// siki.json の setup スクリプトがあれば worktree dir で実行する（inherited stdio）。
/// 失敗しても worktree 作成自体は成功扱いとし、警告のみ表示する。
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
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("setup スクリプトが非ゼロ終了しました（{}）: {}", s, setup),
        Err(e) => eprintln!("setup スクリプトの実行に失敗しました（{}）: {}", e, setup),
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

/// `siki new <project> <name> [--base <ref>]`
pub fn cmd_new(args: &[String]) -> Result<()> {
    let scan = ArgScan::parse(args, &["--base"], &[])?;
    let pos = scan.positionals_opt(2)?;
    let project = pos.first().map(|s| s.as_str());
    // TASK-0007 で name 不足時に対話補完する。現状は必須。
    let name = pos
        .get(1)
        .map(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("worktree 名を指定してください"))?;

    let proj = resolve_project(project)?;
    let path = create_worktree(&proj, name, scan.value("--base"))?;
    println!("{}", path.display());
    Ok(())
}

/// `siki rm <project> <name>`
pub fn cmd_rm(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004 / TASK-0007")
}

/// `siki path <project> <name>`
pub fn cmd_path(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004")
}

/// `siki list [project]`
pub fn cmd_list(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004")
}

/// `siki run <project> <name> [--base <ref>] [--resume] [-- <llm args>]`
pub fn cmd_run(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0005 / TASK-0007")
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
    fn find_project_matches_and_errors() {
        let projects = vec![proj_cfg("alpha"), proj_cfg("beta")];
        assert_eq!(find_project(&projects, "beta").unwrap().name, "beta");
        assert!(find_project(&projects, "gamma").is_err());
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
    }
}
