use doc::{Block, BlockContent, Document};
use llm::generate_typed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{Evidence, RunContext, StageError, pipeline::Result, stages::has_cyrillic};

const MAX_HEADINGS: usize = 40;
const MAX_PREVIEW_BLOCKS: usize = 12;
const MAX_BLOCK_CHARS: usize = 600;
const MAX_TABLE_ROWS: usize = 6;
const MAX_CELL_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Requirements,
    NotRequirements,
    Stub,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TriageDecision {
    pub kind: DocumentKind,
    pub reason: String,
    pub evidence: Evidence,
}

#[derive(Serialize)]
struct TriageInput<'a> {
    title: &'a str,
    headings: Vec<Block>,
    preview_blocks: Vec<Block>,
    total_blocks: usize,
}

pub(crate) async fn run(ctx: &mut RunContext<'_>, prompt: &str) -> Result<()> {
    if !ctx.document.has_meaningful_text() {
        return Err(StageError::EmptyDocument);
    }
    let input = serde_json::to_string(&build_input(&ctx.document))?;
    let decision: TriageDecision = match generate_typed(ctx.provider, "triage", prompt, input).await
    {
        Ok(decision) => decision,
        Err(error) => {
            warn!(
                error = %error,
                "Стадия triage завершилась ошибкой; решение проигнорировано, проверка продолжается"
            );
            ctx.triage = None;
            return Ok(());
        }
    };
    if !verify_decision(&ctx.document, &decision) {
        warn!(
            kind = ?decision.kind,
            "Решение стадии triage проигнорировано: цитата не подтверждена или причина не на русском"
        );
        ctx.triage = None;
        return Ok(());
    }
    if decision.kind == DocumentKind::NotRequirements && has_requirement_markers(&ctx.document) {
        warn!(
            "Решение стадии triage not_requirements проигнорировано: в документе найдены признаки требований"
        );
        ctx.triage = None;
        return Ok(());
    }
    ctx.triage = Some(decision);
    Ok(())
}

fn build_input(document: &Document) -> TriageInput<'_> {
    let headings = document
        .blocks
        .iter()
        .filter(|block| matches!(block.content, BlockContent::Heading { .. }))
        .take(MAX_HEADINGS)
        .map(truncate_block)
        .collect();
    let preview_blocks = document
        .blocks
        .iter()
        .filter(|block| !matches!(block.content, BlockContent::Heading { .. }))
        .take(MAX_PREVIEW_BLOCKS)
        .map(truncate_block)
        .collect();
    TriageInput {
        title: &document.title,
        headings,
        preview_blocks,
        total_blocks: document.blocks.len(),
    }
}

fn truncate_block(block: &Block) -> Block {
    let content = match &block.content {
        BlockContent::Heading { level, text } => BlockContent::Heading {
            level: *level,
            text: truncate_chars(text, MAX_BLOCK_CHARS),
        },
        BlockContent::Paragraph { text } => BlockContent::Paragraph {
            text: truncate_chars(text, MAX_BLOCK_CHARS),
        },
        BlockContent::ListItem { text } => BlockContent::ListItem {
            text: truncate_chars(text, MAX_BLOCK_CHARS),
        },
        BlockContent::Code { language, text } => BlockContent::Code {
            language: language.clone(),
            text: truncate_chars(text, MAX_BLOCK_CHARS),
        },
        BlockContent::Table { rows, row_lines } => BlockContent::Table {
            rows: rows
                .iter()
                .take(MAX_TABLE_ROWS)
                .map(|row| {
                    row.iter()
                        .map(|cell| truncate_chars(cell, MAX_CELL_CHARS))
                        .collect()
                })
                .collect(),
            row_lines: row_lines.iter().take(MAX_TABLE_ROWS).copied().collect(),
        },
    };
    Block {
        id: block.id,
        location: block.location,
        content,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => text[..byte_index].to_owned(),
        None => text.to_owned(),
    }
}

fn verify_decision(document: &Document, decision: &TriageDecision) -> bool {
    !decision.reason.trim().is_empty()
        && has_cyrillic(&decision.reason)
        && document.contains_quote(decision.evidence.location, &decision.evidence.quote)
}

fn has_requirement_markers(document: &Document) -> bool {
    const MIN_MODAL_SUBJECT_BLOCKS: usize = 2;

    let mut modal_subject_blocks = 0usize;
    for block in &document.blocks {
        let has_id = match &block.content {
            BlockContent::Heading { text, .. }
            | BlockContent::Paragraph { text }
            | BlockContent::ListItem { text }
            | BlockContent::Code { text, .. } => text_has_requirement_id(text),
            BlockContent::Table { rows, .. } => rows
                .iter()
                .flatten()
                .any(|cell| text_has_requirement_id(cell)),
        };
        if has_id {
            return true;
        }
        let has_modal_subject = match &block.content {
            BlockContent::Heading { text, .. }
            | BlockContent::Paragraph { text }
            | BlockContent::ListItem { text }
            | BlockContent::Code { text, .. } => text_has_modal_with_subject(text),
            BlockContent::Table { rows, .. } => rows
                .iter()
                .flatten()
                .any(|cell| text_has_modal_with_subject(cell)),
        };
        if has_modal_subject {
            modal_subject_blocks += 1;
            if modal_subject_blocks >= MIN_MODAL_SUBJECT_BLOCKS {
                return true;
            }
        }
    }
    false
}

fn text_has_requirement_id(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Ф", "ФТ", "НФ", "НФТ", "БТ", "Т", "R", "REQ", "FR", "NFR", "US", "UC", "BR", "SR", "AC",
    ];
    text.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '|')
    })
    .any(|token| {
        let Some((prefix, number)) = token.split_once('-') else {
            return false;
        };
        PREFIXES.contains(&prefix)
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn text_has_modal_with_subject(text: &str) -> bool {
    let lower = text.to_lowercase();
    let has_modal = lower.split(|ch: char| !ch.is_alphanumeric()).any(|word| {
        matches!(
            word,
            "должен"
                | "должна"
                | "должно"
                | "должны"
                | "обязан"
                | "обязана"
                | "обязано"
                | "обязаны"
                | "shall"
                | "must"
        )
    });
    if !has_modal {
        return false;
    }
    const SUBJECTS: &[&str] = &[
        "систем",
        "сервис",
        "приложен",
        "пользовател",
        "модул",
        "system",
        "service",
        "application",
        "user",
    ];
    lower
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|word| word == "api" || SUBJECTS.iter().any(|prefix| word.starts_with(prefix)))
}
