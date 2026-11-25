use serde_json::{Value, json};

pub(crate) const DOCUMENT_TITLE: &str = "Уведомление о расходах по карте";

// Fixed responses for the acceptance document. Evidence is taken from the request.
struct Scenario {
    quotes: &'static [&'static str],
    criterion: &'static str,
    severity: &'static str,
    explanation: &'static str,
    question: &'static str,
    topic: &'static str,
    options: [&'static str; 2],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        quotes: &[
            "максимум один такой push за календарный месяц",
            "независимо от истории предыдущей настройки",
        ],
        criterion: "consistent",
        severity: "high",
        explanation: "Ф-3 ограничивает частоту уведомлений, а Ф-4 позволяет повторную отправку после смены порога.",
        question: "После смены порога в том же месяце можно отправить второй push по карте?",
        topic: "notifications",
        options: [
            "Да, при достижении нового порога",
            "Нет, лимит один push в месяц сохраняется",
        ],
    },
    Scenario {
        quotes: &[
            "Месяц определяется по дате совершения покупки",
            "Февральский счётчик вырос на 4 000 ₸",
        ],
        criterion: "consistent",
        severity: "high",
        explanation: "Ф-2 определяет месяц по дате покупки, а критерий приёмки относит покупку к месяцу проведения.",
        question: "По какой дате определять месяц покупки?",
        topic: "calculation",
        options: ["По дате совершения покупки", "По дате проведения покупки"],
    },
    Scenario {
        quotes: &["Держатель активной карты KZT задаёт целый порог"],
        criterion: "set_complete",
        severity: "medium",
        explanation: "Спецификация не определяет доставку уведомлений после блокировки карты.",
        question: "Что делать с уведомлениями, если карта заблокирована?",
        topic: "notifications",
        options: [
            "Приостановить отправку до разблокировки",
            "Продолжать отправку по действующей настройке",
        ],
    },
];

pub(crate) fn findings(input: &str) -> serde_json::Result<Value> {
    #[derive(serde::Deserialize)]
    struct SetInput {
        requirements: Vec<Value>,
        context: Vec<Value>,
    }
    let input: SetInput = serde_json::from_str(input)?;
    let sources: Vec<_> = input.requirements.iter().chain(&input.context).collect();
    let findings: Vec<_> = SCENARIOS
        .iter()
        .filter_map(|scenario| {
            let evidence: Option<Vec<_>> = scenario
                .quotes
                .iter()
                .map(|fragment| {
                    let source = sources.iter().find(|requirement| {
                        requirement["quote"]
                            .as_str()
                            .is_some_and(|quote| quote.contains(fragment))
                    })?;
                    Some(json!({"quote": source["quote"], "location": source["location"]}))
                })
                .collect();
            Some(json!({
                "requirement_id": null,
                "criterion": scenario.criterion,
                "severity": scenario.severity,
                "evidence": evidence?,
                "explanation": scenario.explanation,
                "suggestion": null,
            }))
        })
        .collect();
    Ok(json!(findings))
}

pub(crate) fn questions(input: &str) -> serde_json::Result<Value> {
    let findings: Vec<Value> = serde_json::from_str(input)?;
    let questions: Vec<_> = SCENARIOS
        .iter()
        .filter_map(|scenario| {
            let finding = findings
                .iter()
                .find(|item| item["explanation"] == scenario.explanation)?;
            Some(json!({
                "text": scenario.question,
                "why": scenario.explanation,
                "topic": scenario.topic,
                "options": scenario.options,
                "finding_ids": [finding["id"]],
                "evidence": finding["evidence"],
            }))
        })
        .collect();
    Ok(json!(questions))
}
