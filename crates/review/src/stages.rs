use async_trait::async_trait;
use llm::generate_typed;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    CheckPhase, Criterion, Detector, Evidence, Finding, Question, RelatedId, RunContext, Scope,
    Severity, Stage, consistency,
    pipeline::Result,
    segment::{RequirementDraft, has_non_table_content, merge_requirements, non_table_blocks},
};

#[derive(Deserialize, JsonSchema)]
struct FindingDraft {
    requirement_id: Option<String>,
    criterion: Criterion,
    severity: Severity,
    evidence: Vec<Evidence>,
    explanation: String,
    suggestion: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct QuestionDraft {
    text: String,
    why: String,
    priority: Severity,
    finding_ids: Vec<String>,
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
        let prompt = self.prompt();
        match self {
            Self::Segment => {
                let drafts = if has_non_table_content(&ctx.document) {
                    let input = serde_json::to_string(&serde_json::json!({
                        "title": ctx.document.title,
                        "blocks": non_table_blocks(&ctx.document),
                    }))?;
                    generate_typed::<Vec<RequirementDraft>>(ctx.provider, name, prompt, input)
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
                        prompt,
                        serde_json::to_string(&ctx.requirements)?,
                    )
                    .await?
                };
            }
            Self::Rules => {
                // TODO(issue): зарегистрировать детерминированные проверки формулировок.
                let findings = ctx.checks.run(CheckPhase::Rules, ctx).await?;
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
                consistency::check(ctx, prompt).await?;
            }
            Self::Ambiguity => {
                self.find_with_model(ctx, serde_json::to_string(&ctx.requirements)?)
                    .await?;
            }
            Self::Verify => {
                for finding in &mut ctx.findings {
                    finding.verified = !finding.evidence.is_empty()
                        && finding.evidence.iter().all(|evidence| {
                            ctx.document
                                .contains_quote(evidence.location, &evidence.quote)
                        });
                }
            }
            Self::Clarify => {
                ctx.questions.clear();
                let verified: Vec<_> = ctx
                    .findings
                    .iter()
                    .filter(|finding| finding.verified)
                    .collect();
                if verified.is_empty() {
                    return Ok(());
                }
                let questions: Vec<QuestionDraft> = generate_typed(
                    ctx.provider,
                    name,
                    prompt,
                    serde_json::to_string(&verified)?,
                )
                .await?;
                for question in questions {
                    let related: Vec<_> = question
                        .finding_ids
                        .into_iter()
                        .filter(|id| verified.iter().any(|finding| finding.id == *id))
                        .map(RelatedId::Finding)
                        .collect();
                    if !related.is_empty() {
                        ctx.questions.push(Question {
                            id: format!("Q-{:03}", ctx.questions.len() + 1),
                            text: question.text,
                            why: question.why,
                            priority: question.priority,
                            related,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl ScaffoldStage {
    async fn find_with_model(self, ctx: &mut RunContext<'_>, input: String) -> Result<()> {
        if ctx.requirements.is_empty() {
            return Ok(());
        }
        let prompt = format!(
            "{} Сохрани координаты и дословные цитаты из входных данных. Для замечания к отдельному требованию скопируй его id в requirement_id, для замечания к набору верни null. При отсутствии замечаний верни пустой массив.",
            self.prompt()
        );
        let findings: Vec<FindingDraft> =
            generate_typed(ctx.provider, self.name(), &prompt, input).await?;
        ctx.findings
            .extend(findings.into_iter().enumerate().map(|(index, draft)| {
                let scope = draft
                    .requirement_id
                    .filter(|id| {
                        ctx.requirements
                            .iter()
                            .any(|requirement| requirement.id == *id)
                    })
                    .map_or(Scope::Set, Scope::Requirement);
                Finding {
                    id: format!("{}-{:03}", self.name(), index + 1),
                    scope,
                    criterion: draft.criterion,
                    severity: draft.severity,
                    evidence: draft.evidence,
                    explanation: draft.explanation,
                    suggestion: draft.suggestion,
                    detector: Detector::Llm(ctx.provider.model().into()),
                    verified: false,
                }
            }));
        Ok(())
    }
}
