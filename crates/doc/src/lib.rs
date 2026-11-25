mod docx;
mod markdown;

pub use docx::DocxParser;
pub use markdown::MarkdownParser;

use std::{
    path::{Path, PathBuf},
    str,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    pub title: String,
    pub source_path: PathBuf,
    pub blocks: Vec<Block>,
}

impl Document {
    pub fn contains_quote(&self, span: SourceSpan, quote: &str) -> bool {
        let quote = normalize_quote(quote);
        if span.start_line == 0 || span.start_line > span.end_line || quote.is_empty() {
            return false;
        }
        self.blocks.iter().any(|block| {
            let location = block.location;
            if location.page != span.page
                || span.end_line < location.start_line
                || span.start_line > location.end_line
            {
                return false;
            }
            let includes = |line| (span.start_line..=span.end_line).contains(&line);
            let covers_block = includes(location.start_line) && includes(location.end_line);
            let matches = |text: &str| normalize_quote(text).contains(&quote);
            match &block.content {
                BlockContent::Table { rows, row_lines } => {
                    rows.iter().enumerate().any(|(index, row)| {
                        let non_empty: Vec<_> = row
                            .iter()
                            .map(|cell| cell.trim())
                            .filter(|cell| !cell.is_empty())
                            .collect();
                        (covers_block || row_lines.get(index).is_some_and(|line| includes(*line)))
                            && (row.iter().any(|cell| matches(cell))
                                || matches(&row.join(" "))
                                || matches(&row.join(" | "))
                                || matches(&non_empty.join(" | ")))
                    })
                }
                BlockContent::Heading { text, .. }
                | BlockContent::Paragraph { text }
                | BlockContent::ListItem { text }
                | BlockContent::Code { text, .. } => {
                    covers_block && matches(text)
                        || text.lines().count() == location.end_line - location.start_line + 1
                            && matches(
                                &text
                                    .lines()
                                    .enumerate()
                                    .filter(|(index, _)| includes(location.start_line + index))
                                    .map(|(_, line)| line)
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            )
                }
            }
        })
    }
}

fn normalize_quote(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['«', '»', '“', '”', '„', '‟', '‹', '›'], "\"")
        .replace(['‘', '’', '‚', '‛'], "'")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct BlockId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceSpan {
    pub start_line: usize,
    pub end_line: usize,
    pub page: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Block {
    pub id: BlockId,
    pub location: SourceSpan,
    #[serde(flatten)]
    pub content: BlockContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockContent {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    ListItem {
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        row_lines: Vec<usize>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
}

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("Формат «{0}» пока не поддерживается")]
    Unsupported(String),
    #[error("Документ должен быть сохранён в кодировке UTF-8: {0}")]
    Encoding(#[from] str::Utf8Error),
    #[error("Не удалось разобрать DOCX: {0}")]
    InvalidDocx(String),
}

pub type Result<T> = std::result::Result<T, DocumentError>;

pub trait DocumentParser {
    fn supports(&self, path: &Path) -> bool;
    fn parse(&self, bytes: &[u8]) -> Result<Document>;
}

pub struct ParserRegistry {
    parsers: Vec<Box<dyn DocumentParser>>,
}

impl Default for ParserRegistry {
    fn default() -> Self {
        let mut registry = Self {
            parsers: Vec::new(),
        };
        registry.register(MarkdownParser);
        registry.register(PlainTextParser);
        registry.register(PdfParser);
        registry.register(DocxParser);
        registry
    }
}

impl ParserRegistry {
    pub fn register(&mut self, parser: impl DocumentParser + 'static) {
        self.parsers.push(Box::new(parser));
    }

    pub fn parse(&self, path: &Path, bytes: &[u8]) -> Result<Document> {
        let parser = self
            .parsers
            .iter()
            .find(|parser| parser.supports(path))
            .ok_or_else(|| {
                DocumentError::Unsupported(
                    path.extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                )
            })?;
        let mut document = parser.parse(bytes)?;
        document.source_path = path.into();
        if document.title.is_empty() {
            document.title = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        Ok(document)
    }
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|expected| ext.eq_ignore_ascii_case(expected))
        })
}

pub struct PlainTextParser;

impl DocumentParser for PlainTextParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["txt", "text"])
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let source = str::from_utf8(bytes)?;
        let mut blocks = Vec::new();
        let mut paragraph = Vec::new();
        for (index, line) in source.lines().chain(std::iter::once("")).enumerate() {
            if line.trim().is_empty() {
                if !paragraph.is_empty() {
                    blocks.push(Block {
                        id: BlockId(blocks.len() + 1),
                        location: SourceSpan {
                            start_line: index + 1 - paragraph.len(),
                            end_line: index,
                            page: None,
                        },
                        content: BlockContent::Paragraph {
                            text: paragraph.join("\n"),
                        },
                    });
                    paragraph.clear();
                }
            } else {
                paragraph.push(line);
            }
        }
        Ok(Document {
            title: String::new(),
            source_path: PathBuf::new(),
            blocks,
        })
    }
}

pub struct PdfParser;

impl DocumentParser for PdfParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["pdf"])
    }

    fn parse(&self, _bytes: &[u8]) -> Result<Document> {
        // TODO(issue): извлечь текст и диапазоны страниц из PDF.
        Err(DocumentError::Unsupported("PDF".into()))
    }
}
