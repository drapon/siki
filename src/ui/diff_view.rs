use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// 1 ファイル分の diff 情報
#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub status: char,
    pub additions: usize,
    pub deletions: usize,
    pub diff_content: String,
    /// true ならリモートに push 済み
    pub pushed: bool,
}

const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
pub const REFRESH_SPINNER_TICKS: u8 = 5;

/// タイトルにスピナーを付与した Line を生成する
fn title_with_spinner<'a>(title: Option<&str>, spinner_tick: u8) -> Option<Line<'a>> {
    title.map(|t| {
        if spinner_tick > 0 {
            let ch = SPINNER_CHARS[(REFRESH_SPINNER_TICKS - spinner_tick) as usize % SPINNER_CHARS.len()];
            Line::from(vec![
                Span::raw(format!("{} ", t)),
                Span::styled(format!("{}", ch), Style::default().fg(Color::Cyan)),
            ])
        } else {
            Line::from(t.to_string())
        }
    })
}

/// Git diff の状態（ファイルベース）
#[derive(Debug)]
pub struct DiffView {
    pub files: Vec<DiffFile>,
    pub selected_file: usize,
    pub diff_scroll_offset: usize,
    /// リフレッシュスピナーの残り tick 数（0 = 非表示）
    pub refresh_spinner: u8,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            selected_file: 0,
            diff_scroll_offset: 0,
            refresh_spinner: 0,
        }
    }

    /// worktree パスの git diff を取得・パースする
    /// base_branch: PR のベースブランチ (e.g. "origin/main")
    pub fn load(&mut self, worktree_path: &Path, base_branch: &str) {
        self.selected_file = 0;
        self.diff_scroll_offset = 0;
        let raw = get_git_diff(worktree_path, base_branch);
        self.files = parse_diff(&raw);

        // push 済みファイルにフラグを設定
        let pushed_set = get_pushed_files(worktree_path, base_branch);
        for file in &mut self.files {
            file.pushed = pushed_set.contains(&file.path);
        }
    }

    /// ファイル選択を下へ
    pub fn select_next(&mut self) {
        if !self.files.is_empty() && self.selected_file < self.files.len() - 1 {
            self.selected_file += 1;
            self.diff_scroll_offset = 0;
        }
    }

    /// ファイル選択を上へ
    pub fn select_prev(&mut self) {
        if self.selected_file > 0 {
            self.selected_file -= 1;
            self.diff_scroll_offset = 0;
        }
    }

    /// diff 内容をスクロールダウン
    pub fn scroll_down(&mut self) {
        let max = self.selected_diff_line_count().saturating_sub(1);
        if self.diff_scroll_offset < max {
            self.diff_scroll_offset += 1;
        }
    }

    /// diff 内容をスクロールアップ
    pub fn scroll_up(&mut self) {
        self.diff_scroll_offset = self.diff_scroll_offset.saturating_sub(1);
    }

    fn selected_diff_line_count(&self) -> usize {
        self.files
            .get(self.selected_file)
            .map(|f| f.diff_content.lines().count())
            .unwrap_or(0)
    }

    /// 選択中ファイルの diff_content を返す
    pub fn selected_diff_content(&self) -> Option<&str> {
        self.files
            .get(self.selected_file)
            .map(|f| f.diff_content.as_str())
    }

    /// 選択中ファイルのパスを返す
    pub fn selected_path(&self) -> Option<&str> {
        self.files.get(self.selected_file).map(|f| f.path.as_str())
    }

    /// 描画（ファイル一覧のみ）
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool, title: Option<&str>) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        if self.files.is_empty() {
            let mut block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style);
            if let Some(t) = title_with_spinner(title, self.refresh_spinner) {
                block = block.title(t);
            }
            frame.render_widget(
                Paragraph::new("変更なし")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        self.render_file_list(frame, area, focused, border_style, title);
    }

    fn render_file_list(&self, frame: &mut Frame, area: Rect, focused: bool, border_style: Style, title: Option<&str>) {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        if let Some(t) = title_with_spinner(title, self.refresh_spinner) {
            block = block.title(t);
        }

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines: Vec<Line> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let status_style = match file.status {
                    'A' => Style::default().fg(Color::Green),
                    'D' => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::Yellow),
                };

                let stat = format_stat(file.additions, file.deletions);

                let is_selected = i == self.selected_file;
                let base_style = if is_selected && focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                let (push_indicator, push_color) = if file.pushed {
                    ("↑", Color::DarkGray)
                } else {
                    ("●", Color::Magenta)
                };

                let mut spans = vec![
                    Span::styled(format!("{} ", push_indicator), Style::default().fg(push_color).patch(base_style)),
                    Span::styled(format!("{} ", file.status), status_style.patch(base_style)),
                    Span::styled(&file.path, Style::default().fg(Color::White).patch(base_style)),
                ];

                // 右端に stat を表示するためスペースで埋める
                // インジケータ(1) + スペース(1) + ステータス(1) + スペース(1) + パス + stat
                let used = 4 + file.path.len() + stat.len();
                let padding = (inner.width as usize).saturating_sub(used);
                spans.push(Span::styled(" ".repeat(padding), base_style));

                // additions
                if file.additions > 0 {
                    spans.push(Span::styled(
                        format!("+{}", file.additions),
                        Style::default().fg(Color::Green).patch(base_style),
                    ));
                }
                if file.additions > 0 && file.deletions > 0 {
                    spans.push(Span::styled(" ", base_style));
                }
                // deletions
                if file.deletions > 0 {
                    spans.push(Span::styled(
                        format!("-{}", file.deletions),
                        Style::default().fg(Color::Red).patch(base_style),
                    ));
                }

                Line::from(spans)
            })
            .collect();

        // スクロール対応（ファイル数が多い場合）
        let visible_height = inner.height as usize;
        let scroll = if self.selected_file >= visible_height {
            self.selected_file - visible_height + 1
        } else {
            0
        };

        let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0));
        frame.render_widget(paragraph, inner);
    }

}

/// ローカル未コミット変更の状態（ファイルベース）
#[derive(Debug)]
pub struct LocalChangesView {
    pub files: Vec<DiffFile>,
    pub selected_file: usize,
    /// リフレッシュスピナーの残り tick 数（0 = 非表示）
    pub refresh_spinner: u8,
}

impl LocalChangesView {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            selected_file: 0,
            refresh_spinner: 0,
        }
    }

    /// worktree パスのローカル未コミット変更を取得・パースする
    pub fn load(&mut self, worktree_path: &Path) {
        self.selected_file = 0;
        let raw = get_local_diff(worktree_path);
        self.files = parse_diff(&raw);
    }

    pub fn select_next(&mut self) {
        if !self.files.is_empty() && self.selected_file < self.files.len() - 1 {
            self.selected_file += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected_file > 0 {
            self.selected_file -= 1;
        }
    }

    pub fn selected_diff_content(&self) -> Option<&str> {
        self.files
            .get(self.selected_file)
            .map(|f| f.diff_content.as_str())
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.files.get(self.selected_file).map(|f| f.path.as_str())
    }

    /// 描画（ファイル一覧のみ、DiffView と同じ形式）
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool, title: Option<&str>) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        if self.files.is_empty() {
            let mut block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style);
            if let Some(t) = title_with_spinner(title, self.refresh_spinner) {
                block = block.title(t);
            }
            frame.render_widget(
                Paragraph::new("変更なし")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        if let Some(t) = title_with_spinner(title, self.refresh_spinner) {
            block = block.title(t);
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines: Vec<Line> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let status_style = match file.status {
                    'A' => Style::default().fg(Color::Green),
                    'D' => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::Yellow),
                };
                let stat = format_stat(file.additions, file.deletions);
                let is_selected = i == self.selected_file;
                let base_style = if is_selected && focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };
                let mut spans = vec![
                    Span::styled(format!(" {} ", file.status), status_style.patch(base_style)),
                    Span::styled(&file.path, Style::default().fg(Color::White).patch(base_style)),
                ];
                let used = 3 + file.path.len() + stat.len();
                let padding = (inner.width as usize).saturating_sub(used);
                spans.push(Span::styled(" ".repeat(padding), base_style));
                if file.additions > 0 {
                    spans.push(Span::styled(
                        format!("+{}", file.additions),
                        Style::default().fg(Color::Green).patch(base_style),
                    ));
                }
                if file.additions > 0 && file.deletions > 0 {
                    spans.push(Span::styled(" ", base_style));
                }
                if file.deletions > 0 {
                    spans.push(Span::styled(
                        format!("-{}", file.deletions),
                        Style::default().fg(Color::Red).patch(base_style),
                    ));
                }
                Line::from(spans)
            })
            .collect();

        let visible_height = inner.height as usize;
        let scroll = if self.selected_file >= visible_height {
            self.selected_file - visible_height + 1
        } else {
            0
        };
        let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0));
        frame.render_widget(paragraph, inner);
    }
}

pub fn classify_line_style(line: &str) -> Style {
    if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with("diff ") || line.starts_with("index ") {
        Style::default().fg(Color::Yellow).bold()
    } else {
        Style::default()
    }
}

fn format_stat(additions: usize, deletions: usize) -> String {
    let mut parts = Vec::new();
    if additions > 0 {
        parts.push(format!("+{additions}"));
    }
    if deletions > 0 {
        parts.push(format!("-{deletions}"));
    }
    parts.join(" ")
}

/// git diff HEAD の出力をファイル単位にパースする
fn parse_diff(raw: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current_content = String::new();
    let mut current_path = String::new();

    for line in raw.lines() {
        if line.starts_with("diff --git ") {
            // 前のファイルを保存
            if !current_path.is_empty() {
                let (additions, deletions) = count_changes(&current_content);
                files.push(DiffFile {
                    path: current_path,
                    status: detect_status(&current_content),
                    additions,
                    deletions,
                    diff_content: current_content,
                    pushed: false,
                });
                current_content = String::new();
            }
            // "diff --git a/path b/path" → path を抽出
            current_path = extract_path(line);
        }
        if !current_path.is_empty() {
            if !current_content.is_empty() {
                current_content.push('\n');
            }
            current_content.push_str(line);
        }
    }

    // 最後のファイル
    if !current_path.is_empty() {
        let (additions, deletions) = count_changes(&current_content);
        files.push(DiffFile {
            path: current_path,
            status: detect_status(&current_content),
            additions,
            deletions,
            diff_content: current_content,
            pushed: false,
        });
    }

    files
}

fn extract_path(diff_header: &str) -> String {
    // "diff --git a/src/main.rs b/src/main.rs"
    if let Some(b_part) = diff_header.split(" b/").last() {
        b_part.to_string()
    } else {
        diff_header.to_string()
    }
}

fn detect_status(content: &str) -> char {
    for line in content.lines() {
        if line.starts_with("new file") {
            return 'A';
        }
        if line.starts_with("deleted file") {
            return 'D';
        }
        if line.starts_with("rename from") {
            return 'R';
        }
    }
    'M'
}

fn count_changes(content: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in content.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            additions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }
    (additions, deletions)
}

/// ローカル未コミット変更（staged + unstaged）の diff を取得する
fn get_local_diff(worktree_path: &Path) -> String {
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(worktree_path)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => String::new(),
    }
}

/// リモートに push 済みのファイル一覧を取得する
/// base_branch...@{upstream} の diff --name-only で判定
fn get_pushed_files(worktree_path: &Path, base_branch: &str) -> HashSet<String> {
    let merge_base_arg = format!("{}...@{{upstream}}", base_branch);
    let output = Command::new("git")
        .args(["diff", "--name-only", &merge_base_arg])
        .current_dir(worktree_path)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.lines().map(|s| s.to_string()).collect()
        }
        _ => HashSet::new(),
    }
}

/// git diff コマンドを実行して結果を返す
/// base_branch からの差分（PR の変更内容）を取得する
fn get_git_diff(worktree_path: &Path, base_branch: &str) -> String {
    // merge-base を使ってベースブランチからの差分を取得
    let merge_base_arg = format!("{}...HEAD", base_branch);
    let output = Command::new("git")
        .args(["diff", &merge_base_arg])
        .current_dir(worktree_path)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        Ok(output) => {
            format!(
                "git diff エラー: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        }
        Err(e) => format!("git コマンドの実行に失敗: {}", e),
    }
}

/// テスト用: diff 文字列を直接パースしてスタイル判定するためのヘルパー
pub fn classify_diff_line(line: &str) -> DiffLineType {
    if line.starts_with('+') && !line.starts_with("+++") {
        DiffLineType::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        DiffLineType::Removed
    } else if line.starts_with("@@") {
        DiffLineType::Hunk
    } else if line.starts_with("diff ") || line.starts_with("index ") {
        DiffLineType::Header
    } else {
        DiffLineType::Context
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineType {
    Added,
    Removed,
    Hunk,
    Header,
    Context,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_added_line() {
        assert_eq!(classify_diff_line("+new line"), DiffLineType::Added);
    }

    #[test]
    fn test_classify_removed_line() {
        assert_eq!(classify_diff_line("-old line"), DiffLineType::Removed);
    }

    #[test]
    fn test_classify_hunk_header() {
        assert_eq!(
            classify_diff_line("@@ -1,3 +1,4 @@"),
            DiffLineType::Hunk
        );
    }

    #[test]
    fn test_classify_diff_header() {
        assert_eq!(
            classify_diff_line("diff --git a/file b/file"),
            DiffLineType::Header
        );
    }

    #[test]
    fn test_classify_index_header() {
        assert_eq!(
            classify_diff_line("index abc123..def456 100644"),
            DiffLineType::Header
        );
    }

    #[test]
    fn test_classify_context_line() {
        assert_eq!(classify_diff_line(" unchanged line"), DiffLineType::Context);
    }

    #[test]
    fn test_classify_triple_plus() {
        assert_eq!(classify_diff_line("+++ b/file.rs"), DiffLineType::Context);
    }

    #[test]
    fn test_classify_triple_minus() {
        assert_eq!(classify_diff_line("--- a/file.rs"), DiffLineType::Context);
    }

    #[test]
    fn test_parse_diff_single_file() {
        let raw = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2";
        let files = parse_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].status, 'M');
        assert_eq!(files[0].additions, 1);
        assert_eq!(files[0].deletions, 0);
    }

    #[test]
    fn test_parse_diff_multiple_files() {
        let raw = "\
diff --git a/file1.rs b/file1.rs
--- a/file1.rs
+++ b/file1.rs
@@ -1 +1 @@
-old
+new
diff --git a/file2.rs b/file2.rs
new file mode 100644
--- /dev/null
+++ b/file2.rs
@@ -0,0 +1 @@
+content";
        let files = parse_diff(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "file1.rs");
        assert_eq!(files[0].status, 'M');
        assert_eq!(files[0].additions, 1);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[1].path, "file2.rs");
        assert_eq!(files[1].status, 'A');
        assert_eq!(files[1].additions, 1);
        assert_eq!(files[1].deletions, 0);
    }

    #[test]
    fn test_parse_diff_empty() {
        let files = parse_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_select_next_prev() {
        let mut view = DiffView::new();
        view.files = vec![
            DiffFile {
                path: "a.rs".into(),
                status: 'M',
                additions: 1,
                deletions: 0,
                diff_content: "+line".into(),
                pushed: false,
            },
            DiffFile {
                path: "b.rs".into(),
                status: 'M',
                additions: 0,
                deletions: 1,
                diff_content: "-line".into(),
                pushed: false,
            },
        ];
        assert_eq!(view.selected_file, 0);
        view.select_next();
        assert_eq!(view.selected_file, 1);
        view.select_next(); // 末尾で止まる
        assert_eq!(view.selected_file, 1);
        view.select_prev();
        assert_eq!(view.selected_file, 0);
        view.select_prev(); // 先頭で止まる
        assert_eq!(view.selected_file, 0);
    }

    #[test]
    fn test_scroll_diff_content() {
        let mut view = DiffView::new();
        view.files = vec![DiffFile {
            path: "a.rs".into(),
            status: 'M',
            additions: 0,
            deletions: 0,
            diff_content: "line1\nline2\nline3".into(),
            pushed: false,
        }];
        assert_eq!(view.diff_scroll_offset, 0);
        view.scroll_down();
        assert_eq!(view.diff_scroll_offset, 1);
        view.scroll_down();
        assert_eq!(view.diff_scroll_offset, 2);
        view.scroll_down(); // 末尾で止まる
        assert_eq!(view.diff_scroll_offset, 2);
        view.scroll_up();
        assert_eq!(view.diff_scroll_offset, 1);
    }

    #[test]
    fn test_select_resets_scroll() {
        let mut view = DiffView::new();
        view.files = vec![
            DiffFile {
                path: "a.rs".into(),
                status: 'M',
                additions: 0,
                deletions: 0,
                diff_content: "line1\nline2".into(),
                pushed: false,
            },
            DiffFile {
                path: "b.rs".into(),
                status: 'M',
                additions: 0,
                deletions: 0,
                diff_content: "line1".into(),
                pushed: false,
            },
        ];
        view.scroll_down();
        assert_eq!(view.diff_scroll_offset, 1);
        view.select_next();
        assert_eq!(view.diff_scroll_offset, 0);
    }

    #[test]
    fn test_diff_view_new() {
        let view = DiffView::new();
        assert!(view.files.is_empty());
        assert_eq!(view.selected_file, 0);
        assert_eq!(view.diff_scroll_offset, 0);
    }

    #[test]
    fn test_diff_view_load_nonexistent() {
        let mut view = DiffView::new();
        view.load(Path::new("/nonexistent/path"), "origin/main");
        // エラー時は files が空のまま（git diff がエラーメッセージを返すが diff --git で始まらないのでパースされない）
        assert_eq!(view.selected_file, 0);
    }

    #[test]
    fn test_detect_status() {
        assert_eq!(detect_status("new file mode 100644\n+++ b/f"), 'A');
        assert_eq!(detect_status("deleted file mode 100644"), 'D');
        assert_eq!(detect_status("rename from old\nrename to new"), 'R');
        assert_eq!(detect_status("--- a/f\n+++ b/f"), 'M');
    }

    #[test]
    fn test_extract_path() {
        assert_eq!(
            extract_path("diff --git a/src/main.rs b/src/main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn test_scroll_empty_files() {
        let mut view = DiffView::new();
        view.scroll_down();
        assert_eq!(view.diff_scroll_offset, 0);
        view.select_next();
        assert_eq!(view.selected_file, 0);
    }
}
