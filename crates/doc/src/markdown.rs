use std::{
    ops::Range,
    path::{Path, PathBuf},
    str,
};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::{
    Block, BlockContent, BlockId, Document, DocumentParser, Result, SourceSpan, has_extension,
};

pub struct MarkdownParser;

impl DocumentParser for MarkdownParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["md", "markdown"])
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let source = str::from_utf8(bytes)?;
        let newlines: Vec<_> = source
            .match_indices('\n')
            .map(|(offset, _)| offset)
            .collect();
        let line_at = |offset| newlines.partition_point(|newline| *newline < offset) + 1;
        let mut pending = Vec::new();
        let mut completed = Vec::new();
        for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
            match event {
                Event::Start(tag) => {
                    let line = line_at(range.start);
                    start(tag, range, line, &mut pending);
                }
                Event::End(end) => finish(end, &mut pending, &mut completed),
                Event::Text(text) | Event::Code(text) => push_text(&mut pending, &text),
                Event::SoftBreak | Event::HardBreak => push_text(&mut pending, "\n"),
                _ => {}
            }
        }

        completed.sort_by_key(|(range, _)| range.start);
        let blocks: Vec<_> = completed
            .into_iter()
            .enumerate()
            .map(|(index, (range, content))| {
                let trimmed_end = source[range.clone()].trim_end().len();
                Block {
                    id: BlockId(index + 1),
                    location: SourceSpan {
                        start_line: line_at(range.start),
                        end_line: line_at(range.start + trimmed_end.saturating_sub(1)),
                        page: None,
                    },
                    content,
                }
            })
            .collect();
        let title = blocks
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Heading { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Ok(Document {
            title,
            source_path: PathBuf::new(),
            blocks,
        })
    }
}

enum Pending {
    Block {
        end: TagEnd,
        range: Range<usize>,
        content: BlockContent,
    },
    Row {
        line: usize,
        cells: Vec<String>,
    },
    Cell(String),
}

fn start(tag: Tag<'_>, range: Range<usize>, line: usize, pending: &mut Vec<Pending>) {
    let end = tag.to_end();
    let content = match tag {
        Tag::Heading { level, .. } => BlockContent::Heading {
            level: level as u8,
            text: String::new(),
        },
        Tag::Paragraph => {
            if matches!(
                pending.last_mut(),
                Some(Pending::Block {
                    content: BlockContent::ListItem { .. },
                    ..
                })
            ) {
                push_text(pending, "\n");
                return;
            }
            BlockContent::Paragraph {
                text: String::new(),
            }
        }
        Tag::Item => BlockContent::ListItem {
            text: String::new(),
        },
        Tag::CodeBlock(kind) => BlockContent::Code {
            language: match kind {
                CodeBlockKind::Fenced(info) if !info.is_empty() => {
                    info.split_whitespace().next().map(str::to_owned)
                }
                _ => None,
            },
            text: String::new(),
        },
        Tag::Table(_) => BlockContent::Table {
            rows: Vec::new(),
            row_lines: Vec::new(),
        },
        Tag::TableHead | Tag::TableRow => {
            pending.push(Pending::Row {
                line,
                cells: Vec::new(),
            });
            return;
        }
        Tag::TableCell => {
            pending.push(Pending::Cell(String::new()));
            return;
        }
        _ => return,
    };
    pending.push(Pending::Block {
        end,
        range,
        content,
    });
}

fn finish(
    end: TagEnd,
    pending: &mut Vec<Pending>,
    completed: &mut Vec<(Range<usize>, BlockContent)>,
) {
    let matches = match pending.last() {
        Some(Pending::Block { end: expected, .. }) => end == *expected,
        Some(Pending::Row { .. }) => matches!(end, TagEnd::TableHead | TagEnd::TableRow),
        Some(Pending::Cell(_)) => end == TagEnd::TableCell,
        None => false,
    };
    if !matches {
        return;
    }

    match pending.pop().expect("верхний элемент проверен") {
        Pending::Block {
            range, mut content, ..
        } => {
            if let BlockContent::ListItem { text } = &mut content {
                *text = text.trim().into();
            }
            completed.push((range, content));
        }
        Pending::Row { line, cells } => {
            if let Some(Pending::Block {
                content: BlockContent::Table { rows, row_lines },
                ..
            }) = pending.last_mut()
            {
                rows.push(cells);
                row_lines.push(line);
            }
        }
        Pending::Cell(cell) => {
            if let Some(Pending::Row { cells, .. }) = pending.last_mut() {
                cells.push(cell);
            }
        }
    }
}

fn push_text(pending: &mut [Pending], value: &str) {
    match pending.last_mut() {
        Some(Pending::Cell(text))
        | Some(Pending::Block {
            content:
                BlockContent::Heading { text, .. }
                | BlockContent::Paragraph { text }
                | BlockContent::ListItem { text }
                | BlockContent::Code { text, .. },
            ..
        }) => text.push_str(value),
        _ => {}
    }
}
