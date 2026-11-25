use std::path::Path;

use doc::{ParserRegistry, SourceSpan};
use llm::MockProvider;

use super::*;
use crate::{Bound, EventMonth, FieldType, HttpMethod};

fn check_rule(left: FactData, right: FactData, expected: Option<&str>) {
    let location = SourceSpan {
        start_line: 1,
        end_line: 1,
        page: None,
    };
    let make_fact = |data, id: &str| Fact {
        entity: "inventory".into(),
        requirement_id: id.into(),
        evidence: Evidence {
            quote: id.into(),
            location,
        },
        data,
    };
    let requirement = Requirement {
        id: "R-001".into(),
        source_id: Some("INV-7".into()),
        text: "Правило учёта".into(),
        quote: "Правило учёта".into(),
        location,
        part: None,
        section: None,
        table: None,
    };
    let left = make_fact(left, "R-001");
    let right = make_fact(right, "R-002");
    let group = Group {
        requirements: BTreeMap::from([(requirement.id.as_str(), &requirement)]),
        facts: vec![&left, &right],
        ..Group::default()
    };
    let conversions = BTreeSet::new();
    assert_eq!(
        group
            .compare(&left, &right, &conversions)
            .map(|(name, _, _)| name),
        expected
    );
    assert_eq!(
        group
            .compare(&right, &left, &conversions)
            .map(|(name, _, _)| name),
        expected
    );
}

#[test]
fn quantity_and_range_must_use_matching_units() {
    for (unit, expected) in [("cm", Some("unit_mismatch")), (" M ", None)] {
        check_rule(
            FactData::Quantity {
                value: Some(2.0),
                unit: "m".into(),
            },
            FactData::Range {
                lower: Bound {
                    value: 1.0,
                    inclusive: true,
                },
                upper: Bound {
                    value: 3.0,
                    inclusive: false,
                },
                unit: unit.into(),
            },
            expected,
        );
    }
}

#[test]
fn fixed_duration_units_do_not_need_a_document_conversion() {
    for (value, from, to) in [
        (30.0, "секунд", "min"),
        (24.0, "часа", "сутки"),
        (7.0, "дней", "неделя"),
    ] {
        check_rule(
            FactData::Quantity {
                value: Some(value),
                unit: from.into(),
            },
            FactData::Quantity {
                value: Some(1.0),
                unit: to.into(),
            },
            None,
        );
    }
}

#[tokio::test]
async fn verified_conversion_applies_across_entities() {
    let source = [
        "Комиссия передаётся в тенге.",
        "Комиссия хранится в тиынах.",
        "1 ₸ = 100 тиынов",
    ];
    let markdown = source.join("\n\n");
    let document = ParserRegistry::default()
        .parse(Path::new("input.md"), markdown.as_bytes())
        .unwrap();
    let checks = crate::CheckRegistry::default();
    let mut ctx = RunContext::new(document, &MockProvider, &checks);
    ctx.requirements = source
        .iter()
        .zip(&ctx.document.blocks)
        .enumerate()
        .map(|(i, (&quote, block))| Requirement {
            id: format!("R-{i}"),
            source_id: None,
            text: quote.into(),
            quote: quote.into(),
            location: block.location,
            part: None,
            section: None,
            table: None,
        })
        .collect();
    ctx.facts = [
        (
            "комиссия",
            FactData::Quantity {
                value: None,
                unit: "KZT".into(),
            },
        ),
        (
            "комиссия",
            FactData::Quantity {
                value: None,
                unit: "KZT minor".into(),
            },
        ),
        (
            "валюта",
            FactData::UnitConversion {
                from: "тиынов".into(),
                to: "₸".into(),
            },
        ),
    ]
    .into_iter()
    .zip(&ctx.requirements)
    .map(|((entity, data), requirement)| Fact {
        entity: entity.into(),
        requirement_id: requirement.id.clone(),
        evidence: Evidence {
            quote: requirement.quote.clone(),
            location: requirement.location,
        },
        data,
    })
    .collect();

    check(&mut ctx, "").await.unwrap();
    assert!(ctx.findings.is_empty());

    ctx.facts[2].evidence.quote = "Вымышленное соотношение".into();
    check(&mut ctx, "").await.unwrap();
    assert!(matches!(&ctx.findings[0].detector, Detector::Rule(rule) if rule == "unit_mismatch"));
    assert!(ctx.findings[0].explanation.contains("(тенге и тиын)"));
}

#[test]
fn field_type_must_agree_for_the_same_field() {
    for (name, field_type, expected) in [
        ("weight", FieldType::String, Some("field_type_mismatch")),
        ("weight", FieldType::Number, None),
        ("height", FieldType::String, None),
    ] {
        check_rule(
            FactData::FieldType {
                field: "weight".into(),
                field_type: FieldType::Number,
            },
            FactData::FieldType {
                field: name.into(),
                field_type,
            },
            expected,
        );
    }
}

#[test]
fn safe_http_methods_must_not_change_state() {
    for (method, intent, expected) in [
        (
            HttpMethod::Get,
            OperationIntent::Update,
            Some("http_method_mismatch"),
        ),
        (
            HttpMethod::Head,
            OperationIntent::Create,
            Some("http_method_mismatch"),
        ),
        (
            HttpMethod::Options,
            OperationIntent::Delete,
            Some("http_method_mismatch"),
        ),
        (
            HttpMethod::Trace,
            OperationIntent::Update,
            Some("http_method_mismatch"),
        ),
        (HttpMethod::Post, OperationIntent::Read, None),
        (HttpMethod::Get, OperationIntent::Read, None),
        (HttpMethod::Head, OperationIntent::Read, None),
        (HttpMethod::Options, OperationIntent::Read, None),
        (HttpMethod::Put, OperationIntent::Update, None),
    ] {
        check_rule(
            FactData::HttpMethod { method },
            FactData::Operation { intent },
            expected,
        );
    }
}

#[test]
fn response_status_heading_must_match_returned_code_within_its_scenario() {
    for (scenario, code, expected) in [
        (" LOCKED ", 429, Some("status_code_mismatch")),
        ("locked", 423, None),
        ("rate_limit", 429, None),
    ] {
        check_rule(
            FactData::StatusCode {
                scenario: "locked".into(),
                code: 423,
                role: StatusRole::Heading,
            },
            FactData::StatusCode {
                scenario: scenario.into(),
                code,
                role: StatusRole::Returned,
            },
            expected,
        );
    }
}

#[test]
fn response_status_must_match_body_outcome_within_its_scenario() {
    for (scenario, code, outcome, expected) in [
        (
            " CREATED ",
            202,
            Outcome::Error,
            Some("status_outcome_mismatch"),
        ),
        (
            "created",
            422,
            Outcome::Success,
            Some("status_outcome_mismatch"),
        ),
        ("created", 202, Outcome::Success, None),
        ("created", 422, Outcome::Unspecified, None),
        ("rejected", 202, Outcome::Error, None),
    ] {
        check_rule(
            FactData::StatusCode {
                scenario: "created".into(),
                code,
                role: StatusRole::Returned,
            },
            FactData::ResponseValue {
                scenario: scenario.into(),
                field: "state".into(),
                value: "done".into(),
                outcome,
            },
            expected,
        );
    }
}

#[test]
fn acceptance_must_use_the_period_of_the_event_named_by_the_rule() {
    for (reference, month, year, expected) in [
        (Some("INV-7"), 7, None, Some("acceptance_period_mismatch")),
        (
            Some("R-001"),
            6,
            Some(2033),
            Some("acceptance_period_mismatch"),
        ),
        (Some("INV-8"), 7, None, None),
        (None, 6, None, None),
        (None, 13, None, None),
    ] {
        check_rule(
            FactData::PeriodBasis {
                event: "dispatch".into(),
            },
            FactData::AcceptancePeriod {
                requirement_ref: reference.map(str::to_owned),
                event_months: vec![
                    EventMonth {
                        event: "delivery".into(),
                        month: CalendarMonth {
                            year: Some(2032),
                            month: 7,
                        },
                    },
                    EventMonth {
                        event: " Dispatch ".into(),
                        month: CalendarMonth {
                            year: Some(2032),
                            month: 6,
                        },
                    },
                ],
                expected_month: CalendarMonth { year, month },
            },
            expected,
        );
    }
}
