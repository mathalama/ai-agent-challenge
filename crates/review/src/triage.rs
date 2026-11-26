use doc::{Block, BlockContent, Document};
use llm::generate_typed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{Evidence, RunContext, StageError, pipeline::Result, stages::has_cyrillic};

const MAX_HEADINGS: usize = 40;
const MAX_PREVIEW_BLOCKS: usize = 12;

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
    headings: Vec<&'a Block>,
    preview_blocks: Vec<&'a Block>,
    total_blocks: usize,
}

pub(crate) async fn run(ctx: &mut RunContext<'_>, prompt: &str) -> Result<()> {
    if !ctx.document.has_meaningful_text() {
        return Err(StageError::EmptyDocument);
    }
    let input = serde_json::to_string(&build_input(&ctx.document))?;
    let decision: TriageDecision = generate_typed(ctx.provider, "triage", prompt, input).await?;
    if !verify_decision(&ctx.document, &decision) {
        warn!(
            kind = ?decision.kind,
            "Решение стадии triage проигнорировано: цитата не подтверждена или причина не на русском"
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
        .collect();
    let preview_blocks = document.blocks.iter().take(MAX_PREVIEW_BLOCKS).collect();
    TriageInput {
        title: &document.title,
        headings,
        preview_blocks,
        total_blocks: document.blocks.len(),
    }
}

fn verify_decision(document: &Document, decision: &TriageDecision) -> bool {
    !decision.reason.trim().is_empty()
        && has_cyrillic(&decision.reason)
        && document.contains_quote(decision.evidence.location, &decision.evidence.quote)
}
