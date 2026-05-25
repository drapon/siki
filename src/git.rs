use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Git worktree の管理
pub struct WorktreeManager;

impl WorktreeManager {
    /// Worktree を作成する
    ///
    /// `worktree_path` に新しい worktree を作成し、
    /// 指定ブランチをチェックアウトする。shared_dirs のシンボリックリンクも設定する。
    pub fn create_worktree(
        project_path: &Path,
        worktree_path: &Path,
        branch: &str,
        shared_dirs: &[String],
    ) -> Result<PathBuf> {
        Self::create_worktree_from_ref(project_path, worktree_path, branch, None, shared_dirs)
    }

    /// start_point を指定して worktree を作成する
    ///
    /// `start_point` が Some の場合、そのリビジョンからブランチを作成する。
    /// None の場合は現在の HEAD からブランチを作成する（従来の動作）。
    pub fn create_worktree_from_ref(
        project_path: &Path,
        worktree_path: &Path,
        branch: &str,
        start_point: Option<&str>,
        shared_dirs: &[String],
    ) -> Result<PathBuf> {
        // 親ディレクトリを作成
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("ディレクトリの作成に失敗: {}", parent.display()))?;
        }

        // リモート追跡ブランチの場合は事前に fetch して最新化
        if let Some(sp) = start_point {
            if sp.contains('/') {
                // "origin/main" → remote="origin"
                if let Some(remote) = sp.split('/').next() {
                    let _ = Command::new("git")
                        .args(["fetch", remote])
                        .current_dir(project_path)
                        .output();
                }
            }
        }

        // git worktree add --no-track -b <branch> <path> [<start_point>]
        //
        // --no-track を付けないと、start_point が remote-tracking branch
        // (origin/main 等) の場合に git が自動で upstream をその remote-tracking
        // branch に設定してしまう (branch.autoSetupMerge=true がデフォルトのため)。
        // siki の worktree は独立した feature ブランチとして扱いたいので、
        // upstream は付けず、初回 push 時にユーザが `git push -u origin HEAD`
        // で同名リモートブランチを張る運用にする。
        let mut args = vec!["worktree", "add", "--no-track", "-b", branch];
        let wt_path_str = worktree_path.to_string_lossy().to_string();
        args.push(&wt_path_str);
        if let Some(sp) = start_point {
            args.push(sp);
        }

        let output = Command::new("git")
            .args(&args)
            .current_dir(project_path)
            .output()
            .context("git worktree add の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // ブランチが既に存在する場合は -b なしで再試行
            if stderr.contains("already exists") {
                let output = Command::new("git")
                    .args(["worktree", "add"])
                    .arg(worktree_path)
                    .arg(branch)
                    .current_dir(project_path)
                    .output()
                    .context("git worktree add の実行に失敗")?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git worktree add に失敗: {}", stderr.trim());
                }
            } else {
                anyhow::bail!("git worktree add に失敗: {}", stderr.trim());
            }
        }

        // shared_dirs のシンボリックリンクを設定
        for dir_name in shared_dirs {
            setup_shared_dir(project_path, worktree_path, dir_name)?;
        }

        Ok(worktree_path.to_path_buf())
    }

    /// リモートブランチ一覧を取得する
    pub fn list_remote_branches(project_path: &Path) -> Result<Vec<String>> {
        // まず fetch して最新状態に同期
        let _ = Command::new("git")
            .args(["fetch", "--prune"])
            .current_dir(project_path)
            .output();

        let output = Command::new("git")
            .args(["branch", "-r", "--no-color"])
            .current_dir(project_path)
            .output()
            .context("git branch -r の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git branch -r に失敗: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let branches: Vec<String> = stdout
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty() && !line.contains("HEAD ->"))
            .collect();

        Ok(branches)
    }

    /// ローカル+リモートブランチの一覧を取得する（ベースブランチ選択用）
    pub fn list_all_branches(project_path: &Path) -> Result<Vec<String>> {
        // fetch して最新状態に同期
        let _ = Command::new("git")
            .args(["fetch", "--prune"])
            .current_dir(project_path)
            .output();

        let output = Command::new("git")
            .args(["branch", "-a", "--no-color"])
            .current_dir(project_path)
            .output()
            .context("git branch -a の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git branch -a に失敗: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let branches: Vec<String> = stdout
            .lines()
            .map(|line| line.trim().trim_start_matches("* ").to_string())
            .filter(|line| !line.is_empty() && !line.contains("HEAD ->"))
            .map(|line| {
                // remotes/origin/xxx → origin/xxx
                line.strip_prefix("remotes/")
                    .unwrap_or(&line)
                    .to_string()
            })
            .collect();

        Ok(branches)
    }

    /// Worktree を削除する
    ///
    /// worktree のディレクトリと git の管理情報を両方クリーンアップする。
    pub fn remove_worktree(
        project_path: &Path,
        worktree_path: &Path,
    ) -> Result<()> {
        // git worktree remove --force <path>
        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .current_dir(project_path)
            .output()
            .context("git worktree remove の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // worktree が存在しない場合は手動でディレクトリ削除を試行
            if worktree_path.exists() {
                std::fs::remove_dir_all(&worktree_path)
                    .with_context(|| {
                        format!("worktree ディレクトリの削除に失敗: {}", worktree_path.display())
                    })?;
            }
            // git worktree prune で管理情報をクリーンアップ
            let _ = Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(project_path)
                .output();

            // ディレクトリ削除が成功していればOK
            if worktree_path.exists() {
                anyhow::bail!("worktree の削除に失敗: {}", stderr.trim());
            }
        }

        Ok(())
    }

    /// リモートを fetch して追跡ブランチを最新化する
    pub fn fetch_remote(project_path: &Path, remote: &str) -> Result<String> {
        let output = Command::new("git")
            .args(["fetch", remote, "--prune"])
            .current_dir(project_path)
            .output()
            .context("git fetch の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git fetch に失敗: {}", stderr.trim());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(stderr)
    }

    /// Worktree の git diff を取得する
    pub fn get_diff(worktree_path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["diff"])
            .current_dir(worktree_path)
            .output()
            .context("git diff の実行に失敗")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff に失敗: {}", stderr.trim());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// 共有ディレクトリのシンボリックリンクを設定する
///
/// worktree 内の対応ディレクトリを削除し、メインリポジトリのディレクトリへの
/// シンボリックリンクを作成する。
pub(crate) fn setup_shared_dir(
    project_path: &Path,
    worktree_path: &Path,
    dir_name: &str,
) -> Result<()> {
    let source = project_path.join(dir_name);
    let target = worktree_path.join(dir_name);

    // ソースが存在しない場合はスキップ
    if !source.exists() {
        return Ok(());
    }

    // worktree 内の対応ディレクトリを削除
    if target.exists() {
        if target.is_symlink() {
            std::fs::remove_file(&target)
                .with_context(|| format!("シンボリックリンクの削除に失敗: {}", target.display()))?;
        } else {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("ディレクトリの削除に失敗: {}", target.display()))?;
        }
    }

    // シンボリックリンクを作成
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, &target)
        .with_context(|| {
            format!(
                "シンボリックリンクの作成に失敗: {} -> {}",
                target.display(),
                source.display()
            )
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// テスト用の git リポジトリを初期化する
    fn init_test_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();

        // 初期コミットを作成（worktree 作成に必要）
        fs::write(path.join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .output()
            .unwrap();

        dir
    }

    #[test]
    fn test_create_worktree() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/feature-test");

        let result = WorktreeManager::create_worktree(
            project_path,
            &wt_dest,
            "feature/test",
            &[],
        );

        assert!(result.is_ok(), "worktree 作成に失敗: {:?}", result.err());
        let wt_path = result.unwrap();
        assert!(wt_path.exists());
        assert_eq!(wt_path, wt_dest);
        // README.md がコピーされている
        assert!(wt_path.join("README.md").exists());
    }

    #[test]
    fn test_create_worktree_with_shared_dirs() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/wt-shared");

        // 共有ディレクトリを作成
        fs::create_dir_all(project_path.join("node_modules/some-package")).unwrap();
        fs::write(
            project_path.join("node_modules/some-package/index.js"),
            "module.exports = {};",
        )
        .unwrap();

        let result = WorktreeManager::create_worktree(
            project_path,
            &wt_dest,
            "shared-test",
            &["node_modules".to_string()],
        );

        assert!(result.is_ok(), "worktree 作成に失敗: {:?}", result.err());
        let wt_path = result.unwrap();

        let link = wt_path.join("node_modules");
        assert!(link.exists(), "シンボリックリンクが存在しない");
        assert!(
            link.is_symlink(),
            "node_modules がシンボリックリンクではない"
        );

        // リンク先が正しいか確認
        let link_target = fs::read_link(&link).unwrap();
        assert_eq!(link_target, project_path.join("node_modules"));
    }

    #[test]
    fn test_create_worktree_shared_dir_not_exists() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/wt-no-shared");

        // nonexistent_dir は存在しない → スキップされるべき
        let result = WorktreeManager::create_worktree(
            project_path,
            &wt_dest,
            "no-shared-test",
            &["nonexistent_dir".to_string()],
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_remove_worktree() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/to-remove");

        // まず作成
        let wt_path = WorktreeManager::create_worktree(
            project_path,
            &wt_dest,
            "remove-test",
            &[],
        )
        .unwrap();
        assert!(wt_path.exists());

        // 削除
        let result = WorktreeManager::remove_worktree(project_path, &wt_dest);
        assert!(result.is_ok(), "worktree 削除に失敗: {:?}", result.err());
        assert!(!wt_path.exists());
    }

    #[test]
    fn test_remove_nonexistent_worktree() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/nonexistent");

        // 存在しない worktree の削除 → エラーにならない（ディレクトリも存在しないため）
        let result = WorktreeManager::remove_worktree(project_path, &wt_dest);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_diff_no_changes() {
        let repo = init_test_repo();
        let diff = WorktreeManager::get_diff(repo.path()).unwrap();
        assert!(diff.is_empty(), "変更がないのに diff が空でない");
    }

    #[test]
    fn test_get_diff_with_changes() {
        let repo = init_test_repo();
        let project_path = repo.path();

        // ファイルを変更
        fs::write(project_path.join("README.md"), "# Modified").unwrap();

        let diff = WorktreeManager::get_diff(project_path).unwrap();
        assert!(!diff.is_empty());
        assert!(diff.contains("-# Test"));
        assert!(diff.contains("+# Modified"));
    }

    #[test]
    fn test_get_diff_nonexistent_path() {
        let result = WorktreeManager::get_diff(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn test_create_and_remove_lifecycle() {
        let repo = init_test_repo();
        let project_path = repo.path();
        let wt_dest = project_path.join("worktrees/lifecycle");

        // 作成
        let wt_path = WorktreeManager::create_worktree(
            project_path,
            &wt_dest,
            "lifecycle-branch",
            &[],
        )
        .unwrap();

        // worktree 内でファイルを変更
        fs::write(wt_path.join("new_file.txt"), "hello").unwrap();
        assert!(wt_path.join("new_file.txt").exists());

        // 削除
        WorktreeManager::remove_worktree(project_path, &wt_dest).unwrap();
        assert!(!wt_path.exists());
    }

    #[test]
    fn test_setup_shared_dir_symlink() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let worktree = dir.path().join("worktree");

        fs::create_dir_all(&project.join("shared")).unwrap();
        fs::write(project.join("shared/file.txt"), "data").unwrap();
        fs::create_dir_all(&worktree.join("shared")).unwrap();
        fs::write(worktree.join("shared/old.txt"), "old").unwrap();

        setup_shared_dir(&project, &worktree, "shared").unwrap();

        let link = worktree.join("shared");
        assert!(link.is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), project.join("shared"));
    }

    #[test]
    fn test_setup_shared_dir_source_not_exists() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let worktree = dir.path().join("worktree");

        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&worktree).unwrap();

        // source が存在しない → スキップ（エラーにならない）
        let result = setup_shared_dir(&project, &worktree, "nonexistent");
        assert!(result.is_ok());
    }
}
