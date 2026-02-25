use std::path::Path;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// ハイライト済みスパン (R, G, B, テキスト)
pub type HighlightedSpan = (u8, u8, u8, String);

struct SyntaxResources {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

static RESOURCES: OnceLock<SyntaxResources> = OnceLock::new();

fn resources() -> &'static SyntaxResources {
    RESOURCES.get_or_init(|| SyntaxResources {
        syntax_set: SyntaxSet::load_defaults_newlines(),
        theme_set: ThemeSet::load_defaults(),
    })
}

/// ファイル内容をシンタックスハイライトして行ごとのスパンリストを返す。
/// 結果はキャッシュ用にプリミティブ型で返す。
pub fn highlight(content: &str, path: &Path) -> Vec<Vec<HighlightedSpan>> {
    let res = resources();
    let ss = &res.syntax_set;

    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|name| ss.find_syntax_by_extension(name))
        })
        .or_else(|| {
            content
                .lines()
                .next()
                .and_then(|first| ss.find_syntax_by_first_line(first))
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let theme = &res.theme_set.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);

    content
        .lines()
        .map(|line| match h.highlight_line(line, ss) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, text)| {
                    (
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                        text.to_string(),
                    )
                })
                .collect(),
            Err(_) => vec![(192, 192, 192, line.to_string())],
        })
        .collect()
}
