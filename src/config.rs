use anyhow::{Context, Result};
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct Config {
    pub siki: SikiConfig,
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SikiConfig {
    pub shell: Option<String>,
    /// デフォルトの LLM CLI コマンド（例: "claude", "aider", "codex"）
    #[serde(default)]
    pub llm: Option<String>,
    /// 利用可能な LLM CLI コマンドの一覧（Ctrl+t ピッカーで表示）
    #[serde(default)]
    pub llms: Vec<String>,
    #[serde(default)]
    pub shared_dirs: Vec<String>,
    /// グローバルデフォルトのベースブランチ（例: "origin/main"）
    #[serde(default)]
    pub base_branch: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct ProjectConfig {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub worktrees: Vec<WorktreeConfig>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct WorktreeConfig {
    pub name: String,
    pub branch: String,
}

/// デフォルトの設定ファイルパスを返す
pub fn default_config_path() -> PathBuf {
    siki_home().join("config.toml")
}

/// 指定パスから設定を読み込む（ファイルが存在しない場合はデフォルト設定を返す）
pub fn load_config(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_config(&content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config {
            siki: SikiConfig {
                shell: None,
                llm: None,
                llms: vec![],
                shared_dirs: vec![],
                base_branch: None,
            },
            projects: vec![],
        }),
        Err(e) => Err(anyhow::anyhow!("設定ファイルを読み込めません: {}: {}", path.display(), e)),
    }
}

/// TOML 文字列から設定をパースする
pub fn parse_config(content: &str) -> Result<Config> {
    toml::from_str(content).context("設定ファイルのパースに失敗しました")
}

/// 設定をファイルに保存する
pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    let content =
        toml::to_string_pretty(config).context("設定のシリアライズに失敗しました")?;
    std::fs::write(path, content)
        .with_context(|| format!("設定ファイルの保存に失敗: {}", path.display()))?;
    Ok(())
}

const CITY_NAMES: &[&str] = &[
    "amsterdam",
    "athens",
    "baghdad",
    "bangkok",
    "barcelona",
    "beijing",
    "belgrade",
    "berlin",
    "bogota",
    "bratislava",
    "brussels",
    "bucharest",
    "budapest",
    "buenos-aires",
    "cairo",
    "cape-town",
    "caracas",
    "chicago",
    "copenhagen",
    "dakar",
    "delhi",
    "dhaka",
    "dublin",
    "durban",
    "edinburgh",
    "florence",
    "geneva",
    "hanoi",
    "havana",
    "helsinki",
    "ho-chi-minh",
    "hong-kong",
    "istanbul",
    "jakarta",
    "jerusalem",
    "johannesburg",
    "karachi",
    "kathmandu",
    "kuala-lumpur",
    "kyoto",
    "lagos",
    "lima",
    "lisbon",
    "london",
    "madrid",
    "manila",
    "marrakech",
    "melbourne",
    "mexico-city",
    "milan",
    "montreal",
    "moscow",
    "mumbai",
    "munich",
    "nairobi",
    "new-york",
    "osaka",
    "oslo",
    "ottawa",
    "paris",
    "prague",
    "quito",
    "reykjavik",
    "rio-de-janeiro",
    "rome",
    "santiago",
    "sao-paulo",
    "sarajevo",
    "seoul",
    "shanghai",
    "singapore",
    "stockholm",
    "sydney",
    "taipei",
    "tallinn",
    "tehran",
    "tokyo",
    "toronto",
    "vancouver",
    "vienna",
    "warsaw",
    "washington",
    "wellington",
    "zurich",
];

/// 既存の worktree 名と重複しないランダムな都市名を生成する
pub fn generate_worktree_name(existing_names: &[String]) -> String {
    let mut rng = rand::rng();
    let available: Vec<&str> = CITY_NAMES
        .iter()
        .copied()
        .filter(|name| !existing_names.iter().any(|e| e == name))
        .collect();

    if let Some(&name) = available.choose(&mut rng) {
        name.to_string()
    } else {
        // 全都市名が使用済みの場合はサフィックス付きで生成
        let base = CITY_NAMES.choose(&mut rng).unwrap_or(&"worktree");
        let suffix: u16 = rand::random_range(100..999);
        format!("{}-{}", base, suffix)
    }
}

/// プロジェクトメタデータ (~/.siki/workspaces/<project>/project.json)
#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectMeta {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// プロジェクト個別のスクリプト設定（siki.json が無い場合に使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<SikiScripts>,
    /// プロジェクト個別のベースブランチ（siki.json が無い場合に使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// プロジェクト個別の共有ディレクトリ（worktree にシンボリックリンクする）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_dirs: Option<Vec<String>>,
    /// worktree ごとのメタデータ
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub worktrees: HashMap<String, WorktreeMeta>,
}

/// プロジェクトの project.json パスを返す
pub fn project_meta_path(project_name: &str) -> PathBuf {
    workspaces_dir().join(project_name).join("project.json")
}

/// project.json を保存する
pub fn save_project_meta(project_name: &str, source_path: &Path) -> Result<()> {
    // 既存のフィールドを保持する
    let existing = load_project_meta(project_name);
    let meta = ProjectMeta {
        path: source_path.to_string_lossy().to_string(),
        display_name: existing.as_ref().and_then(|m| m.display_name.clone()),
        scripts: existing.as_ref().and_then(|m| m.scripts.clone()),
        base_branch: existing.as_ref().and_then(|m| m.base_branch.clone()),
        shared_dirs: existing.as_ref().and_then(|m| m.shared_dirs.clone()),
        worktrees: existing.map(|m| m.worktrees).unwrap_or_default(),
    };
    let dir = workspaces_dir().join(project_name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("プロジェクトディレクトリの作成に失敗: {}", dir.display()))?;
    let content =
        serde_json::to_string_pretty(&meta).context("project.json のシリアライズに失敗")?;
    let path = project_meta_path(project_name);
    std::fs::write(&path, content)
        .with_context(|| format!("project.json の保存に失敗: {}", path.display()))?;
    Ok(())
}

/// project.json の display_name のみを更新する
pub fn save_project_display_name(project_name: &str, display_name: Option<&str>) -> Result<()> {
    let meta_path = project_meta_path(project_name);
    let mut meta = load_project_meta(project_name)
        .ok_or_else(|| anyhow::anyhow!("project.json が見つかりません: {}", project_name))?;
    // 表示名は通知の OSC シーケンスにも流れ込むため、保存前にサニタイズする。
    meta.display_name = display_name
        .map(crate::text::sanitize_text)
        .filter(|s| !s.is_empty());
    let content =
        serde_json::to_string_pretty(&meta).context("project.json のシリアライズに失敗")?;
    std::fs::write(&meta_path, content)
        .with_context(|| format!("project.json の保存に失敗: {}", meta_path.display()))?;
    Ok(())
}

/// project.json の shared_dirs のみを更新する
pub fn save_project_shared_dirs(project_name: &str, shared_dirs: Vec<String>) -> Result<()> {
    let meta_path = project_meta_path(project_name);
    let mut meta = load_project_meta(project_name)
        .ok_or_else(|| anyhow::anyhow!("project.json が見つかりません: {}", project_name))?;
    meta.shared_dirs = Some(shared_dirs);
    let content =
        serde_json::to_string_pretty(&meta).context("project.json のシリアライズに失敗")?;
    std::fs::write(&meta_path, content)
        .with_context(|| format!("project.json の保存に失敗: {}", meta_path.display()))?;
    Ok(())
}

/// プロジェクト固有の shared_dirs を取得し、無ければグローバル設定にフォールバック
pub fn load_effective_shared_dirs(project_name: &str) -> Vec<String> {
    if let Some(meta) = load_project_meta(project_name) {
        if let Some(dirs) = meta.shared_dirs {
            return dirs;
        }
    }
    load_config(&default_config_path())
        .map(|c| c.siki.shared_dirs)
        .unwrap_or_default()
}

/// .gitignore からシンボリックリンク候補のディレクトリを発見する
pub fn discover_symlink_candidates(project_path: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    let gitignore_path = project_path.join(".gitignore");
    if let Ok(content) = std::fs::read_to_string(&gitignore_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            // 先頭の `/` を除去（ルート指定）してからディレクトリ名を取得
            let dir_name = line.trim_start_matches('/').trim_end_matches('/');
            // 単純なディレクトリ名のみ対象（ワイルドカードやネストパスは除外）
            if dir_name.is_empty()
                || dir_name.contains('*')
                || dir_name.contains('?')
                || dir_name.contains('/')
                || dir_name == ".git"
            {
                continue;
            }
            // 実際にディレクトリとして存在するもののみ
            if project_path.join(dir_name).is_dir() {
                candidates.push(dir_name.to_string());
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

/// worktree メタデータ
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorktreeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// worktree メタデータを project.json から読み込む
pub fn load_worktree_meta(project_name: &str, worktree_name: &str) -> Option<WorktreeMeta> {
    let meta = load_project_meta(project_name)?;
    meta.worktrees.get(worktree_name).cloned()
}

/// project.json 内の worktree display_name を更新する
pub fn save_worktree_display_name(
    project_name: &str,
    worktree_name: &str,
    display_name: Option<&str>,
) -> Result<()> {
    let meta_path = project_meta_path(project_name);
    let mut meta = load_project_meta(project_name)
        .ok_or_else(|| anyhow::anyhow!("project.json が見つかりません: {}", project_name))?;
    // 表示名は通知の OSC シーケンスにも流れ込むため、保存前に制御文字・
    // 行/段落区切りを除去する。除去後に空なら未設定扱い（削除）にする。
    let clean = display_name
        .map(crate::text::sanitize_text)
        .filter(|s| !s.is_empty());
    if let Some(name) = clean {
        meta.worktrees.insert(
            worktree_name.to_string(),
            WorktreeMeta {
                display_name: Some(name),
            },
        );
    } else {
        meta.worktrees.remove(worktree_name);
    }
    let content =
        serde_json::to_string_pretty(&meta).context("project.json のシリアライズに失敗")?;
    std::fs::write(&meta_path, content)
        .with_context(|| format!("project.json の保存に失敗: {}", meta_path.display()))?;
    Ok(())
}

/// 既存の worktree.json を project.json に統合するマイグレーション
pub fn migrate_worktree_meta(project_name: &str) {
    let project_dir = workspaces_dir().join(project_name);
    let sub_entries = match std::fs::read_dir(&project_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut migrated = Vec::new();

    for entry in sub_entries.flatten() {
        let sub_path = entry.path();
        if !sub_path.is_dir() {
            continue;
        }
        let wt_json = sub_path.join("worktree.json");
        if !wt_json.exists() {
            continue;
        }
        let wt_name = match sub_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        if let Ok(content) = std::fs::read_to_string(&wt_json) {
            if let Ok(wt_meta) = serde_json::from_str::<WorktreeMeta>(&content) {
                if wt_meta.display_name.is_some() {
                    migrated.push((wt_name, wt_meta, wt_json));
                } else {
                    // display_name が空なら削除のみ
                    let _ = std::fs::remove_file(&wt_json);
                }
            }
        }
    }

    if migrated.is_empty() {
        return;
    }

    // project.json にマージ
    if let Some(mut meta) = load_project_meta(project_name) {
        for (wt_name, wt_meta, wt_json) in migrated {
            meta.worktrees.entry(wt_name).or_insert(wt_meta);
            let _ = std::fs::remove_file(&wt_json);
        }
        let meta_path = project_meta_path(project_name);
        if let Ok(content) = serde_json::to_string_pretty(&meta) {
            let _ = std::fs::write(&meta_path, content);
        }
    }
}

/// プロジェクトディレクトリ（project.json 含む）を削除する
pub fn remove_project_meta(project_name: &str) -> Result<()> {
    let dir = workspaces_dir().join(project_name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("プロジェクトディレクトリの削除に失敗: {}", dir.display()))?;
    }
    Ok(())
}

/// project.json を読み込む
pub fn load_project_meta(project_name: &str) -> Option<ProjectMeta> {
    let path = project_meta_path(project_name);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// git worktree の .git ファイルからメインリポジトリのパスを推定する
fn detect_project_path_from_worktree(worktree_path: &Path) -> Option<PathBuf> {
    // .git ファイル (not directory) を読み、gitdir パスから元リポジトリを割り出す
    // 形式: "gitdir: /path/to/main/.git/worktrees/<name>"
    let git_file = worktree_path.join(".git");
    let content = std::fs::read_to_string(&git_file).ok()?;
    let gitdir = content.strip_prefix("gitdir: ")?.trim();
    let gitdir_path = PathBuf::from(gitdir);
    // .git/worktrees/<name> → .git → 親ディレクトリ がメインリポジトリ
    let dot_git = gitdir_path.parent()?.parent()?; // .git
    Some(dot_git.parent()?.to_path_buf())
}

/// ~/.siki/workspaces/ をスキャンしてプロジェクト一覧を自動検出する
pub fn discover_projects() -> Vec<ProjectConfig> {
    let ws_dir = workspaces_dir();
    let entries = match std::fs::read_dir(&ws_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut projects = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let project_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        // 既存の worktree.json を project.json に統合するマイグレーション
        migrate_worktree_meta(&project_name);

        // project.json からソースパスを取得、なければ worktree から推定
        let loaded_meta = load_project_meta(&project_name);
        let display_name = loaded_meta.as_ref().and_then(|m| m.display_name.clone());
        let source_path = if let Some(meta) = loaded_meta {
            meta.path
        } else {
            // worktree から推定して project.json を自動生成（マイグレーション）
            let mut detected = None;
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                for sub in sub_entries.flatten() {
                    let sub_path = sub.path();
                    if sub_path.is_dir() && sub_path.join(".git").exists() {
                        if let Some(repo_path) = detect_project_path_from_worktree(&sub_path) {
                            detected = Some(repo_path);
                            break;
                        }
                    }
                }
            }
            match detected {
                Some(repo_path) => {
                    // 検出成功: project.json を自動保存
                    let _ = save_project_meta(&project_name, &repo_path);
                    repo_path.to_string_lossy().to_string()
                }
                None => {
                    // 推定できない場合はスキップ
                    continue;
                }
            }
        };

        // worktree をスキャンしてブランチ情報を取得
        let mut worktrees = Vec::new();
        if let Ok(sub_entries) = std::fs::read_dir(&path) {
            for sub in sub_entries.flatten() {
                let sub_path = sub.path();
                if !sub_path.is_dir() {
                    continue;
                }
                let wt_name = match sub_path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                // .git が存在しないディレクトリは worktree ではない（skills 等をスキップ）
                if !sub_path.join(".git").exists() {
                    continue;
                }

                // git ブランチ名を取得
                let branch = std::process::Command::new("git")
                    .args(["-C", &sub_path.to_string_lossy(), "rev-parse", "--abbrev-ref", "HEAD"])
                    .output()
                    .ok()
                    .and_then(|o| {
                        if o.status.success() {
                            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "unknown".to_string());

                worktrees.push(WorktreeConfig {
                    name: wt_name,
                    branch,
                });
            }
        }

        // 名前順でソート
        worktrees.sort_by(|a, b| a.name.cmp(&b.name));

        projects.push(ProjectConfig {
            name: project_name,
            path: source_path,
            display_name,
            worktrees,
        });
    }

    // 名前順でソート
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    projects
}

/// プロジェクト名のプレフィックスでフィルタ（大文字小文字を区別しない starts_with）
pub fn filter_projects_by_prefix(projects: Vec<ProjectConfig>, prefix: &str) -> Vec<ProjectConfig> {
    let prefix_lower = prefix.to_lowercase();
    projects
        .into_iter()
        .filter(|p| p.name.to_lowercase().starts_with(&prefix_lower))
        .collect()
}

/// cwd がどのプロジェクトに属するか検出する。
/// ソースリポジトリパスと ~/.siki/workspaces/<project>/ の両方をチェック。
pub fn detect_project_from_cwd(projects: &[ProjectConfig], cwd: &std::path::Path) -> Option<String> {
    let ws_dir = workspaces_dir();
    for project in projects {
        let source = std::path::Path::new(&project.path);
        if cwd.starts_with(source) {
            return Some(project.name.clone());
        }
        let workspace = ws_dir.join(&project.name);
        if cwd.starts_with(&workspace) {
            return Some(project.name.clone());
        }
    }
    None
}

/// cwd から (project_name, worktree_name) を推定する
/// ~/.siki/workspaces/<project>/<worktree>/ 配下にいるか、
/// またはプロジェクトの worktree パスに一致するかで判定
pub fn detect_project_worktree_from_cwd(cwd: &std::path::Path) -> Option<(String, String)> {
    let ws = workspaces_dir();
    // ~/.siki/workspaces/<project>/<worktree>/... のパターン
    if let Ok(rel) = cwd.strip_prefix(&ws) {
        let components: Vec<_> = rel.components().collect();
        if components.len() >= 2 {
            let project = components[0].as_os_str().to_string_lossy().to_string();
            let worktree = components[1].as_os_str().to_string_lossy().to_string();
            // "contexts" や "skills" ディレクトリではないことを確認
            if worktree != "contexts" && worktree != "skills" {
                return Some((project, worktree));
            }
        }
    }
    // プロジェクト設定からの検出（worktreeのパスを workspaces_dir から構築）
    let projects = discover_projects();
    for project in &projects {
        for wt in &project.worktrees {
            let wt_path = ws.join(&project.name).join(&wt.name);
            if cwd.starts_with(&wt_path) {
                return Some((project.name.clone(), wt.name.clone()));
            }
        }
        // プロジェクトのソースディレクトリ直下の場合（mainブランチ等）
        let source = std::path::Path::new(&project.path);
        if cwd.starts_with(source) {
            // ソースディレクトリにいる場合、worktree名はディレクトリ名
            let dir_name = source
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&project.name);
            return Some((project.name.clone(), dir_name.to_string()));
        }
    }
    None
}

/// プロジェクトルートの siki.json を表す構造体
#[derive(Debug, Deserialize, Default)]
pub struct SikiJson {
    #[serde(default)]
    pub scripts: SikiScripts,
    /// プロジェクト固有のベースブランチ（例: "origin/main", "origin/develop"）
    #[serde(default)]
    pub base_branch: Option<String>,
}

/// siki.json の scripts セクション
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SikiScripts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
}

/// プロジェクトルートに siki.json が存在するかチェック
pub fn siki_json_exists(project_path: &Path) -> bool {
    project_path.join("siki.json").exists()
}

/// Claude に渡す siki.json 作成用プロンプトを返す
pub fn siki_json_init_prompt() -> String {
    r#"このプロジェクトを分析して、siki.json を作成してください。

siki.json は以下のフォーマットです:
{
  "scripts": {
    "setup": "worktree作成後に実行するセットアップコマンド（例: npm install, cargo build）",
    "run": "開発サーバー起動などの実行コマンド（例: npm run dev）",
    "archive": "worktreeアーカイブ前に実行するクリーンアップコマンド（任意）"
  }
}

手順:
1. プロジェクトの構成ファイル（package.json, Cargo.toml, pyproject.toml, Makefile 等）を確認
2. 使用しているパッケージマネージャーやビルドツールを特定
3. 各スクリプト（setup, run, archive）の内容を提案
4. 確認を取りながら siki.json をプロジェクトルートに書き出す

注意:
- setup は依存パッケージのインストールなど、worktreeを使い始める前に必要な処理
- run は開発中によく使うコマンド（dev server, watch mode 等）
- archive は任意（不要なら null にする）
- 各スクリプトは単一のシェルコマンド文字列で記述

プロジェクトのファイルを確認して、適切な siki.json を提案してください。"#
        .to_string()
}

/// プロジェクトルートの siki.json を読み込む（なければ None）
pub fn load_siki_json(project_path: &Path) -> Option<SikiJson> {
    let path = project_path.join("siki.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// siki.json を優先し、無ければ project.json のスクリプト設定にフォールバック
pub fn load_effective_siki_json(project_path: &Path, project_name: &str) -> Option<SikiJson> {
    // 1. siki.json があればそちらを使用
    if let Some(sj) = load_siki_json(project_path) {
        return Some(sj);
    }
    // 2. project.json のスクリプト設定にフォールバック
    let meta = load_project_meta(project_name)?;
    if meta.scripts.is_none() && meta.base_branch.is_none() {
        return None;
    }
    Some(SikiJson {
        scripts: meta.scripts.unwrap_or_default(),
        base_branch: meta.base_branch,
    })
}

/// プロジェクト固有の Claude Code skills ディレクトリパスを返す
pub fn project_skills_dir(project_name: &str) -> PathBuf {
    workspaces_dir().join(project_name).join("skills")
}

/// worktree 固有のコンテキストディレクトリパスを返す
/// ~/.siki/workspaces/<project>/contexts/<worktree>/ に保存（worktree内に作らない）
pub fn worktree_contexts_dir(project_name: &str, worktree_name: &str) -> PathBuf {
    workspaces_dir()
        .join(project_name)
        .join("contexts")
        .join(worktree_name)
}

/// worktree のコンテキスト一覧を読み込む (名前, 内容) のペア
pub fn load_contexts(project_name: &str, worktree_name: &str) -> Vec<(String, String)> {
    let dir = worktree_contexts_dir(project_name, worktree_name);
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                items.push((name, content));
            }
        }
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items
}

/// バイト数を KB に丸めて返す（四捨五入相当: +512 してから 1024 で割る）。
/// list_sessions の background ポインタと SessionStart hook の件数表示で共用し、
/// 丸め方針を一箇所に集約する。
pub fn bytes_to_kb(bytes: usize) -> usize {
    (bytes + 512) / 1024
}

/// Unix ソケットのパスを返す: ~/.siki/sock
pub fn sock_path() -> std::path::PathBuf {
    siki_home().join("sock")
}

/// SQLite データベースのパスを返す: ~/.siki/siki.db
pub fn db_path() -> std::path::PathBuf {
    siki_home().join("siki.db")
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

/// 設定からデフォルトの LLM コマンドを取得（未指定時は "claude"）
pub fn resolve_llm(config: &Config) -> String {
    config
        .siki
        .llm
        .clone()
        .unwrap_or_else(|| "claude".to_string())
}

/// 利用可能な LLM コマンド一覧を取得（未設定時はデフォルト LLM のみ）
pub fn resolve_llms(config: &Config) -> Vec<String> {
    if config.siki.llms.is_empty() {
        vec![resolve_llm(config)]
    } else {
        config.siki.llms.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_bytes_to_kb_rounds_half_up() {
        assert_eq!(bytes_to_kb(0), 0);
        assert_eq!(bytes_to_kb(511), 0);
        assert_eq!(bytes_to_kb(512), 1);
        assert_eq!(bytes_to_kb(1023), 1);
        assert_eq!(bytes_to_kb(1024), 1);
        assert_eq!(bytes_to_kb(1536), 2);
        assert_eq!(bytes_to_kb(2000), 2);
    }

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
    fn test_parse_missing_projects_uses_default() {
        // projects がない場合は空リストになる
        let toml = r#"
[siki]
shell = "/bin/bash"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.siki.shell, Some("/bin/bash".to_string()));
        assert!(config.projects.is_empty());
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
    fn test_load_config_file_not_found_returns_default() {
        let config = load_config(Path::new("/nonexistent/path/config.toml")).unwrap();
        assert_eq!(config.siki.shell, None);
        assert!(config.siki.shared_dirs.is_empty());
        assert!(config.projects.is_empty());
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
                ..Default::default()
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
                ..Default::default()
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

    #[test]
    fn test_generate_worktree_name_no_conflicts() {
        let name = generate_worktree_name(&[]);
        assert!(!name.is_empty());
        assert!(CITY_NAMES.contains(&name.as_str()));
    }

    #[test]
    fn test_generate_worktree_name_avoids_existing() {
        let existing = vec!["tokyo".to_string(), "osaka".to_string()];
        for _ in 0..20 {
            let name = generate_worktree_name(&existing);
            assert_ne!(name, "tokyo");
            assert_ne!(name, "osaka");
        }
    }

    #[test]
    fn test_generate_worktree_name_all_used() {
        let existing: Vec<String> = CITY_NAMES.iter().map(|s| s.to_string()).collect();
        let name = generate_worktree_name(&existing);
        // サフィックス付きの名前が生成される
        assert!(name.contains('-'));
        assert!(!name.is_empty());
    }

    #[test]
    fn test_save_config_roundtrip() {
        let config = Config {
            siki: SikiConfig {
                shell: Some("/bin/zsh".to_string()),
                shared_dirs: vec!["node_modules".to_string()],
                ..Default::default()
            },
            projects: vec![ProjectConfig {
                name: "myproject".to_string(),
                path: "/tmp/myproject".to_string(),
                display_name: None,
                worktrees: vec![WorktreeConfig {
                    name: "tokyo".to_string(),
                    branch: "feature/auth".to_string(),
                }],
            }],
        };

        let tmpfile = NamedTempFile::new().unwrap();
        save_config(tmpfile.path(), &config).unwrap();

        let loaded = load_config(tmpfile.path()).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn test_load_siki_json_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = r#"{"scripts": {"setup": "echo setup", "run": "echo run", "archive": "echo archive"}}"#;
        std::fs::write(dir.path().join("siki.json"), json).unwrap();

        let siki = load_siki_json(dir.path()).unwrap();
        assert_eq!(siki.scripts.setup.as_deref(), Some("echo setup"));
        assert_eq!(siki.scripts.run.as_deref(), Some("echo run"));
        assert_eq!(siki.scripts.archive.as_deref(), Some("echo archive"));
    }

    #[test]
    fn test_load_siki_json_partial() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = r#"{"scripts": {"setup": "echo setup"}}"#;
        std::fs::write(dir.path().join("siki.json"), json).unwrap();

        let siki = load_siki_json(dir.path()).unwrap();
        assert_eq!(siki.scripts.setup.as_deref(), Some("echo setup"));
        assert!(siki.scripts.run.is_none());
        assert!(siki.scripts.archive.is_none());
    }

    #[test]
    fn test_load_siki_json_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(load_siki_json(dir.path()).is_none());
    }

    #[test]
    fn test_load_siki_json_invalid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("siki.json"), "not valid json {{{").unwrap();
        assert!(load_siki_json(dir.path()).is_none());
    }

    #[test]
    fn test_save_config_invalid_path() {
        let config = Config {
            siki: SikiConfig::default(),
            projects: vec![],
        };
        let result = save_config(Path::new("/nonexistent/dir/config.toml"), &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_save_and_load_project_meta() {
        let dir = tempfile::TempDir::new().unwrap();
        // workspaces_dir を直接使うとグローバルに影響するため、
        // save/load の中身を直接テスト
        let meta = ProjectMeta {
            path: "/tmp/my-project".to_string(),
            display_name: None,
            scripts: None,
            base_branch: None,
            shared_dirs: None,
            worktrees: HashMap::new(),
        };
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let meta_path = dir.path().join("project.json");
        std::fs::write(&meta_path, &json).unwrap();

        let content = std::fs::read_to_string(&meta_path).unwrap();
        let loaded: ProjectMeta = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.path, "/tmp/my-project");
    }

    #[test]
    fn test_detect_project_path_from_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        let wt_dir = dir.path().join("my-wt");
        std::fs::create_dir_all(&wt_dir).unwrap();

        // .git ファイルを作成（git worktree の形式）
        let gitdir = "/home/user/myproject/.git/worktrees/my-wt";
        std::fs::write(wt_dir.join(".git"), format!("gitdir: {}", gitdir)).unwrap();

        let result = super::detect_project_path_from_worktree(&wt_dir);
        assert_eq!(
            result,
            Some(PathBuf::from("/home/user/myproject"))
        );
    }

    #[test]
    fn test_detect_project_path_no_git_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = super::detect_project_path_from_worktree(dir.path());
        assert!(result.is_none());
    }
}
