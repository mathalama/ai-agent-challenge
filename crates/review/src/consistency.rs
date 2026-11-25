use std::collections::{BTreeMap, BTreeSet};

use llm::generate_typed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CalendarMonth, Criterion, Detector, Evidence, Fact, FactData, Finding, OperationIntent,
    Outcome, Requirement, RunContext, Scope, Severity, StatusRole, pipeline::Result,
};

#[derive(Default, Serialize)]
struct Group<'a> {
    entity: String,
    requirements: BTreeMap<&'a str, &'a Requirement>,
    facts: Vec<&'a Fact>,
    #[serde(skip)]
    conversions: BTreeSet<(String, String)>,
}

#[derive(Deserialize, JsonSchema)]
struct ConflictDraft {
    entity: String,
    requirement_ids: [String; 2],
    severity: Severity,
    explanation: String,
    suggestion: Option<String>,
}

pub(crate) async fn check(ctx: &mut RunContext<'_>, prompt: &str) -> Result<()> {
    let requirements: BTreeMap<_, _> = ctx
        .requirements
        .iter()
        .filter(|r| ctx.document.contains_quote(r.location, &r.quote))
        .map(|r| (r.id.as_str(), r))
        .collect();
    let mut groups = BTreeMap::<_, Group<'_>>::new();
    for fact in &ctx.facts {
        let Some(&requirement) = requirements.get(fact.requirement_id.as_str()) else {
            continue;
        };
        let span = fact.evidence.location;
        let parent = requirement.location;
        let entity = normalized(&fact.entity);
        if entity.is_empty()
            || span.page != parent.page
            || span.start_line < parent.start_line
            || span.end_line > parent.end_line
            || !ctx.document.contains_quote(span, &fact.evidence.quote)
        {
            continue;
        }
        let group = groups.entry(entity.clone()).or_insert_with(|| Group {
            entity,
            ..Group::default()
        });
        group
            .requirements
            .insert(requirement.id.as_str(), requirement);
        if let FactData::UnitConversion { from, to } = &fact.data {
            group
                .conversions
                .insert(ordered_pair(normalized(from), normalized(to)));
        }
        group.facts.push(fact);
    }

    let mut findings = Vec::new();
    let mut seen_rules = BTreeSet::new();
    for group in groups.values() {
        for (index, left) in group.facts.iter().enumerate() {
            for right in &group.facts[index + 1..] {
                let mut quotes = [evidence_key(&left.evidence), evidence_key(&right.evidence)];
                quotes.sort_unstable();
                if quotes[0] == quotes[1] {
                    continue;
                }
                if let Some((rule, explanation)) = group.compare(left, right)
                    && seen_rules.insert((rule, quotes))
                {
                    findings.push(Finding {
                        id: format!("consistency-rule-{:03}", findings.len() + 1),
                        scope: Scope::Set,
                        criterion: Criterion::Consistent,
                        severity: Severity::High,
                        evidence: vec![left.evidence.clone(), right.evidence.clone()],
                        explanation,
                        suggestion: None,
                        detector: Detector::Rule(rule.into()),
                        verified: false,
                    });
                }
            }
        }
    }

    let shared: Vec<_> = groups
        .values()
        .filter(|g| g.requirements.len() > 1)
        .collect();
    if !shared.is_empty() {
        let drafts: Vec<ConflictDraft> = generate_typed(
            ctx.provider,
            "consistency",
            prompt,
            serde_json::to_string(&serde_json::json!({"groups": shared}))?,
        )
        .await?;
        let mut seen = BTreeSet::new();
        for draft in drafts {
            let [first, second] = draft.requirement_ids;
            let pair = ordered_pair(first, second);
            let Some(group) = groups.get(&draft.entity) else {
                continue;
            };
            let (Some(left), Some(right)) = (
                group.requirements.get(pair.0.as_str()),
                group.requirements.get(pair.1.as_str()),
            ) else {
                continue;
            };
            if (left.location == right.location && left.quote == right.quote)
                || !seen.insert((pair, draft.explanation.clone()))
            {
                continue;
            }
            findings.push(Finding {
                id: format!("consistency-{:03}", findings.len() + 1),
                scope: Scope::Set,
                criterion: Criterion::Consistent,
                severity: draft.severity,
                evidence: [left, right]
                    .into_iter()
                    .map(|r| Evidence {
                        quote: r.quote.clone(),
                        location: r.location,
                    })
                    .collect(),
                explanation: draft.explanation,
                suggestion: draft.suggestion,
                detector: Detector::Llm(ctx.provider.model().into()),
                verified: false,
            });
        }
    }
    ctx.findings.extend(findings);
    Ok(())
}

impl Group<'_> {
    fn compare(&self, left: &Fact, right: &Fact) -> Option<(&'static str, String)> {
        use FactData::*;

        if let (Some(a), Some(b)) = (unit(&left.data), unit(&right.data))
            && a != b
        {
            let units = ordered_pair(a, b);
            return (!self.conversions.contains(&units)).then(|| ("unit_mismatch", format!("Для сопоставляемых величин указаны разные единицы ({} и {}) без описания преобразования.", units.0, units.1)));
        }

        match (&left.data, &right.data) {
            (
                FieldType {
                    field: a,
                    field_type: first,
                },
                FieldType {
                    field: b,
                    field_type: second,
                },
            ) if a == b && first != second => Some((
                "field_type_mismatch",
                format!("Поле «{a}» описано с разными типами."),
            )),
            (HttpMethod { method }, Operation { intent })
            | (Operation { intent }, HttpMethod { method })
                if (*method == crate::HttpMethod::Get) != (*intent == OperationIntent::Read) =>
            {
                Some((
                    "http_method_mismatch",
                    "HTTP-метод не соответствует описанному действию операции.".into(),
                ))
            }
            (
                StatusCode {
                    scenario: a,
                    code: first,
                    role: first_role,
                },
                StatusCode {
                    scenario: b,
                    code: second,
                    role: second_role,
                },
            ) if same_scenario(a, b)
                && valid_status(*first)
                && valid_status(*second)
                && first != second
                && first_role != second_role =>
            {
                let (heading, returned) = if *first_role == StatusRole::Heading {
                    (first, second)
                } else {
                    (second, first)
                };
                Some((
                    "status_code_mismatch",
                    format!(
                        "В заголовке заявлен код HTTP {heading}, а для этого сценария возвращается HTTP {returned}."
                    ),
                ))
            }
            (
                StatusCode {
                    scenario: a, code, ..
                },
                ResponseValue {
                    scenario: b,
                    field,
                    value,
                    outcome,
                },
            )
            | (
                ResponseValue {
                    scenario: b,
                    field,
                    value,
                    outcome,
                },
                StatusCode {
                    scenario: a, code, ..
                },
            ) if same_scenario(a, b) && status_conflicts(*code, *outcome) => Some((
                "status_outcome_mismatch",
                format!("Код HTTP {code} противоречит значению «{value}» в поле «{field}»."),
            )),
            (
                PeriodBasis { event },
                AcceptancePeriod {
                    requirement_ref,
                    event_months,
                    expected_month,
                },
            )
            | (
                AcceptancePeriod {
                    requirement_ref,
                    event_months,
                    expected_month,
                },
                PeriodBasis { event },
            ) => {
                let policy = if matches!(&left.data, PeriodBasis { .. }) {
                    left
                } else {
                    right
                };
                let requirement = self.requirements.get(policy.requirement_id.as_str())?;
                if requirement_ref.as_ref().is_some_and(|id| {
                    id != &requirement.id && Some(id) != requirement.source_id.as_ref()
                }) {
                    return None;
                }
                let actual = event_months
                    .iter()
                    .find(|candidate| candidate.event == *event)?
                    .month;
                months_differ(actual, *expected_month).then(|| ("acceptance_period_mismatch", format!("Критерий приёмки относит событие к другому месяцу, чем правило из требования {}.", requirement.source_id.as_ref().unwrap_or(&requirement.id))))
            }
            _ => None,
        }
    }
}

fn evidence_key(evidence: &Evidence) -> (Option<usize>, usize, usize, &str) {
    let span = evidence.location;
    (span.page, span.start_line, span.end_line, &evidence.quote)
}

fn unit(data: &FactData) -> Option<String> {
    match data {
        FactData::Quantity { unit, .. } | FactData::Range { unit, .. } => {
            let unit = normalized(unit);
            (!unit.is_empty()).then_some(unit)
        }
        _ => None,
    }
}

fn normalized(text: &str) -> String {
    text.trim().to_lowercase()
}

fn ordered_pair<T: Ord>(left: T, right: T) -> (T, T) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn same_scenario(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && a == b
}

fn valid_status(code: u16) -> bool {
    (100..600).contains(&code)
}

fn status_conflicts(code: u16, outcome: Outcome) -> bool {
    matches!(
        (code, outcome),
        (200..=299, Outcome::Error) | (400..=599, Outcome::Success)
    )
}

fn months_differ(a: CalendarMonth, b: CalendarMonth) -> bool {
    (1..=12).contains(&a.month)
        && (1..=12).contains(&b.month)
        && (a.month != b.month || matches!((a.year, b.year), (Some(a), Some(b)) if a != b))
}

#[cfg(test)]
mod tests;
