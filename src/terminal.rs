use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

use crate::app::WorktreeId;
use crate::event::AppEvent;

/// グローバルなターミナルID生成用カウンタ
static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

/// vt100 パーサーが検出した本物のベル (BEL) を数えるコールバック
///
/// 生バイト走査と異なり、OSC シーケンス終端の BEL（fish/zsh の
/// ウィンドウタイトル設定 `ESC]0;...BEL` 等）はパーサーが OSC として
/// 消費するため audible_bell は呼ばれず、誤検知しない。
#[derive(Default)]
struct BellCounter {
    bells: usize,
}

impl BellCounter {
    /// 蓄積したベル回数を取り出してリセットする
    fn take(&mut self) -> usize {
        std::mem::take(&mut self.bells)
    }
}

impl vt100::Callbacks for BellCounter {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells += 1;
    }
}

/// PTY ベースのターミナルエミュレータ
///
/// portable-pty でシェルを起動し、vt100 パーサーで画面状態を管理する。
pub struct TerminalEmulator {
    id: u64,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: vt100::Parser<BellCounter>,
    alive: bool,
    /// ベル検出時に外側ターミナルへ送る通知本文（発火元 worktree 名等）。
    /// 未設定ならデフォルト文言を使う。
    notify_label: Option<String>,
    /// 子プロセスハンドル。Drop 時に kill + wait してリーダースレッドの停止と
    /// ゾンビプロセス化の防止を行う。
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        // 子プロセスを止めないと PTY の reader thread (try_clone_reader で複製した
        // fd を保持) が永久に読み続け、event channel に stale tab_index を持つ
        // TerminalOutput が流れ続ける。kill するだけでは zombie が残るので
        // wait まで行う必要があるが、wait は同期ブロックするので別スレッドに逃がす。
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
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
        Self::spawn_internal(shell, &[], working_dir, size, event_tx, worktree_id, tab_index, &[], 5000)
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
        Self::spawn_internal(program, args, working_dir, size, event_tx, worktree_id, tab_index, &[], 5000)
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
        Self::spawn_internal(program, args, working_dir, size, event_tx, worktree_id, tab_index, envs, 5000)
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
        scrollback_lines: usize,
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

        let child = pair
            .slave
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

        let id = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);

        // PTY からの読み取りを背景スレッドで行う
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = event_tx.send(AppEvent::TerminalExited {
                            worktree_id,
                            tab_index,
                            terminal_id: id,
                        });
                        break;
                    }
                    Ok(n) => {
                        if event_tx
                            .send(AppEvent::TerminalOutput {
                                worktree_id,
                                tab_index,
                                terminal_id: id,
                                data: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = event_tx.send(AppEvent::TerminalExited {
                            worktree_id,
                            tab_index,
                            terminal_id: id,
                        });
                        break;
                    }
                }
            }
        });

        let parser = vt100::Parser::new_with_callbacks(
            size.1,
            size.0,
            scrollback_lines,
            BellCounter::default(),
        );

        Ok(Self {
            id,
            master: pair.master,
            writer,
            parser,
            alive: true,
            notify_label: None,
            child: Some(child),
        })
    }

    /// このターミナルエミュレータのユニーク ID を返す
    pub fn id(&self) -> u64 {
        self.id
    }

    /// ベル検出時に外側ターミナルへ送る通知本文を設定する
    pub fn set_notify_label(&mut self, label: impl Into<String>) {
        self.notify_label = Some(label.into());
    }

    /// PTY にデータを書き込む（大きなデータはチャンク分割）
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if !self.alive {
            return Ok(());
        }
        // 大きなペーストでPTYバッファが詰まりフリーズするのを防ぐため
        // チャンク分割して書き込む
        const CHUNK_SIZE: usize = 4096;
        for chunk in data.chunks(CHUNK_SIZE) {
            self.writer
                .write_all(chunk)
                .map_err(|e| anyhow::anyhow!("PTY への書き込みに失敗: {}", e))?;
            self.writer
                .flush()
                .map_err(|e| anyhow::anyhow!("PTY のフラッシュに失敗: {}", e))?;
        }
        Ok(())
    }

    /// PTY プロセスが終了したことをマークする
    pub fn mark_exited(&mut self) {
        self.alive = false;
    }

    /// PTY プロセスが生存しているか
    pub fn is_alive(&self) -> bool {
        self.alive
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
    ///
    /// 子プロセス (claude や make 等) が通知用に出したベルは vt100 パーサーに
    /// 吸収されて外側ターミナルに届かないため、検出したら siki 自身の stdout に
    /// 書き戻して外側ターミナル (tmux / iTerm 等) の通知を発火させる。
    pub fn process(&mut self, data: &[u8]) {
        self.parser.process(data);
        let bells = self.parser.callbacks_mut().take();
        if bells > 0 {
            forward_notification(bells, self.notify_label.as_deref());
        }
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

/// ベル検出時に外側ターミナルへ送るバイト列を組み立てる。
///
/// cmux は素の BEL ではデスクトップ通知を出さず OSC 777 を要求するため、
/// OSC 777 通知シーケンスを先頭に置く。OSC の終端には ST (ESC `\`) を用いる。
/// BEL 終端にすると終端 BEL が後続の互換用 BEL と区別できず、外側端末
/// (iTerm / tmux 等) で audible bell が 1 回余計に鳴るため。終端後に
/// 後方互換用の BEL を `count` 個だけ付与する。未知の OSC は対応しない端末では
/// 無視されるため安全。
fn notification_bytes(count: usize, label: Option<&str>) -> Vec<u8> {
    let title = "siki";
    // body は OSC のフィールド区切り `;` と制御文字を除去してから使う
    let body = label
        .map(sanitize_osc_field)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "セッションが通知を発しました".to_string());
    // OSC 777 通知本体（ST 終端）。body は ESC を除去済みのため、途中で ST を
    // 偽装して打ち切られることはない。
    let mut out = format!("\x1b]777;notify;{title};{body}\x1b\\").into_bytes();
    // iTerm/tmux 等は OSC 777 を解釈しないため、互換用に BEL を count 個送る。
    out.extend(std::iter::repeat_n(0x07, count));
    out
}

/// OSC フィールドに混ぜても安全なように、制御文字・行/段落区切り（[`crate::text`]
/// で共通化）に加え、OSC のフィールド区切り `;` も除去する。
///
/// ESC (0x1B) は制御文字として除去されるため、サニタイズ後の文字列で ST
/// (ESC `\`) を組み立てて OSC を途中終端することはできない（バックスラッシュ単体
/// が残っても ESC を伴わないため無害）。
fn sanitize_osc_field(s: &str) -> String {
    s.chars()
        .filter(|c| !crate::text::is_unsafe_text_char(*c) && *c != ';')
        .collect()
}

/// ベル通知を siki 自身の stdout に書き出し、外側ターミナルの通知を発火させる
///
/// OSC / BEL は表示にも端末状態にも影響しないため、ratatui の描画と交錯しても
/// 画面は壊れない。stdout の内部ロックにより描画の write とは直列化される。
/// stdout が端末でない場合（パイプ・リダイレクト時）は出力を汚染しないため
/// 転送しない。
fn forward_notification(count: usize, label: Option<&str>) {
    use std::io::IsTerminal;

    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let mut handle = stdout.lock();
    let _ = handle.write_all(&notification_bytes(count, label));
    let _ = handle.flush();
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

/// マウススクロールイベントを PTY が期待するエンコーディングのバイト列に変換する
///
/// `col` / `row` は PTY 内座標（1-based）。
/// エンコーディングは vt100::Screen の mouse_protocol_encoding に従う。
pub fn mouse_scroll_to_bytes(
    is_up: bool,
    col: u16,
    row: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let button: u8 = if is_up { 64 } else { 65 };
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            // SGR 形式: \x1b[<button;col;rowM
            format!("\x1b[<{};{};{}M", button, col, row).into_bytes()
        }
        _ => {
            // Default / Utf8 形式: \x1b[M<button+32><col+32><row+32>
            let mut bytes = b"\x1b[M".to_vec();
            bytes.push(button + 32);
            bytes.push((col.min(223) as u8) + 32);
            bytes.push((row.min(223) as u8) + 32);
            bytes
        }
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

    // --- BellCounter テスト ---

    #[test]
    fn test_bell_counter_counts_real_bell() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        parser.process(b"hello\x07world");
        assert_eq!(parser.callbacks().bells, 1);
    }

    #[test]
    fn test_bell_counter_ignores_osc_terminator() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        // fish/zsh のウィンドウタイトル設定 (OSC 0、BEL 終端) はベルではない
        parser.process(b"\x1b]0;my title\x07");
        assert_eq!(parser.callbacks().bells, 0);
    }

    #[test]
    fn test_bell_counter_ignores_osc_split_across_chunks() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        // OSC シーケンスが PTY 読み取りチャンク境界で分断されても誤検知しない
        parser.process(b"\x1b]0;my ti");
        parser.process(b"tle\x07");
        assert_eq!(parser.callbacks().bells, 0);
    }

    #[test]
    fn test_bell_counter_mixed_osc_and_real_bell() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        parser.process(b"\x1b]0;title\x07done\x07");
        assert_eq!(parser.callbacks().bells, 1);
    }

    #[test]
    fn test_bell_counter_counts_multiple_bells() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        parser.process(b"\x07a\x07b\x07");
        assert_eq!(parser.callbacks().bells, 3);
    }

    #[test]
    fn test_bell_counter_take_resets() {
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellCounter::default());
        parser.process(b"\x07");
        assert_eq!(parser.callbacks_mut().take(), 1);
        // take 後はリセットされ、次のチャンクの分だけ数える
        assert_eq!(parser.callbacks_mut().take(), 0);
        parser.process(b"\x07");
        assert_eq!(parser.callbacks_mut().take(), 1);
    }

    // --- 通知バイト列テスト ---

    #[test]
    fn test_notification_bytes_includes_osc777_st_and_bel() {
        let bytes = notification_bytes(1, Some("foo"));
        let s = String::from_utf8_lossy(&bytes);
        // cmux 向けの OSC 777 通知（タイトル siki、本文 foo、ST 終端 ESC \）
        assert!(s.starts_with("\x1b]777;notify;siki;foo\x1b\\"));
        // ST 終端後に iTerm/tmux 互換の BEL が count 個だけ付く（OSC 終端 BEL は無い）
        assert!(s.ends_with('\x07'));
        assert_eq!(bytes.iter().filter(|&&b| b == 0x07).count(), 1);
    }

    #[test]
    fn test_notification_bytes_default_body_when_none() {
        let bytes = notification_bytes(1, None);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("\x1b]777;notify;siki;セッションが通知を発しました\x1b\\"));
    }

    #[test]
    fn test_notification_bytes_default_body_when_empty_after_sanitize() {
        // サニタイズで空になるラベルはデフォルト文言にフォールバック
        let bytes = notification_bytes(1, Some(";;;"));
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("siki;セッションが通知を発しました\x1b\\"));
    }

    #[test]
    fn test_notification_bytes_appends_exactly_count_bels() {
        // BEL は互換用の count 個のみ（OSC は ST 終端のため終端 BEL は無い）
        assert_eq!(
            notification_bytes(3, Some("bar"))
                .iter()
                .filter(|&&b| b == 0x07)
                .count(),
            3
        );
        // count=0 のときは BEL を一切付けない
        assert_eq!(
            notification_bytes(0, Some("bar"))
                .iter()
                .filter(|&&b| b == 0x07)
                .count(),
            0
        );
    }

    #[test]
    fn test_sanitize_osc_field_strips_separators_and_control() {
        // `;`・ESC・BEL・LF・CR・U+2028(行区切り) を除去して OSC を壊さない
        assert_eq!(sanitize_osc_field("a;b\x1bc\x07d\ne\rf\u{2028}g"), "abcdefg");
    }

    #[test]
    fn test_notification_bytes_strips_line_separator_in_label() {
        // U+2028 を含むラベルでも OSC body に混入しない
        let bytes = notification_bytes(0, Some("wt\u{2028}name"));
        let s = String::from_utf8_lossy(&bytes);
        assert_eq!(s, "\x1b]777;notify;siki;wtname\x1b\\");
    }

    #[test]
    fn test_notification_bytes_label_with_control_chars_is_safe() {
        let bytes = notification_bytes(0, Some("my;\x1bwt\x07"));
        let s = String::from_utf8_lossy(&bytes);
        // OSC は title;body の 1 通知として ST 終端で閉じる（区切り `;` は 3 個のみ）
        assert_eq!(s, "\x1b]777;notify;siki;mywt\x1b\\");
        assert_eq!(s.matches(';').count(), 3);
        // 構造上の ESC は OSC 開始(1) と ST 終端(1) の計 2 個のみ。body から ESC が
        // 除去されるため、body 由来の ST 偽装による途中終端は成立しない。
        assert_eq!(bytes.iter().filter(|&&b| b == 0x1b).count(), 2);
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
