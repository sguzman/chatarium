//! Pure local transcript-reader behavior over already-projected visible text.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    Paragraph(String),
    Heading {
        level: u8,
        text: String,
    },
    UnorderedList(Vec<String>),
    OrderedList(Vec<String>),
    BlockQuote(Vec<String>),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
}

pub fn parse_markdown(text: &str) -> Vec<MarkdownBlock> {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut lines = text.lines().peekable();

    let flush_paragraph = |blocks: &mut Vec<MarkdownBlock>, paragraph: &mut Vec<String>| {
        if !paragraph.is_empty() {
            blocks.push(MarkdownBlock::Paragraph(paragraph.join("\n")));
            paragraph.clear();
        }
    };

    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```") {
            flush_paragraph(&mut blocks, &mut paragraph);
            let language = line.trim_start().trim_start_matches('`').trim().to_owned();
            let language = (!language.is_empty()).then_some(language);
            let mut code = Vec::new();
            while let Some(code_line) = lines.next() {
                if code_line.trim() == "```" {
                    break;
                }
                code.push(code_line.to_owned());
            }
            blocks.push(MarkdownBlock::CodeBlock {
                language,
                code: code.join("\n"),
            });
            continue;
        }

        if line.trim().is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            continue;
        }

        if let Some((level, heading)) = heading(line) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::Heading {
                level,
                text: heading.to_owned(),
            });
            continue;
        }

        if is_unordered_item(line) {
            flush_paragraph(&mut blocks, &mut paragraph);
            let mut items = vec![list_item_text(line, false)];
            while let Some(next) = lines.peek().copied() {
                if !is_unordered_item(next) {
                    break;
                }
                items.push(list_item_text(lines.next().unwrap_or_default(), false));
            }
            blocks.push(MarkdownBlock::UnorderedList(items));
            continue;
        }

        if is_ordered_item(line) {
            flush_paragraph(&mut blocks, &mut paragraph);
            let mut items = vec![list_item_text(line, true)];
            while let Some(next) = lines.peek().copied() {
                if !is_ordered_item(next) {
                    break;
                }
                items.push(list_item_text(lines.next().unwrap_or_default(), true));
            }
            blocks.push(MarkdownBlock::OrderedList(items));
            continue;
        }

        if line.trim_start().starts_with('>') {
            flush_paragraph(&mut blocks, &mut paragraph);
            let mut quote = vec![line.trim_start().trim_start_matches('>').trim().to_owned()];
            while let Some(next) = lines.peek().copied() {
                if !next.trim_start().starts_with('>') {
                    break;
                }
                quote.push(
                    lines
                        .next()
                        .unwrap_or_default()
                        .trim_start()
                        .trim_start_matches('>')
                        .trim()
                        .to_owned(),
                );
            }
            blocks.push(MarkdownBlock::BlockQuote(quote));
            continue;
        }

        paragraph.push(line.to_owned());
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    blocks
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (1..=6)
        .contains(&level)
        .then(|| (level as u8, trimmed[level..].trim()))
}

fn is_unordered_item(line: &str) -> bool {
    line.trim_start().starts_with("- ") || line.trim_start().starts_with("* ")
}

fn is_ordered_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.find(". ").is_some_and(|index| {
        index > 0
            && trimmed[..index]
                .chars()
                .all(|character| character.is_ascii_digit())
    })
}

fn list_item_text(line: &str, ordered: bool) -> String {
    let trimmed = line.trim_start();
    if ordered {
        trimmed
            .find(". ")
            .map(|index| trimmed[index + 2..].to_owned())
            .unwrap_or_else(|| trimmed.to_owned())
    } else {
        trimmed[2..].to_owned()
    }
}

pub fn inline_segments(text: &str) -> Vec<(String, bool)> {
    let mut segments = Vec::new();
    let mut remaining = text;
    let mut code = false;
    while let Some(index) = remaining.find('`') {
        if index > 0 {
            segments.push((remaining[..index].to_owned(), code));
        }
        code = !code;
        remaining = &remaining[index + 1..];
    }
    if !remaining.is_empty() {
        segments.push((remaining.to_owned(), code));
    }
    segments
}

pub fn search_hits(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let folded = text.to_lowercase();
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(relative) = folded[from..].find(&query) {
        let start = from + relative;
        let end = start + query.len();
        hits.push((start, end));
        from = end;
    }
    hits
}

pub fn next_hit(current: Option<usize>, hit_count: usize, backwards: bool) -> Option<usize> {
    if hit_count == 0 {
        return None;
    }
    let current = current.unwrap_or(if backwards { 0 } else { hit_count - 1 });
    Some(if backwards {
        current.checked_sub(1).unwrap_or(hit_count - 1)
    } else {
        (current + 1) % hit_count
    })
}

pub fn copy_payload(text: &str) -> String {
    text.to_owned()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReaderPositionStore {
    positions: BTreeMap<String, f32>,
}

impl ReaderPositionStore {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        let mut positions = BTreeMap::new();
        if let Some(object) = value.get("positions").and_then(Value::as_object) {
            for (key, value) in object {
                if let Some(position) = value.as_f64().filter(|position| position.is_finite()) {
                    positions.insert(key.clone(), position.max(0.0) as f32);
                }
            }
        }
        Self { positions }
    }

    pub fn position(&self, key: &str) -> f32 {
        self.positions.get(key).copied().unwrap_or(0.0)
    }

    pub fn set_position(&mut self, key: impl Into<String>, position: f32) {
        self.positions.insert(key.into(), position.max(0.0));
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let value = json!({"schema": "chatarium-local-reader-state", "version": 1, "positions": self.positions});
        std::fs::write(path, serde_json::to_vec_pretty(&value)?)
    }
}

pub fn markdown_counts(texts: impl IntoIterator<Item = impl AsRef<str>>) -> (usize, usize) {
    texts
        .into_iter()
        .flat_map(|text| parse_markdown(text.as_ref()))
        .fold((0, 0), |(markdown, code), block| {
            (
                markdown + 1,
                code + usize::from(matches!(block, MarkdownBlock::CodeBlock { .. })),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reader_blocks_and_preserves_paragraph_lines() {
        let blocks = parse_markdown("# Heading\n\nfirst line\nsecond line\n\n- one\n- two\n\n1. first\n2. second\n\n> quote\n\n`inline`\n\n```rust\nlet x = 1;\n```");
        assert!(matches!(blocks[0], MarkdownBlock::Heading { level: 1, .. }));
        assert_eq!(
            blocks[1],
            MarkdownBlock::Paragraph("first line\nsecond line".to_owned())
        );
        assert!(matches!(blocks[2], MarkdownBlock::UnorderedList(_)));
        assert!(matches!(blocks[3], MarkdownBlock::OrderedList(_)));
        assert!(matches!(blocks[4], MarkdownBlock::BlockQuote(_)));
        assert!(matches!(blocks[6], MarkdownBlock::CodeBlock { .. }));
    }

    #[test]
    fn inline_code_and_search_hits_are_deterministic() {
        assert_eq!(
            inline_segments("before `code` after"),
            vec![
                ("before ".to_owned(), false),
                ("code".to_owned(), true),
                (" after".to_owned(), false)
            ]
        );
        assert_eq!(search_hits("Alpha alpha", "alpha"), vec![(0, 5), (6, 11)]);
        assert_eq!(next_hit(Some(1), 2, false), Some(0));
        assert_eq!(next_hit(Some(0), 2, true), Some(1));
    }

    #[test]
    fn position_store_is_conversation_scoped_and_copy_is_local() {
        let mut store = ReaderPositionStore::default();
        store.set_position("conversation-a", 42.0);
        store.set_position("conversation-b", 9.0);
        assert_eq!(store.position("conversation-a"), 42.0);
        assert_eq!(store.position("conversation-b"), 9.0);
        assert_eq!(
            copy_payload("synthetic visible text"),
            "synthetic visible text"
        );

        let path = std::env::temp_dir().join(format!(
            "chatarium-reader-state-test-{}.json",
            std::process::id()
        ));
        store.save(&path).expect("save local reader state");
        assert_eq!(ReaderPositionStore::load(&path), store);
        let _ = std::fs::remove_file(path);
    }
}
