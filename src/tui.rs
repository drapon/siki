use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, stdout};

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// ターミナルを初期化し、raw mode + alternate screen + mouse capture に切り替える
pub fn init() -> Result<Tui> {
    terminal::enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// ターミナルを元の状態に復元する
pub fn restore() -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

/// パニック時にターミナルを復元するフックを設定する
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // ターミナルを復元してからパニックメッセージを表示
        let _ = restore();
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_panic_hook_does_not_panic() {
        // パニックフックの設定自体がパニックしないことを確認
        // テスト環境では実際のターミナル操作はスキップ
        install_panic_hook();
    }
}
