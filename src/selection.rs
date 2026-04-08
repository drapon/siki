use ratatui::prelude::Rect;

/// ターミナルコンテンツ内の位置（セル座標）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermPos {
    pub row: u16,
    pub col: u16,
}

/// 選択が開始されたパネル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPanel {
    Claude,
    Terminal,
    File,
}

/// テキスト選択状態
#[derive(Debug, Clone)]
pub struct TextSelection {
    /// ドラッグ開始点
    pub anchor: TermPos,
    /// 現在のマウス位置
    pub current: TermPos,
    /// ドラッグ中かどうか
    pub active: bool,
    /// 選択が開始されたパネル
    pub panel: SelectionPanel,
}

impl TextSelection {
    /// 読み順で (start, end) を返す
    pub fn normalize(&self) -> (TermPos, TermPos) {
        let a = self.anchor;
        let b = self.current;
        if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// start == end なら空選択
    pub fn is_empty(&self) -> bool {
        self.anchor == self.current
    }
}

/// マウスのスクリーン座標をターミナルセル座標に変換
///
/// content_area: ボーダーを除いたClaudeペインのコンテンツ領域
/// 範囲外の場合はクランプする
pub fn screen_to_term(column: u16, row: u16, content_area: &Rect) -> TermPos {
    let col = if column < content_area.x {
        0
    } else {
        (column - content_area.x).min(content_area.width.saturating_sub(1))
    };
    let r = if row < content_area.y {
        0
    } else {
        (row - content_area.y).min(content_area.height.saturating_sub(1))
    };
    TermPos { row: r, col }
}

/// vt100::Screen から選択範囲のテキストを抽出
pub fn extract_text(screen: &vt100::Screen, start: &TermPos, end: &TermPos) -> String {
    let (rows, cols) = screen.size();
    let start_row = start.row.min(rows);
    let start_col = start.col.min(cols);
    let end_row = end.row.min(rows);
    let end_col = (end.col + 1).min(cols);
    screen.contents_between(start_row, start_col, end_row, end_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_forward() {
        let sel = TextSelection {
            anchor: TermPos { row: 0, col: 5 },
            current: TermPos { row: 2, col: 3 },
            active: false,
            panel: SelectionPanel::Claude,
        };
        let (start, end) = sel.normalize();
        assert_eq!(start, TermPos { row: 0, col: 5 });
        assert_eq!(end, TermPos { row: 2, col: 3 });
    }

    #[test]
    fn test_normalize_backward() {
        let sel = TextSelection {
            anchor: TermPos { row: 2, col: 3 },
            current: TermPos { row: 0, col: 5 },
            active: false,
            panel: SelectionPanel::Claude,
        };
        let (start, end) = sel.normalize();
        assert_eq!(start, TermPos { row: 0, col: 5 });
        assert_eq!(end, TermPos { row: 2, col: 3 });
    }

    #[test]
    fn test_screen_to_term_clamp() {
        let area = Rect::new(10, 5, 80, 24);
        // 範囲内
        assert_eq!(screen_to_term(15, 10, &area), TermPos { row: 5, col: 5 });
        // 左上にはみ出し → (0, 0) にクランプ
        assert_eq!(screen_to_term(3, 2, &area), TermPos { row: 0, col: 0 });
        // 右下にはみ出し → 最大にクランプ
        assert_eq!(screen_to_term(200, 100, &area), TermPos { row: 23, col: 79 });
    }

    #[test]
    fn test_is_empty() {
        let sel = TextSelection {
            anchor: TermPos { row: 1, col: 5 },
            current: TermPos { row: 1, col: 5 },
            active: false,
            panel: SelectionPanel::Claude,
        };
        assert!(sel.is_empty());
    }
}
