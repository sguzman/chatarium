//! Safe, display-only inline Markdown emphasis parser.
//! This never changes durable source text or model context.

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
}

fn push(out: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.bold == style.bold
            && last.italic == style.italic
            && last.strike == style.strike
            && last.code == style.code
        {
            last.text.push_str(text);
            return;
        }
    }
    out.push(Span {
        text: text.to_owned(),
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        code: style.code,
    });
}

fn append(input: &str, style: Style, depth: usize, out: &mut Vec<Span>) {
    if depth >= 8 {
        push(out, input, style);
        return;
    }
    let mut index = 0;
    while index < input.len() {
        let rest = &input[index..];
        if rest.starts_with('\\') {
            if let Some(next) = rest[1..].chars().next() {
                if matches!(next, '*' | '_' | '~' | '\u{60}' | '\\') {
                    push(out, &rest[1..1 + next.len_utf8()], style);
                    index += 1 + next.len_utf8();
                    continue;
                }
            }
        }
        let marker = if rest.starts_with("**") {
            Some(("**", "bold"))
        } else if rest.starts_with("__") {
            Some(("__", "bold"))
        } else if rest.starts_with("~~") {
            Some(("~~", "strike"))
        } else if rest.starts_with('\u{60}') {
            Some(("\u{60}", "code"))
        } else if rest.starts_with('*') {
            Some(("*", "italic"))
        } else if rest.starts_with('_') {
            Some(("_", "italic"))
        } else {
            None
        };
        if let Some((delimiter, kind)) = marker {
            if let Some(close) = rest[delimiter.len()..].find(delimiter) {
                let inner = &rest[delimiter.len()..delimiter.len() + close];
                if !inner.is_empty()
                    && !inner.chars().next().is_some_and(char::is_whitespace)
                    && !inner.chars().last().is_some_and(char::is_whitespace)
                {
                    let mut changed = style;
                    match kind {
                        "bold" => changed.bold = true,
                        "italic" => changed.italic = true,
                        "strike" => changed.strike = true,
                        "code" => changed.code = true,
                        _ => {}
                    }
                    if kind == "code" {
                        push(out, inner, changed);
                    } else {
                        append(inner, changed, depth + 1, out);
                    }
                    index += delimiter.len() * 2 + close;
                    continue;
                }
            }
        }
        let first = rest.chars().next().expect("non-empty remainder");
        push(out, &rest[..first.len_utf8()], style);
        index += first.len_utf8();
    }
}

#[must_use]
pub fn parse(input: &str) -> Vec<Span> {
    let mut result = Vec::new();
    append(input, Style::default(), 0, &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_valid_markers_and_preserves_literal_unmatched_text() {
        let spans = parse("Two **tools** and *more*, ~~old~~ plus \u{60}code\u{60}");
        let plain = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(plain, "Two tools and more, old plus code");
        assert!(spans.iter().any(|s| s.text == "tools" && s.bold));
        assert!(spans.iter().any(|s| s.text == "more" && s.italic));
        assert!(spans.iter().any(|s| s.text == "old" && s.strike));
        assert!(spans.iter().any(|s| s.text == "code" && s.code));
        assert_eq!(
            parse("literal **never closed")[0].text,
            "literal **never closed"
        );
    }

    #[test]
    fn bold_timestamp_regression_from_live_agent_reply() {
        let input = "The reported time was **October 13, 2026, at 22:05:57.806 UTC**.";
        let spans = parse(input);
        assert!(spans.iter().any(|span| {
            span.text == "October 13, 2026, at 22:05:57.806 UTC" && span.bold
        }));
        assert_eq!(
            spans.iter().map(|span| span.text.as_str()).collect::<String>(),
            "The reported time was October 13, 2026, at 22:05:57.806 UTC."
        );
    }

    #[test]
    fn unicode_nested_emphasis_and_escaping_are_safe() {
        let spans = parse("🪐 **bold *italic* 中文** \\*literal\\*");
        assert!(
            spans
                .iter()
                .any(|s| s.text == "italic" && s.bold && s.italic)
        );
        let plain = spans.into_iter().map(|span| span.text).collect::<String>();
        assert_eq!(plain, "🪐 bold italic 中文 *literal*");
        assert_eq!(parse(""), Vec::<Span>::new());
    }
}
