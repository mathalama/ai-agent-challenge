use std::path::Path;

use async_trait::async_trait;
use doc::ParserRegistry;
use llm::{LlmProvider, LlmRequest, LlmResponse, MockProvider, RetryProvider, TraceLog, Usage};
use review::{CheckRegistry, Detector, Pipeline, RunContext};
use serde_json::{Value, json};

struct GroupedMock;

#[async_trait]
impl LlmProvider for GroupedMock {
    fn model(&self) -> &str {
        "mock"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        let input: Value = serde_json::from_str(&request.user_text).unwrap();
        let json = match request.stage.as_str() {
            "segment" => json!(input["blocks"].as_array().unwrap().iter().filter_map(|block| {
                let quote = block["text"].as_str()?;
                Some(json!({"text": quote, "quote": quote, "location": block["location"]}))
            }).collect::<Vec<_>>()),
            "facts" => json!(input.as_array().unwrap().iter().enumerate().map(|(i, row)| json!({
                "entity": if i < 28 { format!("resource-{}", i / 2) } else { format!("isolated-{i}") },
                "requirement_id": row["id"],
                "evidence": {"quote": row["quote"], "location": row["location"]},
                "data": {"kind": "frequency", "maximum": 2, "period": "week"}
            })).collect::<Vec<_>>()),
            "consistency" => {
                let groups = input["groups"].as_array().unwrap();
                assert_eq!(groups.len(), 14);
                let ids = |group: &Value| -> Vec<String> { group["requirements"].as_object().unwrap().keys().cloned().collect() };
                assert!(groups.iter().all(|group| ids(group).len() == 2));
                let group = &groups[0];
                json!([
                    {"entity": group["entity"], "requirement_ids": ids(group), "explanation": "Условия конфликтуют.", "severity": "high", "suggestion": null},
                    {"entity": group["entity"], "requirement_ids": [ids(group)[0].clone(), ids(&groups[1])[0].clone()], "explanation": "Пара из разных групп.", "severity": "high", "suggestion": null}
                ])
            }
            _ => return MockProvider.generate(request).await,
        };
        Ok(LlmResponse {
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn consistency_batches_shared_entities_for_thirty_requirements() {
    let source = (1..=30)
        .map(|i| format!("* П-{i}. Условие {i}\n"))
        .collect::<String>();
    let document = ParserRegistry::default()
        .parse(Path::new("input.md"), source.as_bytes())
        .unwrap();
    let trace = TraceLog::default();
    let provider = RetryProvider::new(Box::new(GroupedMock)).with_trace(trace.clone());
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &provider, &checks);

    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.requirements.len(), 30);
    let calls = serde_json::to_value(trace.entries()).unwrap();
    assert_eq!(
        calls
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["stage"] == "consistency")
            .count(),
        1
    );
    assert_eq!(ctx.findings.len(), 1);
    assert!(matches!(&ctx.findings[0].detector, Detector::Llm(_)));
    assert!(ctx.findings[0].verified);
    assert_eq!(ctx.findings[0].evidence.len(), 2);
}

struct SampleMock;

#[async_trait]
impl LlmProvider for SampleMock {
    fn model(&self) -> &str {
        "mock"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        let input: Value = serde_json::from_str(&request.user_text).unwrap();
        let json = match request.stage.as_str() {
            "segment" => json!(input["blocks"].as_array().unwrap().iter().filter_map(|block| {
                let quote = block["text"].as_str()?;
                ["Ф-2.", "Ф-3.", "Ф-4.", "Хранить идентификатор", "200. Successful", "{\"result\":\"ERROR\"", "403. Unsuccessful", "Для чужой карты"].iter()
                    .any(|start| quote.starts_with(start))
                    .then(|| json!({"text": quote, "quote": quote, "location": block["location"]}))
            }).collect::<Vec<_>>()),
            "facts" => {
                let schema = request.response_schema.to_string();
                assert!(schema.contains("time_zone") && schema.contains("frequency") && schema.contains("range"));
                assert!(!schema.contains("\"const\""));
                sample_facts(&input)
            }
            "consistency" => {
                let group = input["groups"].as_array().unwrap().iter().find(|group| group["entity"] == "push").unwrap();
                let ids: Vec<_> = group["requirements"].as_object().unwrap().keys().collect();
                json!([{"entity": "push", "requirement_ids": ids, "explanation": "Изменение порога допускает повторный push в том же месяце.", "severity": "high"}])
            }
            _ => return MockProvider.generate(request).await,
        };
        Ok(LlmResponse {
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

fn sample_facts(input: &Value) -> Value {
    let rows = input.as_array().unwrap();
    let mut facts = Vec::new();
    let mut add = |anchor: &str, quote: &str, entity: &str, data: Value| {
        let row = rows
            .iter()
            .find(|row| row["quote"].as_str().unwrap().contains(anchor))
            .unwrap_or_else(|| panic!("Не найдена цитата {anchor}"));
        facts.push(json!({"entity": entity, "requirement_id": row["id"], "evidence": {"quote": quote, "location": row["location"]}, "data": data}));
    };
    let money = "сопоставление расходов и порога";
    for (quote, unit) in [
        ("сумму покупки в тиынах", "KZT minor"),
        ("порог в тенге.", "KZT"),
    ] {
        add(
            "Хранить идентификатор",
            quote,
            money,
            json!({"kind": "quantity", "unit": unit}),
        );
    }
    let api = "сохранение настроек";
    for kind in ["integer", "string"] {
        let quote = format!("amount | {kind}");
        add(
            &quote,
            &quote,
            api,
            json!({"kind": "field_type", "field": "amount", "field_type": kind}),
        );
    }
    add(
        "REST type | GET",
        "GET",
        api,
        json!({"kind": "http_method", "method": "GET"}),
    );
    add(
        "Сохранение порога.",
        "Сохранение порога.",
        api,
        json!({"kind": "operation", "intent": "update"}),
    );
    for (quote, scenario, code, role) in [
        ("200. Successful", "sample", 200, "heading"),
        ("403. Unsuccessful", "foreign_card", 403, "heading"),
        (
            "Для чужой карты возвращается 404",
            "foreign_card",
            404,
            "returned",
        ),
    ] {
        add(
            quote,
            quote,
            api,
            json!({"kind": "status_code", "scenario": scenario, "code": code, "role": role}),
        );
    }
    add(
        "{\"result\":\"ERROR\"",
        "\"result\":\"ERROR\"",
        api,
        json!({"kind": "response_value", "scenario": "sample", "field": "result", "value": "ERROR", "outcome": "error"}),
    );
    add(
        "Ф-2.",
        "Месяц определяется по дате совершения покупки в Алматы.",
        "месяц расходов",
        json!({"kind": "period_basis", "event": "purchase"}),
    );
    add(
        "Покупка 31 января",
        "Покупка 31 января в 23:58 на 4 000 ₸ | Провести 1 февраля в 00:03 по Алматы | Февральский счётчик вырос на 4 000 ₸",
        "месяц расходов",
        json!({"kind": "acceptance_period", "requirement_ref": "Ф-2", "event_months": [{"event": "purchase", "month": {"month": 1}}, {"event": "posting", "month": {"month": 2}}], "expected_month": {"month": 2}}),
    );
    add(
        "Ф-3.",
        "На карту приходится максимум один такой push за календарный месяц.",
        "push",
        json!({"kind": "frequency", "maximum": 1, "period": "month"}),
    );
    let exception =
        "следующее достижение вызывает push независимо от истории предыдущей настройки.";
    add(
        "Ф-4.",
        exception,
        "push",
        json!({"kind": "policy", "statement": exception}),
    );
    json!(facts)
}

#[tokio::test]
async fn sample_conflicts_have_verified_quotes_on_both_sides() {
    let document = ParserRegistry::default()
        .parse(
            Path::new("requirements.md"),
            include_bytes!("../../../samples/requirements.md"),
        )
        .unwrap();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &SampleMock, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    let mut rules = Vec::new();
    for finding in &ctx.findings {
        assert!(finding.verified, "{}", finding.explanation);
        assert_eq!(finding.evidence.len(), 2);
        assert_ne!(finding.evidence[0].quote, finding.evidence[1].quote);
        match &finding.detector {
            Detector::Rule(rule) => rules.push(rule.as_str()),
            Detector::Llm(_) => {
                assert!(finding.evidence[0].quote.starts_with("Ф-3."));
                assert!(finding.evidence[1].quote.starts_with("Ф-4."));
            }
            _ => panic!("Неожиданный детектор"),
        }
    }
    rules.sort_unstable();
    assert_eq!(
        rules,
        [
            "acceptance_period_mismatch",
            "field_type_mismatch",
            "http_method_mismatch",
            "status_code_mismatch",
            "status_outcome_mismatch",
            "unit_mismatch"
        ]
    );
    assert_eq!(ctx.findings.len(), 7);
}
