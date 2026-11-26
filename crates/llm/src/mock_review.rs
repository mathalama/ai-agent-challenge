use doc::{Block, BlockContent, SourceSpan};
use serde::Deserialize;
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

pub(crate) fn triage(input: &str) -> serde_json::Result<Value> {
    #[derive(Deserialize)]
    struct TriageInput {
        #[serde(default)]
        title: String,
        #[serde(default)]
        headings: Vec<Block>,
        #[serde(default)]
        preview_blocks: Vec<Block>,
        #[serde(default)]
        total_blocks: usize,
    }

    let input: TriageInput = serde_json::from_str(input)?;
    let (quote, location) = input
        .preview_blocks
        .iter()
        .chain(&input.headings)
        .find_map(block_quote)
        .unwrap_or_else(|| {
            (
                input.title.clone(),
                SourceSpan {
                    start_line: 1,
                    end_line: 1,
                    page: None,
                },
            )
        });

    let is_headings_only = input.preview_blocks.is_empty()
        && !input.headings.is_empty()
        && input.total_blocks <= input.headings.len();
    let is_placeholder_only = !input.preview_blocks.is_empty()
        && input.total_blocks <= input.headings.len() + input.preview_blocks.len()
        && input.preview_blocks.iter().all(is_stub_block);

    let (kind, reason) = if is_headings_only {
        (
            "stub",
            "Документ содержит только заголовки разделов без описания требований.",
        )
    } else if is_placeholder_only {
        (
            "stub",
            "Документ похож на шаблон или заглушку без содержательных требований.",
        )
    } else if looks_like_requirements(&input.title, &input.headings, &input.preview_blocks) {
        (
            "requirements",
            "Документ содержит формулировки требований или описание поведения системы.",
        )
    } else {
        (
            "not_requirements",
            "Текст не содержит признаков спецификации требований к системе или продукту.",
        )
    };

    Ok(json!({
        "kind": kind,
        "reason": reason,
        "evidence": {
            "quote": quote,
            "location": location,
        },
    }))
}

fn block_quote(block: &Block) -> Option<(String, SourceSpan)> {
    let text = match &block.content {
        BlockContent::Heading { text, .. }
        | BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => text.lines().find(|line| !line.trim().is_empty())?,
        BlockContent::Table { rows, .. } => rows
            .iter()
            .flatten()
            .map(String::as_str)
            .find(|cell| !cell.trim().is_empty())?,
    };
    Some((text.trim().to_owned(), block.location))
}

fn is_stub_block(block: &Block) -> bool {
    let text = match &block.content {
        BlockContent::Heading { text, .. }
        | BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => text.trim().to_lowercase(),
        BlockContent::Table { rows, .. } => rows
            .iter()
            .flatten()
            .map(|cell| cell.trim().to_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
    };
    if text.is_empty() {
        return true;
    }
    const MARKERS: &[&str] = &[
        "todo",
        "tbd",
        "wip",
        "placeholder",
        "заполнить",
        "добавить позже",
        "в разработке",
        "черновик",
        "шаблон",
        "нет данных",
        "уточнить",
    ];
    let stripped = text.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '[' | ']' | '<' | '>' | '.' | '-' | '—' | ':' | '…')
    });
    stripped.is_empty()
        || (stripped.chars().count() <= 40
            && MARKERS.iter().any(|marker| stripped.contains(marker)))
}

fn looks_like_requirements(title: &str, headings: &[Block], preview: &[Block]) -> bool {
    let mut combined = String::from(title);
    for block in headings.iter().chain(preview) {
        combined.push('\n');
        match &block.content {
            BlockContent::Heading { text, .. }
            | BlockContent::Paragraph { text }
            | BlockContent::ListItem { text }
            | BlockContent::Code { text, .. } => combined.push_str(text),
            BlockContent::Table { rows, .. } => {
                for cell in rows.iter().flatten() {
                    combined.push(' ');
                    combined.push_str(cell);
                }
            }
        }
    }
    if has_requirement_id(&combined) {
        return true;
    }
    let lower = combined.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "требован",
        "спецификац",
        "техническое задание",
        "функциональн",
        "нефункциональн",
        "система",
        "сервис",
        "приложен",
        "пользователь",
        "критерии приёмки",
        "критерии приемки",
        "сценарий",
        "модуль",
        "интерфейс",
        "должен",
        "должна",
        "должно",
        "должны",
        "обязан",
        "уведомлен",
        "авторизац",
        "платёж",
        "платеж",
        "перевод",
        "карта",
        "счёт",
        "счет",
        "заказ",
        "клиент",
        "тестирован",
        "параметр",
        "статус",
        "ошибк",
        "запрос",
        "ответ",
        "метод",
        "лимит",
        "порог",
        "кредит",
        "заявк",
        "endpoint",
        "requirement",
        "specification",
        "http",
        "json",
        "api",
        "must",
        "shall",
        "should",
    ];
    if KEYWORDS.iter().any(|keyword| lower.contains(keyword)) {
        return true;
    }
    lower.split(|ch: char| !ch.is_alphanumeric()).any(|word| {
        matches!(
            word,
            "тз" | "фт" | "нфт" | "srs" | "prd" | "br" | "fr" | "nfr"
        )
    })
}

fn has_requirement_id(text: &str) -> bool {
    text.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '|')
    })
    .any(|token| {
        let Some((prefix, number)) = token.split_once('-') else {
            return false;
        };
        !prefix.is_empty()
            && prefix.chars().count() <= 4
            && prefix.chars().all(char::is_alphabetic)
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}
