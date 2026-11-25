use async_trait::async_trait;
use llm::generate_typed;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;

use crate::{
    CheckPhase, Criterion, Detector, Evidence, Finding, RunContext, Scope, Severity, Stage,
    clarify, consistency,
    pipeline::Result,
    segment::{RequirementDraft, has_non_table_content, merge_requirements, non_table_blocks},
};

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
                let drafts = if has_non_table_content(&ctx.document) {
                    let prompt = self.llm_prompt();
                    let input = serde_json::to_string(&serde_json::json!({
                        "title": ctx.document.title,
                        "blocks": non_table_blocks(&ctx.document),
                    }))?;
                    generate_typed::<Vec<RequirementDraft>>(ctx.provider, name, &prompt, input)
                        .await?
                } else {
                    Vec::new()
                };
                ctx.requirements = merge_requirements(&ctx.document, drafts);
            }
            Self::Facts => {
                ctx.facts = if ctx.requirements.is_empty() {
                    Vec::new()
                } else {
                    generate_typed(
                        ctx.provider,
                        name,
                        &self.llm_prompt(),
                        serde_json::to_string(&ctx.requirements)?,
                    )
                    .await?
                };
            }
            Self::Rules => {
                let findings = ctx.checks.run(CheckPhase::Rules, ctx).await?;
                ctx.rules_completed = ctx.checks.can_complete_rules(ctx);
                ctx.findings.extend(findings);
            }
            Self::RequirementChecks => {
                let findings = ctx.checks.run(CheckPhase::Requirement, ctx).await?;
                ctx.findings.extend(findings);
                self.find_with_model(ctx, serde_json::to_string(&ctx.requirements)?)
                    .await?;
            }
            Self::SetChecks => {
                let findings = ctx.checks.run(CheckPhase::Set, ctx).await?;
                ctx.findings.extend(findings);
                self.find_with_model(ctx, serde_json::to_string(&ctx.requirements)?)
                    .await?;
            }
            Self::Consistency => {
                consistency::check(ctx, &self.llm_prompt()).await?;
            }
            Self::Ambiguity => {
                self.find_with_model(ctx, serde_json::to_string(&ctx.requirements)?)
                    .await?;
            }
            Self::Verify => {
                ctx.findings.retain(|finding| {
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
                    if finding
                        .suggestion
                        .as_deref()
                        .is_some_and(|suggestion| !has_cyrillic(suggestion))
                    {
                        warn!(
                            finding_id = %finding.id,
                            "Удалена рекомендация без кириллицы"
                        );
                        finding.suggestion = None;
                    }
                    finding.verified = !finding.evidence.is_empty()
                        && finding.evidence.iter().all(|evidence| {
                            ctx.document
                                .contains_quote(evidence.location, &evidence.quote)
                        });
                }
                ctx.findings = crate::dedup::merge_findings(std::mem::take(&mut ctx.findings));
            }
            Self::Clarify => clarify::ask_questions(ctx, &self.llm_prompt()).await?,
        }
        Ok(())
    }
}

impl ScaffoldStage {
    async fn find_with_model(self, ctx: &mut RunContext<'_>, input: String) -> Result<()> {
        if ctx.requirements.is_empty() {
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
                let suggestion = draft.suggestion.filter(|suggestion| {
                    if has_cyrillic(suggestion) {
                        true
                    } else {
                        warn!(stage = self.name(), "Удалена рекомендация без кириллицы");
                        false
                    }
                });
                Finding {
                    id: format!("{}-{:03}", self.name(), index + 1),
                    scope,
                    criterion: draft.criterion,
                    additional_criteria: Vec::new(),
                    severity: draft.severity,
                    evidence: draft.evidence,
                    explanation: draft.explanation,
                    suggestion,
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
