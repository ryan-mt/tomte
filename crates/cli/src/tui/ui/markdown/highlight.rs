use super::*;

/// Lazily-loaded syntect assets (syntax definitions + a dark theme). Loading
/// the default sets parses an embedded binary dump, so do it once per process.
pub(crate) fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme)
{
    use std::sync::OnceLock;
    static ASSETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        OnceLock::new();
    ASSETS.get_or_init(|| {
        let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = ts.themes["base16-ocean.dark"].clone();
        (ss, theme)
    })
}

/// Background fill behind a fenced code block, so it reads as one solid panel.
pub(crate) const CODE_BG: Color = palette::SURFACE_CODE;

/// Resolve a fenced-code language token to a syntect syntax. syntect matches by
/// file extension or exact name, so common fence labels (`rust`, `python`,
/// `bash`, …) miss unless mapped to the extension first; we then fall back to
/// the raw/lower-cased token and finally plain text.
pub(crate) fn resolve_syntax<'a>(
    ss: &'a syntect::parsing::SyntaxSet,
    lang: &str,
) -> &'a syntect::parsing::SyntaxReference {
    let lower = lang.to_ascii_lowercase();
    let token = match lower.as_str() {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" | "node" | "mjs" => "js",
        "typescript" | "ts" => "ts",
        "jsx" | "tsx" => "tsx",
        "bash" | "sh" | "shell" | "zsh" | "console" => "sh",
        "yaml" | "yml" => "yaml",
        "markdown" | "md" => "md",
        "go" | "golang" => "go",
        "c++" | "cpp" | "cxx" | "cc" => "cpp",
        "c#" | "csharp" | "cs" => "cs",
        "ruby" | "rb" => "rb",
        "kotlin" | "kt" => "kt",
        "rust-script" => "rs",
        other => other,
    };
    ss.find_syntax_by_token(token)
        .or_else(|| ss.find_syntax_by_token(&lower))
        .or_else(|| ss.find_syntax_by_extension(&lower))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Syntax-highlight `code` and return content rows padded to `content_width`
/// with the code background. `lang` is the fence's language token, if any.
pub(crate) fn highlight_code_lines(
    code: &str,
    lang: Option<&str>,
    content_width: usize,
) -> Vec<Vec<Span<'static>>> {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;
    let (ss, theme) = syntax_assets();
    let syntax = lang
        .map(|l| resolve_syntax(ss, l))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);

    // Sanitize once up front (preserving newlines) so embedded tabs/escapes in a
    // model-echoed code block can't desync the terminal; syntect then highlights
    // the cleaned text.
    let code = sanitize_display(code);
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    for line in LinesWithEndings::from(code.as_ref()) {
        let ranges = hl.highlight_line(line, ss).unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (style, piece) in ranges {
            let piece = piece.trim_end_matches(['\n', '\r']);
            if piece.is_empty() {
                continue;
            }
            let c = style.foreground;
            spans.push(Span::styled(
                piece.to_string(),
                Style::default().fg(Color::Rgb(c.r, c.g, c.b)).bg(CODE_BG),
            ));
        }
        out.extend(wrap_spans(spans, content_width, CODE_BG));
    }
    if out.is_empty() {
        out.push(wrap_spans(Vec::new(), content_width, CODE_BG).remove(0));
    }
    out
}
