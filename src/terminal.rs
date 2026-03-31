use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use tokio::sync::mpsc;

use crate::app::WorktreeId;
use crate::event::AppEvent;

/// PTY ベースのターミナルエミュレータ
///
/// portable-pty でシェルを起動し、vt100 パーサーで画面状態を管理する。
pub struct TerminalEmulator {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: vt100::Parser,
}

impl TerminalEmulator {
    /// 新しいターミナルエミュレータを作成する
    ///
    /// 指定されたシェルを worktree ディレクトリで起動し、
    /// PTY 出力を背景スレッドで読み取ってイベントチャネルに送信する。
    pub fn new(
        shell: &str,
        working_dir: &Path,
        size: (u16, u16),
        event_tx: mpsc::UnboundedSender<AppEvent>,
        worktree_id: WorktreeId,
        tab_index: usize,
    ) -> Result<Self> {
        Self::spawn_internal(shell, &[], working_dir, size, event_tx, worktree_id, tab_index, &[])
    }

    /// 引数付きでコマンドを起動するターミナルエミュレータを作成する
    pub fn with_args(
        program: &str,
        args: &[&str],
        working_dir: &Path,
        size: (u16, u16),
        event_tx: mpsc::UnboundedSender<AppEvent>,
        worktree_id: WorktreeId,
        tab_index: usize,
    ) -> Result<Self> {
        Self::spawn_internal(program, args, working_dir, size, event_tx, worktree_id, tab_index, &[])
    }

    /// 引数と環境変数付きでコマンドを起動する
    pub fn with_args_and_envs(
        program: &str,
        args: &[&str],
        working_dir: &Path,
        size: (u16, u16),
        event_tx: mpsc::UnboundedSender<AppEvent>,
        worktree_id: WorktreeId,
        tab_index: usize,
        envs: &[(&str, &str)],
    ) -> Result<Self> {
        Self::spawn_internal(program, args, working_dir, size, event_tx, worktree_id, tab_index, envs)
    }

    fn spawn_internal(
        program: &str,
        args: &[&str],
        working_dir: &Path,
        size: (u16, u16),
        event_tx: mpsc::UnboundedSender<AppEvent>,
        worktree_id: WorktreeId,
        tab_index: usize,
        envs: &[(&str, &str)],
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pty_size = PtySize {
            rows: size.1,
            cols: size.0,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system
            .openpty(pty_size)
            .map_err(|e| anyhow::anyhow!("PTY のオープンに失敗: {}", e))?;

        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(*arg);
        }
        cmd.cwd(working_dir);
        for (key, value) in envs {
            cmd.env(*key, *value);
        }

        pair.slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("シェルの起動に失敗: {}", e))?;

        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow::anyhow!("PTY writer の取得に失敗: {}", e))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow::anyhow!("PTY reader の取得に失敗: {}", e))?;

        // PTY からの読み取りを背景スレッドで行う
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if event_tx
                            .send(AppEvent::TerminalOutput {
                                worktree_id,
                                tab_index,
                                data: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let scrollback = if args.is_empty() { 0 } else { 5000 };
        let parser = vt100::Parser::new(size.1, size.0, scrollback);

        Ok(Self {
            master: pair.master,
            writer,
            parser,
        })
    }

    /// PTY にデータを書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer
            .write_all(data)
            .map_err(|e| anyhow::anyhow!("PTY への書き込みに失敗: {}", e))?;
        self.writer
            .flush()
            .map_err(|e| anyhow::anyhow!("PTY のフラッシュに失敗: {}", e))?;
        Ok(())
    }

    /// PTY とパーサーのサイズを変更する
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow::anyhow!("PTY のリサイズに失敗: {}", e))?;
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    /// PTY からのデータを VT100 パーサーに処理させる
    pub fn process(&mut self, data: &[u8]) {
        self.parser.process(data);
    }

    /// VT100 パーサーの画面状態を取得する
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// VT100 パーサーの画面状態を可変で取得する
    pub fn screen_mut(&mut self) -> &mut vt100::Screen {
        self.parser.screen_mut()
    }

    /// スクロールバックの位置を設定する（0 = 最新、大きいほど過去）
    pub fn set_scrollback(&mut self, offset: usize) {
        self.parser.screen_mut().set_scrollback(offset);
    }

    /// 現在のスクロールバック位置を取得する
    pub fn scrollback(&self) -> usize {
        self.parser.screen().scrollback()
    }
}

/// crossterm の KeyEvent をターミナルバイト列に変換する
pub fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            match c {
                'a'..='z' | 'A'..='Z' => {
                    // Ctrl+a = 0x01, Ctrl+z = 0x1a
                    vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
                }
                '\\' => vec![0x1c], // Ctrl+\ = FS
                ']' => vec![0x1d],  // Ctrl+] = GS
                '[' => vec![0x1b], // Ctrl+[ = ESC
                _ => vec![],
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // --- key_to_bytes テスト ---

    #[test]
    fn test_key_char() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('a'))), b"a");
        assert_eq!(key_to_bytes(&key(KeyCode::Char('Z'))), b"Z");
        assert_eq!(key_to_bytes(&key(KeyCode::Char('1'))), b"1");
    }

    #[test]
    fn test_key_unicode() {
        let bytes = key_to_bytes(&key(KeyCode::Char('あ')));
        assert_eq!(bytes, "あ".as_bytes());
    }

    #[test]
    fn test_key_enter() {
        assert_eq!(key_to_bytes(&key(KeyCode::Enter)), b"\r");
    }

    #[test]
    fn test_key_backspace() {
        assert_eq!(key_to_bytes(&key(KeyCode::Backspace)), vec![0x7f]);
    }

    #[test]
    fn test_key_esc() {
        assert_eq!(key_to_bytes(&key(KeyCode::Esc)), vec![0x1b]);
    }

    #[test]
    fn test_key_tab() {
        assert_eq!(key_to_bytes(&key(KeyCode::Tab)), b"\t");
    }

    #[test]
    fn test_key_arrows() {
        assert_eq!(key_to_bytes(&key(KeyCode::Up)), b"\x1b[A");
        assert_eq!(key_to_bytes(&key(KeyCode::Down)), b"\x1b[B");
        assert_eq!(key_to_bytes(&key(KeyCode::Right)), b"\x1b[C");
        assert_eq!(key_to_bytes(&key(KeyCode::Left)), b"\x1b[D");
    }

    #[test]
    fn test_key_home_end() {
        assert_eq!(key_to_bytes(&key(KeyCode::Home)), b"\x1b[H");
        assert_eq!(key_to_bytes(&key(KeyCode::End)), b"\x1b[F");
    }

    #[test]
    fn test_key_delete() {
        assert_eq!(key_to_bytes(&key(KeyCode::Delete)), b"\x1b[3~");
    }

    #[test]
    fn test_key_page_up_down() {
        assert_eq!(key_to_bytes(&key(KeyCode::PageUp)), b"\x1b[5~");
        assert_eq!(key_to_bytes(&key(KeyCode::PageDown)), b"\x1b[6~");
    }

    #[test]
    fn test_key_ctrl_c() {
        assert_eq!(key_to_bytes(&ctrl_key('c')), vec![0x03]);
    }

    #[test]
    fn test_key_ctrl_d() {
        assert_eq!(key_to_bytes(&ctrl_key('d')), vec![0x04]);
    }

    #[test]
    fn test_key_ctrl_z() {
        assert_eq!(key_to_bytes(&ctrl_key('z')), vec![0x1a]);
    }

    #[test]
    fn test_key_ctrl_a() {
        assert_eq!(key_to_bytes(&ctrl_key('a')), vec![0x01]);
    }

    #[test]
    fn test_key_ctrl_backslash() {
        assert_eq!(key_to_bytes(&ctrl_key('\\')), vec![0x1c]);
    }

    #[test]
    fn test_key_unknown_returns_empty() {
        assert_eq!(key_to_bytes(&key(KeyCode::F(1))), Vec::<u8>::new());
    }

    #[test]
    fn test_key_backtab() {
        assert_eq!(key_to_bytes(&key(KeyCode::BackTab)), b"\x1b[Z");
    }

    // --- vt100 パーサーテスト ---

    #[test]
    fn test_vt100_parser_basic() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Hello, World!");
        let screen = parser.screen();
        let row = screen.contents_between(0, 0, 0, 80);
        assert!(row.contains("Hello, World!"));
    }

    #[test]
    fn test_vt100_parser_newline() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"line1\r\nline2");
        let screen = parser.screen();
        let row0 = screen.contents_between(0, 0, 0, 80);
        let row1 = screen.contents_between(1, 0, 1, 80);
        assert!(row0.contains("line1"));
        assert!(row1.contains("line2"));
    }

    #[test]
    fn test_vt100_parser_ansi_colors() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        // ESC[31m = red, ESC[0m = reset
        parser.process(b"\x1b[31mRed text\x1b[0m Normal");
        let screen = parser.screen();
        let contents = screen.contents_between(0, 0, 0, 80);
        assert!(contents.contains("Red text"));
        assert!(contents.contains("Normal"));
    }

    #[test]
    fn test_vt100_parser_set_size() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.screen_mut().set_size(40, 120);
        // サイズ変更後もパーサーが正常動作する
        parser.process(b"After resize");
        let screen = parser.screen();
        let contents = screen.contents_between(0, 0, 0, 120);
        assert!(contents.contains("After resize"));
    }

    // --- PTY 統合テスト ---

    #[test]
    fn test_pty_spawn_and_output() {
        use crate::event::AppEvent;
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let dir = std::env::temp_dir();
        let wt_id = (0, 0);
        let tab = 0;

        let mut emu =
            TerminalEmulator::new("/bin/sh", &dir, (80, 24), tx, wt_id, tab)
                .expect("PTY の起動に成功するべき");

        // echo コマンドを送信
        emu.write(b"echo SIKI_TEST_OUTPUT\n")
            .expect("書き込みに成功するべき");

        // 出力イベントを受信するまで待機（最大2秒）
        let start = std::time::Instant::now();
        let mut received = false;
        while start.elapsed() < std::time::Duration::from_secs(2) {
            match rx.try_recv() {
                Ok(AppEvent::TerminalOutput { data, .. }) => {
                    emu.process(&data);
                    let contents = emu.screen().contents();
                    if contents.contains("SIKI_TEST_OUTPUT") {
                        received = true;
                        break;
                    }
                }
                _ => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        assert!(received, "PTY から echo の出力を受信するべき");
    }

    #[test]
    fn test_pty_resize() {
        use tokio::sync::mpsc;

        let (tx, _rx) = mpsc::unbounded_channel();
        let dir = std::env::temp_dir();

        let mut emu =
            TerminalEmulator::new("/bin/sh", &dir, (80, 24), tx, (0, 0), 0)
                .expect("PTY の起動に成功するべき");

        // リサイズがエラーなく完了する
        emu.resize(120, 40).expect("リサイズに成功するべき");
    }

    #[test]
    fn test_pty_write_and_process() {
        use crate::event::AppEvent;
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let dir = std::env::temp_dir();

        let mut emu =
            TerminalEmulator::new("/bin/sh", &dir, (80, 24), tx, (0, 0), 0)
                .expect("PTY の起動に成功するべき");

        // 文字を送信
        emu.write(b"A").expect("書き込みに成功するべき");

        // 何らかの出力イベントが来ることを確認
        let start = std::time::Instant::now();
        let mut got_output = false;
        while start.elapsed() < std::time::Duration::from_secs(2) {
            match rx.try_recv() {
                Ok(AppEvent::TerminalOutput { data, .. }) => {
                    emu.process(&data);
                    got_output = true;
                    break;
                }
                _ => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        assert!(got_output, "PTY から何らかの出力を受信するべき");
    }
}
