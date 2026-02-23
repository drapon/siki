use crate::app::Panel;
use ratatui::prelude::*;

/// 各パネルの描画エリア
pub struct AppLayout {
    pub left: Rect,
    pub main: Rect,
    pub right_top: Rect,
    pub right_bottom: Rect,
    pub status_bar: Rect,
}

impl AppLayout {
    /// 座標がどのパネルに属するか判定する
    pub fn hit_test(&self, column: u16, row: u16) -> Option<Panel> {
        if self.left.contains(Position::new(column, row)) {
            Some(Panel::Left)
        } else if self.main.contains(Position::new(column, row)) {
            Some(Panel::Main)
        } else if self.right_top.contains(Position::new(column, row)) {
            Some(Panel::Right)
        } else if self.right_bottom.contains(Position::new(column, row)) {
            Some(Panel::Terminal)
        } else {
            None
        }
    }
}

/// 画面全体のレイアウトを計算する
///
/// 構成:
/// - 上部: メイン領域（左20% / 中央55% / 右25%）
/// - 下部: ステータスバー（1行）
/// - 右パネル: 上部60% / 下部40%
pub fn compute_layout(area: Rect) -> AppLayout {
    // メイン領域 + ステータスバー
    let vertical = Layout::vertical([
        Constraint::Min(0),    // メイン
        Constraint::Length(1), // ステータスバー
    ])
    .split(area);

    let main_area = vertical[0];
    let status_bar = vertical[1];

    // 3列分割: 左20% / 中央55% / 右25%
    let columns = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(55),
        Constraint::Percentage(25),
    ])
    .split(main_area);

    let left = columns[0];
    let main = columns[1];
    let right = columns[2];

    // 右パネルを縦分割: 上部60% / 下部40%
    let right_split = Layout::vertical([
        Constraint::Percentage(60),
        Constraint::Percentage(40),
    ])
    .split(right);

    AppLayout {
        left,
        main,
        right_top: right_split[0],
        right_bottom: right_split[1],
        status_bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_layout_proportions() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);

        // 左パネル: 約20% = 40
        assert_eq!(layout.left.width, 40);
        // 中央パネル: 約55% = 110
        assert_eq!(layout.main.width, 110);
        // 右パネル: 約25% = 50
        assert_eq!(layout.right_top.width, 50);
        assert_eq!(layout.right_bottom.width, 50);

        // ステータスバーは1行
        assert_eq!(layout.status_bar.height, 1);

        // メインエリアの高さ = 全体 - ステータスバー
        assert_eq!(layout.left.height, 49);
        assert_eq!(layout.main.height, 49);
    }

    #[test]
    fn test_compute_layout_right_panel_split() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);

        // 右パネルは上部60% / 下部40%
        let right_total = layout.right_top.height + layout.right_bottom.height;
        assert_eq!(right_total, 49); // 全体高さ - ステータスバー

        // 上部が下部より大きい
        assert!(layout.right_top.height > layout.right_bottom.height);
    }

    #[test]
    fn test_compute_layout_small_terminal() {
        let area = Rect::new(0, 0, 80, 24);
        let layout = compute_layout(area);

        // 小さいターミナルでもクラッシュしない
        assert!(layout.left.width > 0);
        assert!(layout.main.width > 0);
        assert!(layout.right_top.width > 0);
        assert_eq!(layout.status_bar.height, 1);
    }

    #[test]
    fn test_hit_test_left_panel() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);
        assert_eq!(layout.hit_test(5, 10), Some(Panel::Left));
    }

    #[test]
    fn test_hit_test_main_panel() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);
        // main は left.width の右隣
        assert_eq!(layout.hit_test(layout.main.x + 5, 10), Some(Panel::Main));
    }

    #[test]
    fn test_hit_test_right_top() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);
        assert_eq!(
            layout.hit_test(layout.right_top.x + 5, layout.right_top.y + 1),
            Some(Panel::Right)
        );
    }

    #[test]
    fn test_hit_test_right_bottom() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);
        assert_eq!(
            layout.hit_test(layout.right_bottom.x + 5, layout.right_bottom.y + 1),
            Some(Panel::Terminal)
        );
    }

    #[test]
    fn test_hit_test_status_bar_returns_none() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);
        // ステータスバーはどのパネルにも属さない
        assert_eq!(layout.hit_test(50, layout.status_bar.y), None);
    }

    #[test]
    fn test_compute_layout_positions() {
        let area = Rect::new(0, 0, 200, 50);
        let layout = compute_layout(area);

        // 左パネルは x=0 から始まる
        assert_eq!(layout.left.x, 0);
        // 中央パネルは左パネルの右隣
        assert_eq!(layout.main.x, layout.left.width);
        // 右パネルは中央パネルの右隣
        assert_eq!(layout.right_top.x, layout.left.width + layout.main.width);
        // 右下は右上と同じ x 位置
        assert_eq!(layout.right_bottom.x, layout.right_top.x);
        // 右下は右上の下
        assert_eq!(layout.right_bottom.y, layout.right_top.y + layout.right_top.height);
    }
}
