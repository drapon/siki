//! TUI を起動せずに worktree とセッションを操作する CLI サブコマンド群。
//!
//! 各サブコマンドは TUI 状態に依存せず、`config` / `git` / `hooks` の純粋関数を
//! 薄く呼び出す。`main.rs` のディスパッチから `cli::cmd_*(&args[2..])` で呼ばれる。
//!
//! 引数不足時は run / new / rm が対話セレクタ（[`prompt`]）で補完する 2 モード方式。
//! `path` / `list` はスクリプト連携のため非対話。

// 後続タスク（TASK-0002〜0007）で各コマンドを実装・結線するまでの暫定。
#![allow(dead_code)]

pub mod args;
pub mod prompt;

use anyhow::Result;

/// `siki new <project> <name> [--base <ref>]`
pub fn cmd_new(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0003")
}

/// `siki rm <project> <name>`
pub fn cmd_rm(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004 / TASK-0007")
}

/// `siki path <project> <name>`
pub fn cmd_path(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004")
}

/// `siki list [project]`
pub fn cmd_list(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0004")
}

/// `siki run <project> <name> [--base <ref>] [--resume] [-- <llm args>]`
pub fn cmd_run(_args: &[String]) -> Result<()> {
    unimplemented!("TASK-0005 / TASK-0007")
}
