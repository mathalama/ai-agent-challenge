use doc::{BlockId, SourceSpan};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type RequirementId = String;
pub type FindingId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TableRef {
    pub block_id: BlockId,
    pub index: usize,
    pub row: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Requirement {
    pub id: RequirementId,
    pub source_id: Option<String>,
    pub text: String,
    pub quote: String,
    pub location: SourceSpan,
    pub part: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<TableRef>,
}

macro_rules! criteria {
    ($($criterion:ident => ($name:literal, $reference:expr)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum Criterion { $($criterion),+ }

        impl Criterion {
            pub fn russian_name(self) -> &'static str {
                match self { $(Self::$criterion => $name),+ }
            }

            pub fn reference(self) -> &'static str {
                match self { $(Self::$criterion => $reference),+ }
            }
        }
    };
}

const INDIVIDUAL: &str = "ISO/IEC/IEEE 29148:2018 § 5.2.5";
const SET: &str = "ISO/IEC/IEEE 29148:2018 § 5.2.6";
const IEEE_830: &str = "IEEE Std 830-1998 § 4.3";

criteria! {
    Necessary => ("Необходимость", INDIVIDUAL),
    Appropriate => ("Уместность", INDIVIDUAL),
    Unambiguous => ("Однозначность", INDIVIDUAL),
    Complete => ("Полнота требования", INDIVIDUAL),
    Singular => ("Единичность", INDIVIDUAL),
    Feasible => ("Выполнимость требования", INDIVIDUAL),
    Verifiable => ("Проверяемость", INDIVIDUAL),
    Correct => ("Корректность", INDIVIDUAL),
    Conforming => ("Соответствие правилам оформления", INDIVIDUAL),
    SetComplete => ("Полнота набора", SET),
    Consistent => ("Непротиворечивость", SET),
    SetFeasible => ("Выполнимость набора", SET),
    Comprehensible => ("Понятность", SET),
    AbleToBeValidated => ("Возможность валидации", SET),
    Ranked => ("Ранжирование", IEEE_830),
    Modifiable => ("Модифицируемость", IEEE_830),
    Traceable => ("Трассируемость", IEEE_830),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Scope {
    Requirement(RequirementId),
    Set,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn russian_name(self) -> &'static str {
        match self {
            Self::Low => "низкая",
            Self::Medium => "средняя",
            Self::High => "высокая",
            Self::Critical => "критическая",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub quote: String,
    pub location: SourceSpan,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "source")]
pub enum Detector {
    Rule(String),
    Llm(String),
    Solver,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub id: FindingId,
    pub scope: Scope,
    pub criterion: Criterion,
    pub severity: Severity,
    pub evidence: Vec<Evidence>,
    pub explanation: String,
    pub suggestion: Option<String>,
    pub detector: Detector,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fact {
    pub text: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum RelatedId {
    Finding(FindingId),
    Requirement(RequirementId),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub why: String,
    pub priority: Severity,
    pub related: Vec<RelatedId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub question_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunPassport {
    pub document_title: String,
    pub date: String,
    pub model: String,
    pub agent_version: String,
}
