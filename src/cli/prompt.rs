//! 対話セレクタ。
//!
//! crossterm（既存依存）ベースの `select_one` / `input_line` / `confirm` を提供する。
//! `select_one` は raw mode を内部で取得・解除し（RAII ガードでパニック時も復帰）、
//! alternate screen は使わず通常スクロールバック内にインライン描画する。
//! 対話 UI は stdout を汚さないよう stderr に出力する（`path`/`list` の stdout と分離）。
//!
//! 再描画は「1 項目 = 1 物理行」を前提に `MoveUp(項目数)` で項目先頭へ戻る。各行は端末幅で
//! 切り詰めて折り返しを防ぐ（長い branch 名対策）。項目数が端末高さを超えるケースは
//! スクロールにより崩れうる既知の制約（worktree 数は通常小さい前提）。

use anyhow::{bail, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType},
    QueueableCommand,
};
use std::io::{stderr, stdin, IsTerminal, Write};
use unicode_width::UnicodeWidthChar;

/// セレクタの 1 行（ラベル + 右側補助表示。例: "tokyo" + "[feature/auth]"）。
pub struct SelectItem {
    pub label: String,
    pub note: Option<String>,
}

impl SelectItem {
    pub fn new(label: impl Into<String>, note: Option<String>) -> Self {
        Self {
            label: label.into(),
            note,
        }
    }
}

/// raw mode の解除とカーソル再表示を確実に行うガード（早期 return / パニック時も復帰）。
struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let mut out = stderr();
        let _ = out.queue(cursor::Show);
        let _ = out.flush();
        let _ = disable_raw_mode();
    }
}

/// 文字列を表示幅（端末カラム）で切り詰める純粋関数。全角文字を考慮する。
fn truncate_to_width(s: &str, max_cols: usize) -> String {
    let mut width = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > max_cols {
            break;
        }
        width += w;
        out.push(ch);
    }
    out
}

/// カーソル位置を 1 つ動かす（端で巻き戻す）純粋関数。`up=true` で上方向。
fn next_cursor(cur: usize, len: usize, up: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if up {
        if cur == 0 {
            len - 1
        } else {
            cur - 1
        }
    } else if cur + 1 >= len {
        0
    } else {
        cur + 1
    }
}

fn write_items<W: Write>(out: &mut W, items: &[SelectItem], cur: usize, width: usize) -> Result<()> {
    for (i, it) in items.iter().enumerate() {
        let marker = if i == cur { "❯ " } else { "  " };
        let note = it
            .note
            .as_deref()
            .map(|n| format!("  {}", n))
            .unwrap_or_default();
        // 端末幅で切り詰めて折り返し（= 物理行が増えて MoveUp がずれる）を防ぐ。
        let line = truncate_to_width(&format!("{}{}{}", marker, it.label, note), width);
        out.queue(Clear(ClearType::CurrentLine))?;
        out.queue(cursor::MoveToColumn(0))?;
        write!(out, "{}\r\n", line)?;
    }
    Ok(())
}

/// 矢印キーで 1 件選択する（↑↓/jk 移動・Enter 決定・Esc/Ctrl-C 中止）。
/// 戻り値 `None` は中止。
pub fn select_one(title: &str, items: &[SelectItem]) -> Result<Option<usize>> {
    if items.is_empty() {
        bail!("選択肢がありません");
    }

    // 非対話環境（CI/パイプ）では raw mode + event::read が制御端末に依存し、
    // 到達しないキー入力を待ってハング（あるいは不明瞭な OS エラー）になる。
    // 事前に stdin が端末かを確認し、明快なエラーで即座に中止する。
    if !stdin().is_terminal() {
        bail!("対話選択できません（非対話環境）。引数でプロジェクト/worktree 名を指定してください");
    }

    let mut out = stderr();
    enable_raw_mode()?;
    let _guard = TermGuard;

    // 折り返し防止に使う表示幅（取得失敗時は 80 にフォールバック）。
    // 選択中のリサイズに追従するため mut。
    let mut width = size().map(|(c, _)| c as usize).unwrap_or(80);

    out.queue(cursor::Hide)?;
    write!(out, "{}\r\n", title)?;
    let mut cur = 0usize;
    write_items(&mut out, items, cur, width)?;
    out.flush()?;

    let chosen = loop {
        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => cur = next_cursor(cur, items.len(), true),
                    KeyCode::Down | KeyCode::Char('j') => cur = next_cursor(cur, items.len(), false),
                    KeyCode::Enter => break Some(cur),
                    KeyCode::Esc => break None,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break None
                    }
                    _ => continue,
                }
            }
            // 端末リサイズに追従して幅を更新する（古い幅のままだと折り返し→MoveUp ずれで崩れる）。
            Event::Resize(new_w, _) => width = new_w as usize,
            _ => continue,
        }
        out.queue(cursor::MoveUp(items.len() as u16))?;
        write_items(&mut out, items, cur, width)?;
        out.flush()?;
    };

    // 次の出力が項目行の直後ではなく行頭から始まるよう改行を 1 つ出す。
    // カーソル再表示と raw 解除は TermGuard が行う。
    write!(out, "\r\n")?;
    out.flush()?;
    Ok(chosen)
}

/// 入力文字列を解決する純粋関数（空入力なら default、なければ空文字）。
fn resolve_input(raw: &str, default: Option<&str>) -> String {
    let t = raw.trim();
    if t.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        t.to_string()
    }
}

/// 1 行テキスト入力（空入力で default）。stdin を行単位で読む（raw mode 不要）。
pub fn input_line(prompt: &str, default: Option<&str>) -> Result<String> {
    let mut out = stderr();
    let hint = default.map(|d| format!(" [{}]", d)).unwrap_or_default();
    write!(out, "{}{} > ", prompt, hint)?;
    out.flush()?;

    let mut line = String::new();
    stdin().read_line(&mut line)?;
    Ok(resolve_input(&line, default))
}

/// 確認入力を解釈する純粋関数。空は default、y/yes=true・n/no=false、その他は false（安全側）。
fn parse_confirm(raw: &str, default_yes: bool) -> bool {
    match raw.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        "" => default_yes,
        _ => false,
    }
}

/// y/N 確認（rm の誤削除防止）。
pub fn confirm(prompt: &str, default_yes: bool) -> Result<bool> {
    let mut out = stderr();
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    write!(out, "{} {} > ", prompt, hint)?;
    out.flush()?;

    let mut line = String::new();
    stdin().read_line(&mut line)?;
    Ok(parse_confirm(&line, default_yes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_cursor_wraps() {
        // 下方向: 末尾で先頭へ
        assert_eq!(next_cursor(0, 3, false), 1);
        assert_eq!(next_cursor(2, 3, false), 0);
        // 上方向: 先頭で末尾へ
        assert_eq!(next_cursor(0, 3, true), 2);
        assert_eq!(next_cursor(1, 3, true), 0);
        // 長さ0は0
        assert_eq!(next_cursor(0, 0, false), 0);
    }

    #[test]
    fn resolve_input_uses_default_on_empty() {
        assert_eq!(resolve_input("  kyoto \n", None), "kyoto");
        assert_eq!(resolve_input("\n", Some("origin/main")), "origin/main");
        assert_eq!(resolve_input("   ", Some("d")), "d");
        assert_eq!(resolve_input("x", None), "x");
        assert_eq!(resolve_input("", None), "");
    }

    #[test]
    fn truncate_to_width_handles_ascii_and_wide() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 3), "hel");
        assert_eq!(truncate_to_width("hello", 0), "");
        // 全角は幅2: "あい" は幅4。max=3 では "あ"(幅2)まで（次の"い"で4>3）
        assert_eq!(truncate_to_width("あいう", 3), "あ");
        assert_eq!(truncate_to_width("あいう", 4), "あい");
    }

    #[test]
    fn parse_confirm_variants() {
        assert!(parse_confirm("y\n", false));
        assert!(parse_confirm("YES\n", false));
        assert!(!parse_confirm("n\n", true));
        assert!(!parse_confirm("no\n", true));
        // 空は default
        assert!(parse_confirm("\n", true));
        assert!(!parse_confirm("\n", false));
        // 未知入力は安全側で false
        assert!(!parse_confirm("maybe\n", true));
    }
}
