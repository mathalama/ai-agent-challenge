use std::collections::HashSet;

use async_trait::async_trait;
use doc::{BlockContent, Document};
use llm::generate_typed;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;
use unicode_script::{Script, UnicodeScript};

use crate::{
    CheckPhase, ContextItem, Criterion, Detector, Evidence, Finding, Requirement, RunContext,
    Scope, Severity, Stage, clarify, consistency, facts,
    pipeline::Result,
    segment::{RequirementDraft, merge_requirements, segment_blocks},
};

const REVIEW_BATCH_SIZE: usize = 12;
const DOMAIN_CONTEXT: &str = include_str!("../../../prompts/domain_context.md");

#[derive(Deserialize, JsonSchema)]
struct FindingDraft {
    requirement_id: Option<String>,
    criterion: Criterion,
    severity: Severity,
    evidence: Vec<Evidence>,
    explanation: String,
    suggestion: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum ScaffoldStage {
    Segment,
    Facts,
    Rules,
    RequirementChecks,
    SetChecks,
    Consistency,
    Ambiguity,
    Verify,
    Clarify,
}

impl ScaffoldStage {
    pub(crate) const ORDER: [Self; 9] = [
        Self::Segment,
        Self::Facts,
        Self::Rules,
        Self::RequirementChecks,
        Self::SetChecks,
        Self::Consistency,
        Self::Ambiguity,
        Self::Verify,
        Self::Clarify,
    ];

    fn prompt(self) -> &'static str {
        match self {
            Self::Segment => include_str!("../../../prompts/segment.md"),
            Self::Facts => include_str!("../../../prompts/facts.md"),
            Self::Rules => include_str!("../../../prompts/rules.md"),
            Self::RequirementChecks => include_str!("../../../prompts/requirement_checks.md"),
            Self::SetChecks => include_str!("../../../prompts/set_checks.md"),
            Self::Consistency => include_str!("../../../prompts/consistency.md"),
            Self::Ambiguity => include_str!("../../../prompts/ambiguity.md"),
            Self::Verify => include_str!("../../../prompts/verify.md"),
            Self::Clarify => include_str!("../../../prompts/clarify.md"),
        }
    }

    fn llm_prompt(self) -> String {
        format!("{}\n\n{}", self.prompt().trim(), DOMAIN_CONTEXT.trim())
    }
}

#[async_trait]
impl Stage for ScaffoldStage {
    fn name(&self) -> &'static str {
        match self {
            Self::Segment => "segment",
            Self::Facts => "facts",
            Self::Rules => "rules",
            Self::RequirementChecks => "requirement_checks",
            Self::SetChecks => "set_checks",
            Self::Consistency => "consistency",
            Self::Ambiguity => "ambiguity",
            Self::Verify => "verify",
            Self::Clarify => "clarify",
        }
    }

    async fn run(&self, ctx: &mut RunContext<'_>) -> Result<()> {
        let name = self.name();
        match self {
            Self::Segment => {
                let blocks = segment_blocks(&ctx.document);
                let drafts = if blocks.iter().any(|block| {
                    matches!(
                        block.content,
                        doc::BlockContent::Paragraph { .. } | doc::BlockContent::ListItem { .. }
                    )
                }) {
                    let prompt = self.llm_prompt();
                    let input = serde_json::to_string(&serde_json::json!({
                        "title": ctx.document.title,
                        "blocks": blocks,
                    }))?;
                    match generate_typed::<Vec<RequirementDraft>>(
                        ctx.provider,
                        name,
                        &prompt,
                        input,
                    )
                    .await
                    {
                        Ok(drafts) => drafts,
                        Err(error) if error.is_output_limit() => {
                            (ctx.requirements, ctx.context_items) =
                                merge_requirements(&ctx.document, Vec::new());
                            return Err(error.into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    Vec::new()
                };
                (ctx.requirements, ctx.context_items) = merge_requirements(&ctx.document, drafts);
            }
            Self::Facts => {
                facts::extract(ctx, &self.llm_prompt(), REVIEW_BATCH_SIZE).await?;
            }
            Self::Rules => {
                let findings = ctx.checks.run(CheckPhase::Rules, ctx).await?;
                ctx.rules_completed = ctx.checks.can_complete_rules(ctx);
                ctx.findings.extend(findings);
            }
            Self::RequirementChecks => {
                let findings = ctx.checks.run(CheckPhase::Requirement, ctx).await?;
                ctx.findings.extend(findings);
                let before = ctx.findings.len();
                for start in (0..ctx.requirements.len()).step_by(REVIEW_BATCH_SIZE) {
                    let end = (start + REVIEW_BATCH_SIZE).min(ctx.requirements.len());
                    let input = serde_json::to_string(&ctx.requirements[start..end])?;
                    self.find_with_model(ctx, input, ctx.findings.len() - before)
                        .await?;
                }
            }
            Self::SetChecks => {
                let findings = ctx.checks.run(CheckPhase::Set, ctx).await?;
                ctx.findings.extend(findings);
                self.find_with_model(ctx, set_input(&ctx.requirements, &ctx.context_items)?, 0)
                    .await?;
            }
            Self::Consistency => {
                consistency::check(ctx, &self.llm_prompt()).await?;
            }
            Self::Ambiguity => {
                self.find_with_model(ctx, set_input(&ctx.requirements, &ctx.context_items)?, 0)
                    .await?;
            }
            Self::Verify => {
                let greek = source_greek_symbols(&ctx.document);
                ctx.findings.retain(|finding| {
                    if matches!(finding.detector, Detector::Llm(_))
                        && has_foreign_letters(&finding.explanation, &greek)
                    {
                        warn!(
                            finding_id = %finding.id,
                            "Отброшена находка с посторонней письменностью в пояснении"
                        );
                        return false;
                    }
                    if !has_cyrillic(&finding.explanation) {
                        warn!(
                            finding_id = %finding.id,
                            "Отброшена находка без кириллицы в пояснении"
                        );
                        return false;
                    }
                    true
                });
                for finding in &mut ctx.findings {
                    if finding.suggestion.as_deref().is_some_and(|suggestion| {
                        !has_cyrillic(suggestion)
                            || matches!(finding.detector, Detector::Llm(_))
                                && has_foreign_letters(suggestion, &greek)
                    }) {
                        warn!(
                            finding_id = %finding.id,
                            "Удалена рекомендация с неподдерживаемой письменностью"
                        );
                        finding.suggestion = None;
                    }
                    finding.verified = !finding.evidence.is_empty()
                        && finding.evidence.iter().all(|evidence| {
                            ctx.document
                                .contains_quote(evidence.location, &evidence.quote)
                        });
                    crate::dedup::narrow_set_scope(finding, &ctx.requirements);
                }
                ctx.findings = crate::dedup::merge_findings_in_context(
                    std::mem::take(&mut ctx.findings),
                    &ctx.requirements,
                );
            }
            Self::Clarify => clarify::ask_questions(ctx, &self.llm_prompt()).await?,
        }
        Ok(())
    }
}

impl ScaffoldStage {
    async fn find_with_model(
        self,
        ctx: &mut RunContext<'_>,
        input: String,
        offset: usize,
    ) -> Result<()> {
        if ctx.requirements.is_empty()
            && (matches!(self, Self::RequirementChecks) || ctx.context_items.is_empty())
        {
            return Ok(());
        }
        let prompt = self.llm_prompt();
        let findings: Vec<FindingDraft> =
            generate_typed(ctx.provider, self.name(), &prompt, input).await?;
        let accepted = findings.into_iter().filter(|draft| {
            if has_cyrillic(&draft.explanation) {
                true
            } else {
                warn!(
                    stage = self.name(),
                    "Отброшена находка без кириллицы в пояснении"
                );
                false
            }
        });
        ctx.findings
            .extend(accepted.enumerate().map(|(index, draft)| {
                let scope = draft
                    .requirement_id
                    .filter(|id| {
                        ctx.requirements
                            .iter()
                            .any(|requirement| requirement.id == *id)
                    })
                    .map_or(Scope::Set, Scope::Requirement);
                Finding {
                    id: format!("{}-{:03}", self.name(), offset + index + 1),
                    scope,
                    criterion: draft.criterion,
                    additional_criteria: Vec::new(),
                    severity: draft.severity,
                    evidence: draft.evidence,
                    explanation: draft.explanation,
                    suggestion: draft.suggestion,
                    detector: Detector::Llm(format!("{}:{}", self.name(), ctx.provider.model())),
                    additional_detectors: Vec::new(),
                    merged_ids: Vec::new(),
                    fallback_evidence: false,
                    verified: false,
                }
            }));
        Ok(())
    }
}

fn set_input(requirements: &[Requirement], context: &[ContextItem]) -> Result<String> {
    Ok(serde_json::to_string(
        &serde_json::json!({ "requirements": requirements, "context": context }),
    )?)
}

fn source_greek_symbols(document: &Document) -> HashSet<char> {
    let mut symbols = HashSet::new();
    let mut collect = |text: &str| {
        symbols.extend(text.chars().filter(|ch| ch.script() == Script::Greek));
    };
    for block in &document.blocks {
        match &block.content {
            BlockContent::Heading { text, .. }
            | BlockContent::Paragraph { text }
            | BlockContent::ListItem { text }
            | BlockContent::Code { text, .. } => collect(text),
            BlockContent::Table { rows, .. } => {
                rows.iter().flatten().for_each(|cell| collect(cell))
            }
        }
    }
    symbols
}

fn has_foreign_letters(text: &str, greek: &HashSet<char>) -> bool {
    text.chars().any(|character| {
        character.is_alphabetic()
            && match character.script() {
                Script::Cyrillic | Script::Latin | Script::Common | Script::Inherited => false,
                Script::Greek => !greek.contains(&character),
                _ => true,
            }
    })
}

pub(crate) fn has_cyrillic(text: &str) -> bool {
    text.chars().any(|character| {
        ('\u{0400}'..='\u{04FF}').contains(&character) && character.is_alphabetic()
    })
}

#[cfg(test)]
mod tests {
    use super::has_cyrillic;

    #[test]
    fn cyrillic_detection_handles_mixed_and_non_cyrillic_text() {
        assert!(has_cyrillic("Замечание по требованию"));
        assert!(has_cyrillic("Метод GET /loans/create изменяет состояние"));
        assert!(has_cyrillic("Теңге"));
        assert!(!has_cyrillic("Requirement is ambiguous"));
        assert!(!has_cyrillic("HTTP 200 / ERROR"));
        assert!(!has_cyrillic("12345 — !?"));
        assert!(!has_cyrillic(""));
    }
}
