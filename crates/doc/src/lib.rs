mod markdown;

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

pub struct DocxParser;

impl DocumentParser for DocxParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["docx"])
    }

    fn parse(&self, _bytes: &[u8]) -> Result<Document> {
        // TODO(issue): разобрать абзацы, списки и таблицы DOCX.
        Err(DocumentError::Unsupported("DOCX".into()))
    }
}
