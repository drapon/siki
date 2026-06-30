//! CLI 引数スキャナ。
//!
//! 位置引数 / 値フラグ（`--base <ref>` または `--base=<ref>`）/ 真偽フラグ（`--resume`）/
//! `--` 以降の passthrough を 1 箇所で仕分ける 0 依存ヘルパ。
//! `--` 丸投げや値フラグの検証ロジックをここに集約し、各サブコマンドからは宣言的に使う。
//!
//! サブコマンドは許可する値フラグ・真偽フラグの集合を明示的に渡す。集合に無い `--xxx` は
//! エラーにするため、フラグ名のタイポ（`--reume` 等）を黙って positional に落とさず弾ける。

// TASK-0003〜0007 で各サブコマンドから利用するまでの暫定（TASK-0007 で除去）。
#![allow(dead_code)]

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

/// 仕分け済みの引数。フラグ名は `--` 接頭辞込みで保持する（例: `value("--base")`）。
pub struct ArgScan {
    positionals: Vec<String>,
    flags: HashSet<String>,
    values: HashMap<String, String>,
    rest: Vec<String>,
}

impl ArgScan {
    /// `value_keys`（例: `["--base"]`）は次トークンを値として取るフラグ、
    /// `bool_keys`（例: `["--resume"]`）は値を取らない真偽フラグ。
    /// どちらにも無い `--xxx` や単一ハイフンの未知トークン（`-x`）はエラー。
    /// `--` 以降は全て rest（passthrough）。
    pub fn parse(args: &[String], value_keys: &[&str], bool_keys: &[&str]) -> Result<Self> {
        let mut positionals = Vec::new();
        let mut flags = HashSet::new();
        let mut values = HashMap::new();
        let mut rest = Vec::new();

        let mut i = 0;
        while i < args.len() {
            let tok = &args[i];

            if tok == "--" {
                rest.extend_from_slice(&args[i + 1..]);
                break;
            }

            if let Some(stripped) = tok.strip_prefix("--") {
                // `--key=value` 形式に対応
                if let Some(eq) = stripped.find('=') {
                    let key = format!("--{}", &stripped[..eq]);
                    let val = stripped[eq + 1..].to_string();
                    if !value_keys.contains(&key.as_str()) {
                        bail!("不明な値フラグ: {}", key);
                    }
                    values.insert(key, val);
                    i += 1;
                    continue;
                }

                if value_keys.contains(&tok.as_str()) {
                    // `--key value` 形式。次が別フラグ（`--` / `--xxx`）なら値欠落として弾く
                    // （`--base --resume` が --resume を黙って飲み込む事故を防ぐ）。
                    let val = match args.get(i + 1) {
                        Some(v) if v == "--" || v.starts_with("--") => {
                            bail!("{} の値が指定されていません（次が {} です）", tok, v)
                        }
                        Some(v) => v.clone(),
                        None => bail!("{} には値が必要です", tok),
                    };
                    values.insert(tok.clone(), val);
                    i += 2;
                    continue;
                }

                if bool_keys.contains(&tok.as_str()) {
                    flags.insert(tok.clone());
                    i += 1;
                    continue;
                }

                bail!("不明なフラグ: {}", tok);
            }

            if tok.starts_with('-') && tok.len() > 1 {
                bail!("不明なフラグ: {}", tok);
            }

            positionals.push(tok.clone());
            i += 1;
        }

        Ok(Self {
            positionals,
            flags,
            values,
            rest,
        })
    }

    /// 位置引数をちょうど `n` 個取り出す。過不足はエラー。
    pub fn positionals(&self, n: usize) -> Result<&[String]> {
        if self.positionals.len() != n {
            bail!(
                "位置引数の数が不正です（期待: {}, 実際: {}）",
                n,
                self.positionals.len()
            );
        }
        Ok(&self.positionals)
    }

    /// 位置引数を最大 `max` 個まで許容して返す（対話フォールバック用に不足を許す）。
    pub fn positionals_opt(&self, max: usize) -> Result<&[String]> {
        if self.positionals.len() > max {
            bail!(
                "位置引数が多すぎます（最大: {}, 実際: {}）",
                max,
                self.positionals.len()
            );
        }
        Ok(&self.positionals)
    }

    /// 値フラグの値を取得。
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    /// 真偽フラグの有無。
    pub fn has(&self, key: &str) -> bool {
        self.flags.contains(key)
    }

    /// `--` 以降の passthrough。
    pub fn rest(&self) -> &[String] {
        &self.rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn positionals_only() {
        let s = ArgScan::parse(&v(&["myapp", "feature"]), &[], &[]).unwrap();
        assert_eq!(s.positionals(2).unwrap(), &v(&["myapp", "feature"])[..]);
        assert!(s.rest().is_empty());
    }

    #[test]
    fn value_flag_space_form() {
        let s = ArgScan::parse(&v(&["myapp", "--base", "origin/dev"]), &["--base"], &[]).unwrap();
        assert_eq!(s.value("--base"), Some("origin/dev"));
        assert_eq!(s.positionals_opt(2).unwrap(), &v(&["myapp"])[..]);
    }

    #[test]
    fn value_flag_equals_form() {
        let s = ArgScan::parse(&v(&["myapp", "--base=origin/dev"]), &["--base"], &[]).unwrap();
        assert_eq!(s.value("--base"), Some("origin/dev"));
        assert_eq!(s.positionals_opt(2).unwrap(), &v(&["myapp"])[..]);
    }

    #[test]
    fn bool_flag() {
        let s = ArgScan::parse(&v(&["--resume", "myapp", "wt"]), &[], &["--resume"]).unwrap();
        assert!(s.has("--resume"));
        assert_eq!(s.positionals(2).unwrap(), &v(&["myapp", "wt"])[..]);
    }

    #[test]
    fn passthrough_after_dashdash() {
        let s = ArgScan::parse(&v(&["myapp", "wt", "--", "--model", "opus"]), &[], &[]).unwrap();
        assert_eq!(s.rest(), &v(&["--model", "opus"])[..]);
        assert_eq!(s.positionals(2).unwrap(), &v(&["myapp", "wt"])[..]);
    }

    #[test]
    fn value_flag_then_passthrough() {
        let s = ArgScan::parse(
            &v(&["myapp", "wt", "--base", "origin/dev", "--resume", "--", "-r", "x"]),
            &["--base"],
            &["--resume"],
        )
        .unwrap();
        assert_eq!(s.value("--base"), Some("origin/dev"));
        assert!(s.has("--resume"));
        assert_eq!(s.rest(), &v(&["-r", "x"])[..]);
        assert_eq!(s.positionals(2).unwrap(), &v(&["myapp", "wt"])[..]);
    }

    #[test]
    fn err_missing_value() {
        assert!(ArgScan::parse(&v(&["myapp", "--base"]), &["--base"], &[]).is_err());
    }

    #[test]
    fn err_value_flag_swallows_next_flag() {
        // W1: `--base --resume` は --resume を値にせず、値欠落エラーにする
        assert!(ArgScan::parse(&v(&["--base", "--resume"]), &["--base"], &["--resume"]).is_err());
        // `--base --` も passthrough を飲み込まず値欠落エラー
        assert!(ArgScan::parse(&v(&["wt", "--base", "--", "x"]), &["--base"], &[]).is_err());
    }

    #[test]
    fn err_positionals_count() {
        let s = ArgScan::parse(&v(&["myapp"]), &[], &[]).unwrap();
        assert!(s.positionals(2).is_err());
    }

    #[test]
    fn err_unknown_short_flag() {
        assert!(ArgScan::parse(&v(&["-x", "myapp"]), &[], &[]).is_err());
    }

    #[test]
    fn err_unknown_long_flag() {
        // W2: 未知の長フラグは空白形でもエラー（タイポを positional に落とさない）
        assert!(ArgScan::parse(&v(&["--reume", "myapp"]), &[], &["--resume"]).is_err());
    }

    #[test]
    fn err_unknown_value_flag_equals() {
        // 値フラグでないキーに `=` を付けた場合はエラー
        assert!(ArgScan::parse(&v(&["--foo=bar"]), &["--base"], &[]).is_err());
    }
}
