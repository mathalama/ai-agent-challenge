use async_trait::async_trait;
use doc::BlockContent;
use llm::generate_typed;

use crate::{CheckPhase, Requirement, RunContext, Stage, pipeline::Result};

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
                // TODO(issue): извлечь все требования, их идентификаторы, части и точные цитаты.
                let candidate = serde_json::to_string(&mock_candidate(ctx))?;
                ctx.requirements = generate_typed(ctx.provider, name, prompt, candidate).await?;
            }
            Self::Facts => {
                // TODO(issue): извлечь сущности, параметры и ограничения из требований.
                ctx.facts = generate_typed(
                    ctx.provider,
                    name,
                    prompt,
                    serde_json::to_string(&ctx.requirements)?,
                )
                .await?;
            }
            Self::Rules => {
                // TODO(issue): зарегистрировать детерминированные проверки формулировок.
                let findings = ctx.checks.run(CheckPhase::Rules, ctx).await?;
                ctx.findings.extend(findings);
            }
            Self::RequirementChecks => {
                // TODO(issue): реализовать проверки требования по ISO/IEC/IEEE 29148 и IEEE 830.
                let findings = ctx.checks.run(CheckPhase::Requirement, ctx).await?;
                ctx.findings.extend(findings);
            }
            Self::SetChecks => {
                // TODO(issue): реализовать проверки набора по ISO/IEC/IEEE 29148 и IEEE 830.
                let findings = ctx.checks.run(CheckPhase::Set, ctx).await?;
                ctx.findings.extend(findings);
            }
            Self::Consistency => {
                // TODO(issue): сопоставить факты и выявить противоречия между требованиями.
            }
            Self::Ambiguity => {
                // TODO(issue): выявить разные прочтения и размытые формулировки.
            }
            Self::Verify => {
                // TODO(issue): протоколировать отклонённые доказательства и поддержать страницы PDF.
                let mut findings = std::mem::take(&mut ctx.findings);
                for finding in &mut findings {
                    finding.verified = !finding.evidence.is_empty()
                        && finding.evidence.iter().all(|evidence| {
                            !evidence.quote.trim().is_empty()
                                && ctx
                                    .source_at(evidence.location)
                                    .is_some_and(|source| source.contains(&evidence.quote))
                        });
                }
                ctx.findings = findings;
            }
            Self::Clarify => {
                // TODO(issue): формировать приоритетные вопросы по подтверждённым замечаниям.
                ctx.questions = generate_typed(
                    ctx.provider,
                    name,
                    prompt,
                    serde_json::to_string(&ctx.findings)?,
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn mock_candidate(ctx: &RunContext<'_>) -> Option<Requirement> {
    let candidate = ctx
        .document
        .blocks
        .iter()
        .find_map(|block| {
            let BlockContent::ListItem { text } = &block.content else {
                return None;
            };
            let (id, body) = text.split_once(". ")?;
            let (prefix, number) = id.split_once('-')?;
            if prefix.is_empty()
                || !prefix.chars().all(char::is_alphabetic)
                || number.is_empty()
                || !number.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some((block, Some(id.to_owned()), body))
        })
        .or_else(|| {
            ctx.document
                .blocks
                .iter()
                .find_map(|block| match &block.content {
                    BlockContent::Paragraph { text } | BlockContent::ListItem { text }
                        if !text.trim().is_empty() =>
                    {
                        Some((block, None, text.as_str()))
                    }
                    _ => None,
                })
        })?;
    let (block, source_id, text) = candidate;
    let quote = ctx.source_at(block.location)?.trim();
    Some(Requirement {
        id: "R-001".into(),
        source_id,
        text: text.into(),
        quote: quote.into(),
        location: block.location,
        part: None,
    })
}
