use std::collections::{BTreeMap, BTreeSet};

use llm::generate_typed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CalendarMonth, ContextItem, Criterion, Detector, Evidence, Fact, FactData, Finding,
    OperationIntent, Outcome, Requirement, RunContext, Scope, Severity, StatusRole,
    pipeline::Result,
};

#[derive(Default, Serialize)]
struct Group<'a> {
    entity: String,
    requirements: BTreeMap<&'a str, &'a Requirement>,
    context: BTreeMap<&'a str, &'a ContextItem>,
    facts: Vec<&'a Fact>,
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
    let context: BTreeMap<_, _> = ctx
        .context_items
        .iter()
        .filter(|r| ctx.document.contains_quote(r.location, &r.quote))
        .map(|r| (r.id.as_str(), r))
        .collect();
    let mut groups = BTreeMap::<_, Group<'_>>::new();
    let mut conversions = BTreeSet::new();
    for fact in &ctx.facts {
        let id = fact.requirement_id.as_str();
        let requirement = requirements.get(id).copied();
        let item = context.get(id).copied();
        let Some(parent) = requirement
            .map(|r| r.location)
            .or_else(|| item.map(|r| r.location))
        else {
            continue;
        };
        let span = fact.evidence.location;
        if span.page != parent.page
            || span.start_line < parent.start_line
            || span.end_line > parent.end_line
            || !ctx.document.contains_quote(span, &fact.evidence.quote)
        {
            continue;
        }
        if let FactData::UnitConversion { from, to } = &fact.data {
            conversions.insert(ordered_pair(normalized_unit(from), normalized_unit(to)));
        }
        let entity = normalized(&fact.entity);
        if entity.is_empty() {
            continue;
        }
        let group = groups.entry(entity.clone()).or_insert_with(|| Group {
            entity,
            ..Group::default()
        });
        if let Some(requirement) = requirement {
            group.requirements.insert(id, requirement);
        } else if let Some(item) = item {
            group.context.insert(id, item);
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
                if let Some((rule, severity, explanation)) =
                    group.compare(left, right, &conversions)
                    && seen_rules.insert((rule, quotes))
                {
                    findings.push(Finding {
                        id: format!("consistency-rule-{:03}", findings.len() + 1),
                        scope: Scope::Set,
                        criterion: Criterion::Consistent,
                        additional_criteria: if rule == "http_method_mismatch" {
                            vec![Criterion::Correct]
                        } else {
                            Vec::new()
                        },
                        severity,
                        evidence: vec![left.evidence.clone(), right.evidence.clone()],
                        explanation,
                        suggestion: None,
                        detector: Detector::Rule(rule.into()),
                        additional_detectors: Vec::new(),
                        merged_ids: Vec::new(),
                        fallback_evidence: false,
                        verified: false,
                    });
                }
            }
        }
    }

    let rule_count = findings.len();
    ctx.findings.extend(findings);

    let shared: Vec<_> = groups
        .values()
        .filter(|g| g.requirements.len() + g.context.len() > 1)
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
        let mut findings = Vec::new();
        for draft in drafts {
            let [first, second] = draft.requirement_ids;
            let pair = ordered_pair(first, second);
            let Some(group) = groups.get(&normalized(&draft.entity)) else {
                continue;
            };
            let (Some(left), Some(right)) = (
                group.evidence(pair.0.as_str()),
                group.evidence(pair.1.as_str()),
            ) else {
                continue;
            };
            if (left.location == right.location && left.quote == right.quote)
                || !seen.insert((pair, draft.explanation.clone()))
            {
                continue;
            }
            findings.push(Finding {
                id: format!("consistency-{:03}", rule_count + findings.len() + 1),
                scope: Scope::Set,
                criterion: Criterion::Consistent,
                additional_criteria: Vec::new(),
                severity: draft.severity,
                evidence: vec![left, right],
                explanation: draft.explanation,
                suggestion: draft.suggestion,
                detector: Detector::Llm(format!("consistency:{}", ctx.provider.model())),
                additional_detectors: Vec::new(),
                merged_ids: Vec::new(),
                fallback_evidence: false,
                verified: false,
            });
        }
        ctx.findings.extend(findings);
    }
    Ok(())
}

impl Group<'_> {
    fn evidence(&self, id: &str) -> Option<Evidence> {
        let (quote, location) = if let Some(requirement) = self.requirements.get(id) {
            (&requirement.quote, requirement.location)
        } else {
            let item = self.context.get(id)?;
            (&item.quote, item.location)
        };
        Some(Evidence {
            quote: quote.clone(),
            location,
        })
    }

    fn compare(
        &self,
        left: &Fact,
        right: &Fact,
        conversions: &BTreeSet<(String, String)>,
    ) -> Option<(&'static str, Severity, String)> {
        use FactData::*;

        if let (Some(a), Some(b)) = (unit(&left.data), unit(&right.data))
            && a != b
            && !(fixed_duration(&a) && fixed_duration(&b))
        {
            let units = ordered_pair(a, b);
            return (!conversions.contains(&units)).then(|| {
                let first = unit_label(&units.0);
                let second = unit_label(&units.1);
                let severity = if units.0 == "kzt" && units.1 == "kzt minor" {
                    Severity::Critical
                } else {
                    Severity::High
                };
                (
                    "unit_mismatch",
                    severity,
                    format!("Для сопоставляемых величин указаны разные единицы ({first} и {second}) без описания преобразования."),
                )
            });
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
                Severity::High,
                format!("Поле «{a}» описано с разными типами."),
            )),
            (HttpMethod { method }, Operation { intent })
            | (Operation { intent }, HttpMethod { method })
                if method.is_safe() && *intent != OperationIntent::Read =>
            {
                Some((
                    "http_method_mismatch",
                    Severity::Critical,
                    "Безопасный HTTP-метод используется для операции, меняющей состояние.".into(),
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
            ) if same_key(a, b)
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
                    Severity::High,
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
            ) if same_key(a, b) && status_conflicts(*code, *outcome) => Some((
                "status_outcome_mismatch",
                Severity::High,
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
                    .find(|candidate| same_key(&candidate.event, event))?
                    .month;
                months_differ(actual, *expected_month).then(|| {
                    (
                        "acceptance_period_mismatch",
                        Severity::High,
                        format!(
                            "Критерий приёмки относит событие к другому месяцу, чем правило из требования {}.",
                            requirement.source_id.as_ref().unwrap_or(&requirement.id)
                        ),
                    )
                })
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
            let unit = normalized_unit(unit);
            (!unit.is_empty()).then_some(unit)
        }
        _ => None,
    }
}

fn normalized(text: &str) -> String {
    text.trim().to_lowercase()
}

fn normalized_unit(text: &str) -> String {
    let unit = normalized(text);
    match unit.as_str() {
        "₸" | "тенге" | "теңге" => "kzt".into(),
        "тиын" | "тиына" | "тиыны" | "тиынов" | "тиынах" | "тиыне" => {
            "kzt minor".into()
        }
        "ms"
        | "мс"
        | "millisecond"
        | "milliseconds"
        | "миллисекунда"
        | "миллисекунды"
        | "миллисекунд"
        | "миллисекунду" => "ms".into(),
        "s" | "с" | "сек" | "сек." | "second" | "seconds" | "секунда" | "секунды" | "секунд"
        | "секунду" => "s".into(),
        "min" | "мин" | "мин." | "minute" | "minutes" | "минута" | "минуты" | "минут"
        | "минуту" => "min".into(),
        "h" | "ч" | "ч." | "hour" | "hours" | "час" | "часа" | "часов" => "h".into(),
        "d" | "day" | "days" | "день" | "дня" | "дней" | "дни" | "сутки" | "суток" | "сут"
        | "сут." => "d".into(),
        "wk" | "week" | "weeks" | "неделя" | "недели" | "неделю" | "недель" | "нед" | "нед." => {
            "wk".into()
        }
        _ => unit,
    }
}

fn unit_label(unit: &str) -> &str {
    match unit {
        "kzt" => "тенге",
        "kzt minor" => "тиын",
        _ => unit,
    }
}

fn fixed_duration(unit: &str) -> bool {
    matches!(unit, "ms" | "s" | "min" | "h" | "d" | "wk")
}

fn ordered_pair<T: Ord>(left: T, right: T) -> (T, T) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn same_key(a: &str, b: &str) -> bool {
    let first = normalized(a);
    !first.is_empty() && first == normalized(b)
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
