//! 対話セレクタ（TASK-0006 で実装）。
//!
//! crossterm（既存依存）ベースの `select_one` / `input_line` / `confirm` を提供予定。
//! raw mode は内部で取得・解除し、端末状態を残さない。
