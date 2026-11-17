//! Markdown and code for the Ultimate preview, in the terminal's palette.
//!
//! tui-markdown renders prose with a palette-only style sheet. Its own highlighter always emits
//! 24-bit colour, so top-level fenced code blocks are split out and highlighted here with
//! syntect and bat's `ansi` theme, whose colours encode palette indices (alpha 0: red byte is the
//! index; alpha 1: the default colour). The syntax set loads once, lazily, on the preview thread.
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::sync::LazyLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{self, FontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
};

struct Syntax {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static SYNTAX: LazyLock<Syntax> = LazyLock::new(|| Syntax {
    syntaxes: SyntaxSet::load_defaults_newlines(),
    theme: ThemeSet::load_from_reader(&mut std::io::Cursor::new(include_str!(
        "../../assets/ansi.tmTheme"
    )))
    .expect("embedded ansi.tmTheme parses"),
});

/// Loads the syntax set now (for timing); otherwise the first code block does.
pub fn warm() {
    LazyLock::force(&SYNTAX);
}

#[derive(Clone, Copy)]
struct Sheet;

impl tui_markdown::StyleSheet for Sheet {
    fn heading(&self, _level: u8) -> Style {
        Style::new()
            .fg(Color::Indexed(3))
            .add_modifier(Modifier::BOLD)
    }
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
    fn code(&self) -> Style {
        Style::new().fg(Color::Indexed(6))
    }
    fn link(&self) -> Style {
        Style::new()
            .fg(Color::Indexed(4))
            .add_modifier(Modifier::UNDERLINED)
    }
    fn blockquote(&self) -> Style {
        Style::new()
            .fg(Color::Indexed(8))
            .add_modifier(Modifier::ITALIC)
    }
    fn code_block_fence(&self) -> &str {
        ""
    }
    fn list_marker(&self) -> Style {
        Style::new().fg(Color::Indexed(8))
    }
    fn table_header(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }
    fn table_border(&self) -> Style {
        Style::new().fg(Color::Indexed(8))
    }
}

fn colour(c: highlighting::Color) -> Option<Color> {
    match c.a {
        0 => Some(Color::Indexed(c.r)),
        1 => None,
        // A theme that is not palette-encoded: fall back to the default colour, never truecolor.
        _ => None,
    }
}

/// One line of code in palette colours; `None` when the language is unknown.
fn highlight(lang: &str, code: &str) -> Option<Vec<Line<'static>>> {
    let syntax = &*SYNTAX;
    let found = syntax.syntaxes.find_syntax_by_token(lang)?;
    let mut lines = HighlightLines::new(found, &syntax.theme);
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(code) {
        let parts = lines.highlight_line(line, &syntax.syntaxes).ok()?;
        out.push(Line::from(
            parts
                .into_iter()
                .map(|(style, text)| {
                    let mut out = Style::new();
                    if let Some(fg) = colour(style.foreground) {
                        out = out.fg(fg);
                    }
                    if style.font_style.contains(FontStyle::BOLD) {
                        out = out.add_modifier(Modifier::BOLD);
                    }
                    if style.font_style.contains(FontStyle::ITALIC) {
                        out = out.add_modifier(Modifier::ITALIC);
                    }
                    Span::styled(text.trim_end_matches(['\n', '\r']).to_owned(), out)
                })
                .collect::<Vec<_>>(),
        ));
    }
    Some(out)
}

/// A shell command as one highlighted line.
pub fn shell(command: &str) -> Vec<Span<'static>> {
    highlight("bash", command)
        .and_then(|lines| lines.into_iter().next())
        .map_or_else(|| vec![Span::raw(command.to_owned())], |line| line.spans)
}

fn prose(text: &str, out: &mut Vec<Line<'static>>) {
    if text.trim().is_empty() {
        return;
    }
    let options = tui_markdown::Options::new(Sheet);
    for line in tui_markdown::from_str_with_options(text, &options).lines {
        out.push(Line {
            style: line.style,
            alignment: line.alignment,
            spans: line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style))
                .collect(),
        });
    }
}

/// Markdown as lines: prose through tui-markdown, recognised fenced code highlighted.
pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut depth = 0usize;
    let mut fence: Option<(String, usize)> = None;
    let mut code = String::new();
    for (event, range) in Parser::new_ext(text, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) if depth == 0 => {
                fence = Some((
                    lang.split_whitespace().next().unwrap_or("").to_owned(),
                    range.start,
                ));
                code.clear();
            }
            Event::Text(part) if fence.is_some() => code.push_str(&part),
            Event::End(TagEnd::CodeBlock) if fence.is_some() => {
                let (lang, start) = fence.take().expect("checked");
                // An unknown language keeps tui-markdown's plain code style.
                if let Some(lines) = (!lang.is_empty())
                    .then(|| highlight(&lang, &code))
                    .flatten()
                {
                    prose(&text[from..start], &mut out);
                    if !out.is_empty() {
                        out.push(Line::default());
                    }
                    out.extend(lines);
                    from = range.end;
                }
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let rest = &text[from..];
    if from > 0 && !rest.trim().is_empty() {
        out.push(Line::default());
    }
    prose(rest, &mut out);
    out
}
