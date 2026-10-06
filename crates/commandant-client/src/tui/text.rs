//! Turning the agent's text into styled lines: a little markdown, and
//! wrapping that keeps each line's gutter.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const CODE: Color = Color::Indexed(180);
/// Secondary text. A fixed grey, since some terminal palettes make dark gray
/// too dark to read.
pub const MUTED: Color = Color::Indexed(245);
const HEADING: Color = Color::Indexed(117);

/// Styles the markdown the agent writes, line by line: headings, lists,
/// quotes, rules, fenced code, and inline `code` and **bold**.
pub fn markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            let language = trimmed.trim_start_matches('`').trim();
            if in_code && !language.is_empty() {
                lines.push(Line::from(language.to_string()).fg(MUTED).italic());
            }
            continue;
        }
        if in_code {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::new().fg(CODE),
            )));
            continue;
        }
        lines.push(block(line, trimmed));
    }
    lines
}

fn block(line: &str, trimmed: &str) -> Line<'static> {
    let indent = &line[..line.len() - trimmed.len()];
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        let style = Style::new().fg(HEADING).add_modifier(Modifier::BOLD);
        return Line::from(inline(trimmed[hashes..].trim(), style));
    }
    if is_rule(trimmed) {
        return Line::from("─".repeat(24)).fg(MUTED);
    }
    if let Some(quote) = trimmed.strip_prefix('>') {
        let mut spans = vec![Span::raw("│ ").fg(MUTED)];
        spans.extend(inline(quote.trim_start(), Style::new().italic().fg(MUTED)));
        return Line::from(spans);
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(item) = trimmed.strip_prefix(bullet) {
            let mut spans = vec![Span::raw(format!("{indent}• ")).fg(MUTED)];
            spans.extend(inline(item, Style::new()));
            return Line::from(spans);
        }
    }
    Line::from(inline(line, Style::new()))
}

fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|c| line.chars().all(|x| x.to_string() == *c))
}

/// Splits a line into plain, `code` and **bold** spans. Unclosed markers
/// stay as they are.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (marker, style) = if rest.starts_with('`') {
            ("`", base.fg(CODE))
        } else if rest.starts_with("**") {
            ("**", base.add_modifier(Modifier::BOLD))
        } else {
            let c = rest.chars().next().expect("rest isn't empty");
            plain.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        };
        let body = &rest[marker.len()..];
        match body.find(marker).filter(|&end| end > 0) {
            Some(end) => {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base));
                }
                spans.push(Span::styled(body[..end].to_string(), style));
                rest = &body[end + marker.len()..];
            }
            None => {
                plain.push_str(marker);
                rest = body;
            }
        }
    }
    if !plain.is_empty() || spans.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// Wraps `line` at word boundaries to `width` columns. The first row starts
/// with `first`, the others with `rest`; both should be as wide.
pub fn wrap(
    line: Line<'static>,
    width: usize,
    first: &Span<'static>,
    rest: &Span<'static>,
) -> Vec<Line<'static>> {
    let gutter = first.width();
    let room = width.saturating_sub(gutter).max(1);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (word, style) in words(&line) {
        let space = word.starts_with(char::is_whitespace);
        let w = word.width();
        if used + w > room && used > 0 {
            rows.push(trimmed(std::mem::take(&mut row)));
            used = 0;
            if space {
                continue;
            }
        }
        if w <= room {
            used += w;
            row.push(Span::styled(word, style));
            continue;
        }
        // Longer than a whole row: split it anywhere.
        let mut piece = String::new();
        for c in word.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > room {
                row.push(Span::styled(std::mem::take(&mut piece), style));
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            piece.push(c);
            used += cw;
        }
        row.push(Span::styled(piece, style));
    }
    rows.push(row);
    rows.into_iter()
        .enumerate()
        .map(|(i, spans)| {
            let gutter = if i == 0 { first } else { rest };
            Line::from(
                std::iter::once(gutter.clone())
                    .chain(spans)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// A row without the spaces it broke at.
fn trimmed(mut row: Vec<Span<'static>>) -> Vec<Span<'static>> {
    while row.last().is_some_and(|s| s.content.trim().is_empty()) {
        row.pop();
    }
    row
}

/// The line's text as runs of spaces and runs of anything else, each with
/// its style.
fn words(line: &Line<'static>) -> Vec<(String, Style)> {
    let mut words = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        let mut word = String::new();
        let mut space = None;
        for c in span.content.chars() {
            let is_space = c.is_whitespace();
            if space.is_some_and(|s| s != is_space) {
                words.push((std::mem::take(&mut word), style));
            }
            space = Some(is_space);
            word.push(c);
        }
        if !word.is_empty() {
            words.push((word, style));
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn styles_markdown() {
        let lines = markdown(
            "# Title\nsome `code` and **bold** and **open\n- item\n```rust\nlet x = 1;\n```",
        );
        let texts: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "Title",
                "some code and bold and **open",
                "• item",
                "rust",
                "let x = 1;"
            ]
        );
        let spans = &lines[1].spans;
        assert_eq!(spans[1].style.fg, Some(CODE));
        assert!(spans[3].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(lines[4].spans[0].style.fg, Some(CODE));
    }

    #[test]
    fn wraps_words_behind_the_gutter() {
        let first = Span::raw("▌ ");
        let rest = Span::raw("  ");
        let rows = wrap(Line::from("one two three fourfivesix"), 10, &first, &rest);
        let texts: Vec<_> = rows.iter().map(text).collect();
        assert_eq!(texts, ["▌ one two", "  three", "  fourfive", "  six"]);
        assert_eq!(wrap(Line::default(), 10, &first, &rest).len(), 1);
    }
}
