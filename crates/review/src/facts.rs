use std::collections::BTreeSet;

use doc::SourceSpan;
use llm::{LlmError, generate_typed};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use tracing::warn;

use crate::{
    ContextItem, Evidence, ProgressEvent, Requirement, RequirementId, RunContext, pipeline::Result,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fact {
    pub entity: String,
    pub requirement_id: RequirementId,
    pub evidence: Evidence,
    pub data: FactData,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(transform = schemars::transform::ReplaceConstValue::default())]
pub enum FactData {
    Quantity {
        value: Option<f64>,
        unit: String,
    },
    Range {
        lower: Bound,
        upper: Bound,
        unit: String,
    },
    UnitConversion {
        from: String,
        to: String,
    },
    FieldType {
        field: String,
        field_type: FieldType,
    },
    HttpMethod {
        method: HttpMethod,
    },
    Operation {
        intent: OperationIntent,
    },
    StatusCode {
        scenario: String,
        code: u16,
        role: StatusRole,
    },
    ResponseValue {
        scenario: String,
        field: String,
        value: String,
        outcome: Outcome,
    },
    TimeZone {
        zone: String,
    },
    Frequency {
        maximum: u32,
        period: FrequencyPeriod,
    },
    PeriodBasis {
        event: String,
    },
    AcceptancePeriod {
        requirement_ref: Option<RequirementId>,
        event_months: Vec<EventMonth>,
        expected_month: CalendarMonth,
    },
    Policy {
        statement: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Bound {
    pub value: f64,
    pub inclusive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Integer,
    Number,
    String,
    Boolean,
    Object,
    Array,
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Trace,
    Connect,
}

impl HttpMethod {
    pub(crate) fn is_safe(self) -> bool {
        matches!(self, Self::Get | Self::Head | Self::Options | Self::Trace)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationIntent {
    Read,
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusRole {
    Heading,
    Returned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Error,
    Unspecified,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrequencyPeriod {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EventMonth {
    pub event: String,
    pub month: CalendarMonth,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct CalendarMonth {
    pub year: Option<u16>,
    #[schemars(range(min = 1, max = 12))]
    pub month: u8,
}

#[derive(Deserialize, JsonSchema)]
struct FactDraft {
    entity: String,
    requirement_id: String,
    quote: Option<String>,
    data: FactData,
}

#[derive(Clone, Copy, Serialize)]
struct SourceBatch<'a> {
    requirements: &'a [Requirement],
    context: &'a [ContextItem],
}

#[derive(Serialize)]
struct FactInput<'a> {
    #[serde(flatten)]
    batch: SourceBatch<'a>,
    known_entities: &'a BTreeSet<String>,
}

impl<'a> SourceBatch<'a> {
    fn sources(self) -> impl Iterator<Item = (&'a str, &'a str, SourceSpan)> {
        self.requirements
            .iter()
            .map(|source| (source.id.as_str(), source.quote.as_str(), source.location))
            .chain(
                self.context
                    .iter()
                    .map(|source| (source.id.as_str(), source.quote.as_str(), source.location)),
            )
    }

    fn split(self) -> (Self, Self) {
        let (left_requirements, right_requirements) =
            self.requirements.split_at(self.requirements.len() / 2);
        let (left_context, right_context) = self.context.split_at(self.context.len() / 2);
        (
            Self {
                requirements: left_requirements,
                context: left_context,
            },
            Self {
                requirements: right_requirements,
                context: right_context,
            },
        )
    }
}

pub(crate) async fn extract(
    ctx: &mut RunContext<'_>,
    prompt: &str,
    batch_size: usize,
) -> Result<()> {
    let mut pending: Vec<_> = ctx
        .requirements
        .chunks(batch_size)
        .map(|requirements| SourceBatch {
            requirements,
            context: &[],
        })
        .chain(
            ctx.context_items
                .chunks(batch_size)
                .map(|context| SourceBatch {
                    requirements: &[],
                    context,
                }),
        )
        .rev()
        .collect();
    let mut facts = Vec::new();
    let mut known_entities = BTreeSet::new();
    while let Some(batch) = pending.pop() {
        let input = serde_json::to_string(&FactInput {
            batch,
            known_entities: &known_entities,
        })?;
        let drafts: Vec<FactDraft> =
            match generate_typed(ctx.provider, "facts", prompt, input).await {
                Ok(drafts) => drafts,
                Err(LlmError::MissingCandidate {
                    ref finish_reason, ..
                }) if matches!(finish_reason.as_str(), "length" | "MAX_TOKENS") => {
                    if batch.requirements.len() + batch.context.len() > 1 {
                        let (left, right) = batch.split();
                        ctx.emit(|| ProgressEvent::FactBatchSplit {
                            source_ids: batch.sources().map(|(id, _, _)| id.to_owned()).collect(),
                        });
                        pending.push(right);
                        pending.push(left);
                    } else if let Some((id, _, location)) = batch.sources().next() {
                        ctx.facts_complete = false;
                        ctx.emit(|| ProgressEvent::FactSourceSkipped {
                            source_id: id.to_owned(),
                            location,
                        });
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
        facts.extend(drafts.into_iter().filter_map(|draft| {
            let (id, source_quote, location) = batch
                .sources()
                .find(|(id, _, _)| *id == draft.requirement_id)?;
            let quote = draft.quote.unwrap_or_else(|| source_quote.to_owned());
            if !source_quote.contains(&quote) || !ctx.document.contains_quote(location, &quote) {
                warn!(
                    stage = "facts",
                    source_id = id,
                    start_line = location.start_line,
                    end_line = location.end_line,
                    page = location.page,
                    "Отброшен факт: цитата не совпадает с источником"
                );
                return None;
            }
            let entity_key = draft.entity.trim().to_lowercase();
            if !entity_key.is_empty() {
                known_entities.insert(entity_key);
            }
            Some(Fact {
                entity: draft.entity,
                requirement_id: draft.requirement_id,
                evidence: Evidence { quote, location },
                data: draft.data,
            })
        }));
    }
    ctx.facts = facts;
    Ok(())
}
