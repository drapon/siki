//! テキストサニタイズの共通ユーティリティ
//!
//! 端末出力・エスケープシーケンス・永続化される表示名を壊しうる文字を除去する。
//! ユーザー入力（worktree 名 / 表示名）は通知の OSC シーケンスや project.json に
//! 流れ込むため、入力・永続化・出力の各段でこの共通ロジックを通す。

/// 端末出力やエスケープシーケンスを壊しうる文字か判定する。
///
/// `char::is_control()` は Cc カテゴリ（C0: U+0000–001F、DEL: U+007F、
/// C1: U+0080–009F）のみを対象とし、U+2028 (LINE SEPARATOR) /
/// U+2029 (PARAGRAPH SEPARATOR) を含まない。これらは一部端末（VTE 系等）で
/// 行終端と解釈され表示を乱すため、明示的に対象へ加える。
pub fn is_unsafe_text_char(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// 制御文字・行/段落区切りを除去した新しい文字列を返す。
pub fn sanitize_text(s: &str) -> String {
    s.chars().filter(|c| !is_unsafe_text_char(*c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_text_strips_c0_and_del_control() {
        // NUL/ESC/BEL/LF/CR/TAB（C0）と DEL を除去する
        assert_eq!(sanitize_text("a\x00b\x1bc\x07d\ne\rf\tg\x7fh"), "abcdefgh");
    }

    #[test]
    fn test_sanitize_text_strips_c1_control() {
        // C1 制御 (U+0080–009F) も is_control() で除去される
        assert_eq!(sanitize_text("x\u{0085}y\u{009b}z"), "xyz");
    }

    #[test]
    fn test_sanitize_text_strips_line_paragraph_separators() {
        // U+2028 / U+2029 は char::is_control() では false だが除去する
        assert_eq!(sanitize_text("foo\u{2028}bar\u{2029}baz"), "foobarbaz");
    }

    #[test]
    fn test_sanitize_text_keeps_printable_including_multibyte_and_semicolon() {
        // 印字可能文字・日本語・絵文字・`;` は保持する（`;` 除去は OSC 固有のため別処理）
        assert_eq!(sanitize_text("機能追加 v1; ok😀"), "機能追加 v1; ok😀");
    }

    #[test]
    fn test_is_unsafe_text_char() {
        assert!(is_unsafe_text_char('\n'));
        assert!(is_unsafe_text_char('\r'));
        assert!(is_unsafe_text_char('\u{2028}'));
        assert!(is_unsafe_text_char('\u{2029}'));
        assert!(is_unsafe_text_char('\u{009b}')); // C1
        assert!(!is_unsafe_text_char('a'));
        assert!(!is_unsafe_text_char(';'));
        assert!(!is_unsafe_text_char('機'));
    }
}
