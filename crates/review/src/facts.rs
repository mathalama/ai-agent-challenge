use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Evidence, RequirementId};

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
