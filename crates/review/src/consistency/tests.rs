use doc::SourceSpan;

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
    assert_eq!(group.compare(&left, &right).map(|(name, _)| name), expected);
    assert_eq!(group.compare(&right, &left).map(|(name, _)| name), expected);
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
fn http_method_must_match_operation_intent() {
    for (method, intent, expected) in [
        (
            HttpMethod::Get,
            OperationIntent::Update,
            Some("http_method_mismatch"),
        ),
        (
            HttpMethod::Post,
            OperationIntent::Read,
            Some("http_method_mismatch"),
        ),
        (HttpMethod::Get, OperationIntent::Read, None),
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
        ("locked", 429, Some("status_code_mismatch")),
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
            "created",
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
                        event: "dispatch".into(),
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
