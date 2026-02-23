use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// ~/.siki/ ディレクトリのパスを返す
pub fn siki_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".siki")
}

/// ~/.siki/workspaces/ ディレクトリのパスを返す
pub fn workspaces_dir() -> PathBuf {
    siki_home().join("workspaces")
}

/// ~/.siki/archived/ ディレクトリのパスを返す
pub fn archived_dir() -> PathBuf {
    siki_home().join("archived")
}

/// worktree のパスを返す: ~/.siki/workspaces/<project_name>/<worktree_name>/
pub fn worktree_path(project_name: &str, worktree_name: &str) -> PathBuf {
    workspaces_dir().join(project_name).join(worktree_name)
}

/// 起動時に必要なディレクトリを作成する
pub fn ensure_dirs() -> Result<()> {
    let dirs = [siki_home(), workspaces_dir(), archived_dir()];
    for dir in &dirs {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("ディレクトリの作成に失敗: {}", dir.display()))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    pub siki: SikiConfig,
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SikiConfig {
    pub shell: Option<String>,
    #[serde(default)]
    pub shared_dirs: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct ProjectConfig {
    pub name: String,
    pub path: String,
    pub worktrees: Vec<WorktreeConfig>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct WorktreeConfig {
    pub name: String,
    pub branch: String,
}

/// デフォルトの設定ファイルパスを返す
pub fn default_config_path() -> PathBuf {
    siki_home().join("config.toml")
}

/// 指定パスから設定を読み込む
pub fn load_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("設定ファイルを読み込めません: {}", path.display()))?;
    parse_config(&content)
}

/// TOML 文字列から設定をパースする
pub fn parse_config(content: &str) -> Result<Config> {
    toml::from_str(content).context("設定ファイルのパースに失敗しました")
}

/// ユーザーのデフォルトシェルを検出する
pub fn detect_default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// 設定から有効なシェルを取得（指定があればそれを、なければデフォルトを使用）
pub fn resolve_shell(config: &Config) -> String {
    config
        .siki
        .shell
        .clone()
        .unwrap_or_else(detect_default_shell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_valid_config() {
        let toml = r#"
[siki]
shell = "/usr/local/bin/fish"
shared_dirs = ["node_modules", ".next"]

[[projects]]
name = "webapp"
path = "/home/user/projects/webapp"
worktrees = [
    { name = "feature-auth", branch = "feature/auth" },
    { name = "fix-bug-123", branch = "fix/bug-123" },
]

[[projects]]
name = "api-server"
path = "/home/user/projects/api"
worktrees = [
    { name = "refactor-db", branch = "refactor/db" },
]
"#;
        let config = parse_config(toml).unwrap();

        assert_eq!(
            config.siki.shell,
            Some("/usr/local/bin/fish".to_string())
        );
        assert_eq!(config.siki.shared_dirs, vec!["node_modules", ".next"]);
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].name, "webapp");
        assert_eq!(config.projects[0].worktrees.len(), 2);
        assert_eq!(config.projects[0].worktrees[0].name, "feature-auth");
        assert_eq!(
            config.projects[0].worktrees[0].branch,
            "feature/auth"
        );
        assert_eq!(config.projects[1].name, "api-server");
        assert_eq!(config.projects[1].worktrees.len(), 1);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[siki]

[[projects]]
name = "myproject"
path = "/tmp/myproject"
worktrees = []
"#;
        let config = parse_config(toml).unwrap();

        assert_eq!(config.siki.shell, None);
        assert!(config.siki.shared_dirs.is_empty());
        assert_eq!(config.projects.len(), 1);
    }

    #[test]
    fn test_parse_invalid_toml() {
        let result = parse_config("this is not valid toml [[[");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("パースに失敗"));
    }

    #[test]
    fn test_parse_missing_required_fields() {
        // projects がない
        let toml = r#"
[siki]
shell = "/bin/bash"
"#;
        let result = parse_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_worktree_fields() {
        // worktree に branch がない
        let toml = r#"
[siki]

[[projects]]
name = "test"
path = "/tmp/test"
worktrees = [
    { name = "wt1" },
]
"#;
        let result = parse_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_file_not_found() {
        let result = load_config(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("設定ファイルを読み込めません"));
    }

    #[test]
    fn test_load_config_from_file() {
        let toml = r#"
[siki]
shell = "/bin/zsh"

[[projects]]
name = "test-project"
path = "/tmp/test"
worktrees = [
    { name = "main-work", branch = "main" },
]
"#;
        let mut tmpfile = NamedTempFile::new().unwrap();
        tmpfile.write_all(toml.as_bytes()).unwrap();

        let config = load_config(tmpfile.path()).unwrap();
        assert_eq!(config.siki.shell, Some("/bin/zsh".to_string()));
        assert_eq!(config.projects[0].name, "test-project");
    }

    #[test]
    fn test_resolve_shell_with_explicit_shell() {
        let config = Config {
            siki: SikiConfig {
                shell: Some("/usr/local/bin/fish".to_string()),
                shared_dirs: vec![],
            },
            projects: vec![],
        };
        assert_eq!(resolve_shell(&config), "/usr/local/bin/fish");
    }

    #[test]
    fn test_resolve_shell_without_explicit_shell() {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
            },
            projects: vec![],
        };
        // SHELL 環境変数が設定されていればそれを使用、なければ /bin/sh
        let shell = resolve_shell(&config);
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_detect_default_shell() {
        let shell = detect_default_shell();
        // 何らかのシェルパスが返る
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_siki_home() {
        let home = siki_home();
        assert!(home.ends_with(".siki"));
    }

    #[test]
    fn test_workspaces_dir() {
        let dir = workspaces_dir();
        assert!(dir.ends_with("workspaces"));
        assert!(dir.starts_with(siki_home()));
    }

    #[test]
    fn test_archived_dir() {
        let dir = archived_dir();
        assert!(dir.ends_with("archived"));
        assert!(dir.starts_with(siki_home()));
    }

    #[test]
    fn test_worktree_path() {
        let path = worktree_path("my-project", "feature-auth");
        assert!(path.ends_with("my-project/feature-auth"));
        assert!(path.starts_with(workspaces_dir()));
    }

    #[test]
    fn test_default_config_path_under_siki_home() {
        let path = default_config_path();
        assert!(path.starts_with(siki_home()));
        assert!(path.ends_with("config.toml"));
    }
}
