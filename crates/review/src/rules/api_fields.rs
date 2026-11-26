use std::collections::BTreeMap;

use async_trait::async_trait;
use doc::BlockContent;

use super::rule_finding;
use crate::{
    Check, CheckPhase, ContextItem, ContextKind, Criterion, Evidence, Finding, RunContext, Scope,
    Severity, StageResult,
};

pub(crate) struct ApiFieldTypes;

#[derive(Default)]
struct Declarations<'a> {
    request: Vec<(&'a ContextItem, String)>,
    response: Vec<(&'a ContextItem, String)>,
}

#[async_trait]
impl Check for ApiFieldTypes {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Set
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let tables: BTreeMap<_, _> = ctx
            .document
            .blocks
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Table { rows, .. } => Some((block.id.0, rows)),
                _ => None,
            })
            .collect();
        let mut fields = BTreeMap::<_, Declarations<'_>>::new();
        for item in &ctx.context_items {
            let (Some(table), Some(section)) = (&item.table, item.section.as_deref()) else {
                continue;
            };
            if item.kind != ContextKind::ApiParameter {
                continue;
            }
            let Some((operation, is_request)) = api_direction(section) else {
                continue;
            };
            let Some((headers, row)) = tables
                .get(&table.block_id.0)
                .and_then(|rows| Some((rows.first()?, rows.get(table.row)?)))
            else {
                continue;
            };
            let field = table_cell(headers, row, &["поле", "параметр", "field", "parameter"]);
            let kind = table_cell(headers, row, &["тип", "формат", "type", "format"]);
            let (Some(field), Some(kind)) = (field, kind) else {
                continue;
            };
            let kind = kind.to_ascii_lowercase();
            if !matches!(
                kind.as_str(),
                "integer" | "number" | "string" | "boolean" | "object" | "array" | "null"
            ) {
                continue;
            }
            let declarations = fields
                .entry((item.part.as_deref(), operation, field))
                .or_default();
            if is_request {
                declarations.request.push((item, kind));
            } else {
                declarations.response.push((item, kind));
            }
        }

        let mut findings = Vec::new();
        for ((_, _, field), declarations) in fields {
            let ([(request, input_type)], [(response, output_type)]) = (
                declarations.request.as_slice(),
                declarations.response.as_slice(),
            ) else {
                continue;
            };
            if input_type == output_type {
                continue;
            }
            let evidence = [request, response]
                .into_iter()
                .map(|item| Evidence {
                    quote: item.quote.clone(),
                    location: item.location,
                })
                .collect();
            let mut finding = rule_finding(
                "field_type_mismatch",
                format!("api-field-type-{}", request.id),
                Scope::Set,
                Criterion::Consistent,
                evidence,
                format!(
                    "Поле «{field}» имеет тип {input_type} в запросе и {output_type} в ответе."
                ),
                "Согласовать тип поля в контракте или явно описать его преобразование.",
            );
            finding.severity = Severity::High;
            findings.push(finding);
        }
        Ok(findings)
    }
}

fn table_cell<'a>(headers: &[String], row: &'a [String], names: &[&str]) -> Option<&'a str> {
    let index = headers
        .iter()
        .position(|header| names.contains(&header.trim().to_lowercase().as_str()))?;
    let value = row.get(index)?.trim().trim_matches('`').trim();
    (!value.is_empty()).then_some(value)
}

fn api_direction(section: &str) -> Option<(Vec<&str>, bool)> {
    let mut operation = Vec::new();
    for heading in section.split(" / ") {
        let is_request = match heading.trim().to_lowercase().as_str() {
            "параметры запроса"
            | "входные параметры"
            | "параметры строки запроса"
            | "query-параметры"
            | "path-параметры"
            | "тело запроса"
            | "request parameters"
            | "request body" => true,
            "ответ"
            | "структура ответа"
            | "параметры ответа"
            | "response"
            | "response parameters"
            | "response body" => false,
            _ => {
                operation.push(heading);
                continue;
            }
        };
        return Some((operation, is_request));
    }
    None
}
