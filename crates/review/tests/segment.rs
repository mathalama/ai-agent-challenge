use std::path::Path;

use async_trait::async_trait;
use doc::{ParserRegistry, SourceSpan};
use llm::{LlmProvider, LlmRequest, LlmResponse, Usage};
use review::{
    CheckRegistry, MarkdownRenderer, Pipeline, Report, ReportRenderer, RunContext, RunPassport,
    extract_source_id, extract_table_requirements,
};
use serde_json::json;

#[test]
fn extracts_requirement_source_ids_deterministically() {
    for (input, expected) in [
        ("Ф-1. Держатель активной карты KZT", Some("Ф-1")),
        ("* **Ф-2.** Месяц определяется по дате", Some("Ф-2")),
        ("**Ф-3**. Когда расходы переходят порог", Some("Ф-3")),
        ("Ф-4: При изменении порога", Some("Ф-4")),
        ("НФ-2 — Сервис передаёт push в течение минуты", Some("НФ-2")),
        ("НФТ-03) Максимальная задержка 60 с", Some("НФТ-03")),
        ("ФТ-1.2. Подпункт требования", Some("ФТ-1.2")),
        ("1.2. ФТ-1.2 Расчёт лимита", Some("ФТ-1.2")),
        ("[REQ-001] System shall send alert", Some("REQ-001")),
        ("(BR-4) Business rule", Some("BR-4")),
        ("FR_12: Underscore separator", Some("FR_12")),
        ("Ф–3. Короткое тире внутри ID", Some("Ф-3")),
        ("Ф-1", Some("Ф-1")),
        ("Ф-4 Держатель карты", Some("Ф-4")),
        ("Требование Ф-5: сумма не превышает лимит", Some("Ф-5")),
        (
            "Функциональное требование ФТ-7. Проверка баланса",
            Some("ФТ-7"),
        ),
        ("Пункт НФ-2 — задержка не более 1 минуты", Some("НФ-2")),
    ] {
        assert_eq!(
            extract_source_id(input).as_deref(),
            expected,
            "input: {input:?}"
        );
    }

    for non_id in [
        "",
        "   ",
        "UTF-8",
        "UTF-8.",
        "UTF-8 кодировка документа",
        "ISO-29148 стандарт требований",
        "ISO-4217: код валюты",
        "(ISO-20022) формат сообщений",
        "[TLS-1.3] защищённый транспорт",
        "HTTP-200: успешный статус",
        "SHA-256",
        "demo-42",
        "Path-параметры",
        "200. Ответ в случае успешного выполнения метода.",
        "1. Проверить пользовательскую сессию.",
        "Система реализует правило Ф-1 для карт KZT.",
        "Лимит применяется аналогично Ф-1 — по календарному месяцу.",
        "Соответствует требованию Ф-1: проверять сумму.",
        "Как в Ф-2 — считать по Алматы.",
        "Требования НФ-1 – НФ-5",
        "НФ-1 – НФ-5. Нефункциональные требования",
        "Ф-1…Ф-4",
        "См. Ф-1: подробности выше.",
        "Согласно Ф-2, месяц определяется по дате покупки.",
        "Ф-1.a некорректный суффикс",
    ] {
        assert_eq!(extract_source_id(non_id), None, "input: {non_id:?}");
    }
}

#[test]
fn slices_sample_tables_with_part_section_table_ref_and_verified_quotes() {
    let source = include_str!("../../../samples/requirements.md");
    let document = ParserRegistry::default()
        .parse(Path::new("samples/requirements.md"), source.as_bytes())
        .unwrap();

    let requirements = extract_table_requirements(&document);

    // Все извлечённые строки таблиц должны иметь подтверждённые цитаты и ссылки на таблицы.
    for requirement in &requirements {
        assert!(
            document.contains_quote(requirement.location, &requirement.quote),
            "Цитата не подтверждена: {:?}",
            requirement
        );
        assert!(requirement.table.is_some());
    }

    // 1. Критерии приёмки: 3 строки (строки 63, 64, 65), часть A, таблица №2 в документе.
    let acceptance: Vec<_> = requirements
        .iter()
        .filter(|req| req.section.as_deref() == Some("Критерии приемки"))
        .collect();
    assert_eq!(acceptance.len(), 3);
    assert_eq!(
        acceptance
            .iter()
            .map(|req| {
                let table = req.table.as_ref().unwrap();
                (
                    req.location.start_line,
                    req.location.end_line,
                    table.index,
                    table.row,
                )
            })
            .collect::<Vec<_>>(),
        [(63, 63, 2, 1), (64, 64, 2, 2), (65, 65, 2, 3)]
    );
    assert!(
        acceptance
            .iter()
            .all(|req| req.part.as_deref() == Some("Часть A. Новый функционал"))
    );
    assert!(acceptance[1].quote.contains("Покупка 31 января"));
    assert!(acceptance[1].text.contains("Ожидаемый результат:"));

    let description = requirements
        .iter()
        .find(|req| req.section.as_deref() == Some("Общие параметры"))
        .unwrap();
    assert_eq!(description.location.start_line, 103);
    assert!(description.quote.contains("чтение порога"));
    assert!(
        requirements
            .iter()
            .all(|req| !req.quote.contains("REST type |") && !req.quote.contains("amount |"))
    );

    // 5. Служебные таблицы (Общие сведения, Ревью требований, Блок согласующих) не попали в требования.
    assert!(requirements.iter().all(|req| !matches!(
        req.section.as_deref(),
        Some("Общие сведения" | "Ревью требований" | "Блок согласующих")
    )));
}

#[test]
fn filters_numbered_metadata_tables_without_dropping_domain_tables() {
    let source = concat!(
        "# Спецификация платежей\n\n",
        "## 1. Общие сведения\n\n",
        "| Наименование | Значение |\n",
        "| --- | --- |\n",
        "| Продукт | Мобильный банк |\n\n",
        "## 1.1. Паспорт документа\n\n",
        "| Реквизит | Значение |\n",
        "| --- | --- |\n",
        "| Версия | 1.0 |\n\n",
        "## 2. Согласование платежа\n\n",
        "| Сумма | Роль | Срок |\n",
        "| --- | --- | --- |\n",
        "| > 1 000 000 ₸ | Финансовый контролёр | 1 час |\n\n",
        "## 3. Ревью заявки\n\n",
        "| Этап | Действие |\n",
        "| --- | --- |\n",
        "| Первичная проверка | Проверить лимит клиента |\n\n",
        "## 4. Часть ответа с ошибками\n\n",
        "| Код | Сообщение |\n",
        "| --- | --- |\n",
        "| ERR_LIMIT | Превышен лимит |\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("payments.md"), source.as_bytes())
        .unwrap();

    let requirements = extract_table_requirements(&document);
    assert_eq!(requirements.len(), 3);

    // Нумерованные «1. Общие сведения» отфильтрованы.
    assert!(
        requirements
            .iter()
            .all(|req| req.section.as_deref() != Some("1. Общие сведения"))
    );

    // Предметные таблицы «Согласование платежа» и «Ревью заявки» сохранены.
    assert_eq!(
        requirements[0].section.as_deref(),
        Some("2. Согласование платежа")
    );
    assert_eq!(requirements[0].table.as_ref().unwrap().index, 3);
    assert_eq!(requirements[1].section.as_deref(), Some("3. Ревью заявки"));
    assert_eq!(requirements[1].table.as_ref().unwrap().index, 4);

    // Заголовок «Часть ответа с ошибками» — раздел, а не часть документа (A/B).
    assert_eq!(requirements[2].part, None);
    assert_eq!(
        requirements[2].section.as_deref(),
        Some("4. Часть ответа с ошибками")
    );
    assert_eq!(requirements[2].table.as_ref().unwrap().index, 5);
}

#[test]
fn slices_status_and_id_tables_and_skips_empty_rows() {
    let source = concat!(
        "# API\n\n",
        "## Часть B. REST API\n\n",
        "### Функциональные правила\n\n",
        "| ID | Требование |\n",
        "| --- | --- |\n",
        "| Ф-1 | Проверить лимит |\n",
        "| | |\n",
        "| НФ-2 | Ответ за 200 мс |\n\n",
        "### Статусы\n\n",
        "| Код | Описание |\n",
        "| --- | --- |\n",
        "| 200 | Успешный ответ |\n",
        "| 400 | Неверные параметры |\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("api.md"), source.as_bytes())
        .unwrap();

    let requirements = extract_table_requirements(&document);
    assert_eq!(requirements.len(), 4);

    assert_eq!(requirements[0].source_id.as_deref(), Some("Ф-1"));
    assert_eq!(requirements[0].table.as_ref().unwrap().index, 1);
    assert_eq!(requirements[0].table.as_ref().unwrap().row, 1);
    assert_eq!(requirements[0].location.start_line, 9);

    // Пустая строка пропущена, индекс строки таблицы соответствует исходному номеру строки данных.
    assert_eq!(requirements[1].source_id.as_deref(), Some("НФ-2"));
    assert_eq!(requirements[1].table.as_ref().unwrap().row, 3);
    assert_eq!(requirements[1].location.start_line, 11);

    assert_eq!(requirements[2].section.as_deref(), Some("Статусы"));
    assert_eq!(requirements[2].source_id, None);
    assert_eq!(requirements[2].table.as_ref().unwrap().index, 2);
    assert_eq!(requirements[2].text, "Код: 200; Описание: Успешный ответ");
    assert_eq!(
        requirements[3].text,
        "Код: 400; Описание: Неверные параметры"
    );
}

struct StubSegmentProvider;

#[async_trait]
impl LlmProvider for StubSegmentProvider {
    fn model(&self) -> &str {
        "stub"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
        let json = match request.stage.as_str() {
            "segment" => {
                let blocks = input["blocks"].as_array().unwrap();
                let response = blocks
                    .iter()
                    .find(|b| b["location"]["start_line"] == 151)
                    .unwrap();
                let functional = blocks
                    .iter()
                    .find(|b| b["location"]["start_line"] == 38)
                    .unwrap();
                json!([
                    {"block_id": functional["id"], "start_line": 38, "end_line": 38, "kind": "requirement"},
                    {"block_id": functional["id"], "start_line": 38, "end_line": 38, "kind": "requirement"},
                    {"block_id": response["id"], "start_line": 63, "end_line": 63, "kind": "requirement"},
                    {"block_id": response["id"], "start_line": 151, "end_line": 153, "kind": "requirement"},
                    {"block_id": response["id"], "start_line": 151, "end_line": 151, "kind": "requirement"}
                ])
            }
            "set_checks" => {
                let example = input["context"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["quote"].as_str().unwrap().contains("Проверьте сумму"))
                    .unwrap();
                json!([{
                    "requirement_id": null,
                    "criterion": "consistent",
                    "severity": "high",
                    "evidence": [{"quote": example["quote"], "location": example["location"]}],
                    "explanation": "Успешный статус сопровождается ошибкой.",
                    "suggestion": null
                }])
            }
            _ => json!([]),
        };
        Ok(LlmResponse {
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn segment_stage_fills_source_ids_parts_sections_and_deduplicates_tables() {
    let source = include_str!("../../../samples/requirements.md");
    let document = ParserRegistry::default()
        .parse(Path::new("samples/requirements.md"), source.as_bytes())
        .unwrap();

    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &StubSegmentProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    // Ф-1..Ф-4 получили детерминированные source_id, часть A и раздел, несмотря на то что модель их не вернула.
    let functional: Vec<_> = ctx
        .requirements
        .iter()
        .filter(|req| req.source_id.is_some())
        .collect();
    assert_eq!(
        functional
            .iter()
            .map(|req| req.source_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["Ф-1", "Ф-2", "Ф-3", "Ф-4"]
    );
    for req in &functional {
        assert_eq!(req.part.as_deref(), Some("Часть A. Новый функционал"));
        assert_eq!(
            req.section.as_deref(),
            Some("Требования к изменениям / Функциональные требования")
        );
        assert!(req.table.is_none());
    }

    // Дубль строки 63 от модели отброшен: осталась ровно одна запись (детерминированная из таблицы).
    let line_63: Vec<_> = ctx
        .requirements
        .iter()
        .filter(|req| req.location.start_line == 63)
        .collect();
    assert_eq!(line_63.len(), 1);
    assert!(line_63[0].table.is_some());

    // Абзац на строке 151 сохранён, даже если его диапазон 151..=153 коснулся таблицы на строке 153.
    let response_intro = ctx
        .requirements
        .iter()
        .find(|req| req.location.start_line == 151)
        .expect("Абзац на строке 151 не должен отбрасываться из-за касания таблицы");
    assert!(response_intro.table.is_none());
    assert_eq!(response_intro.part.as_deref(), Some("Часть B. REST API"));
    assert_eq!(response_intro.section.as_deref(), Some("Ответ"));

    assert_eq!(ctx.requirements.len(), 19);
    assert_eq!(ctx.requirements[0].id, "R-001");
    assert_eq!(ctx.requirements[18].id, "R-019");
    assert!(ctx.requirements[1].quote.contains("Возврат уменьшает"));
    assert!(ctx.requirements[1].text.starts_with("Месяц определяется"));
    assert!(
        ctx.findings
            .iter()
            .any(|f| f.id == "set_checks-001" && f.verified)
    );

    // Отчёт выводит ID из документа, разделы, порядковый номер таблицы и её заголовок (а не внутренний BlockId).
    let passport = RunPassport {
        document_title: ctx.document.title.clone(),
        date: "2026-11-25".into(),
        model: "stub".into(),
        agent_version: "test".into(),
    };
    let report = Report::from_run(ctx, passport);
    let rendered = MarkdownRenderer.render(&report).unwrap();
    assert!(rendered.contains("| R-001 | Ф-1 |"));
    assert!(rendered.contains("| R-002 | Ф-2 |"));
    assert!(rendered.contains("Часть B. REST API / Общие параметры"));
    assert!(!rendered.contains("REST type: GET"));
    assert!(rendered.contains("Проверьте сумму"));
    assert!(rendered.contains("таблица 2 «Критерии приемки», строка 1"));
    assert!(rendered.contains("таблица 5 «Общие параметры», строка 1"));
    assert!(!rendered.contains("таблица #"));
}

#[tokio::test]
async fn segment_stage_extracts_id_from_heading_and_mid_sentence() {
    struct HeadingIdProvider;

    #[async_trait]
    impl LlmProvider for HeadingIdProvider {
        fn model(&self) -> &str {
            "stub"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            let json = if request.stage == "segment" {
                let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
                json!(input["blocks"].as_array().unwrap().iter().filter_map(|block| {
                    let line = block["location"]["start_line"].as_u64()?;
                    [5, 7, 11].contains(&line).then(|| json!({
                        "block_id": block["id"], "start_line": line, "end_line": line, "kind": "requirement"
                    }))
                }).collect::<Vec<_>>())
            } else {
                json!([])
            };
            Ok(LlmResponse {
                json,
                usage: Usage::default(),
                model: self.model().into(),
            })
        }
    }

    let source = concat!(
        "# Требования\n\n",
        "#### ФТ-1.2 Расчёт лимита\n\n",
        "Система рассчитывает лимит по календарному месяцу.\n\n",
        "Требование Ф-5: сумма не превышает дневной лимит.\n\n",
        "#### Требования НФ-1 – НФ-5\n\n",
        "Лимит применяется аналогично Ф-1 — по календарному месяцу.\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("ids.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &HeadingIdProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.requirements.len(), 3);
    assert_eq!(ctx.requirements[0].source_id.as_deref(), Some("ФТ-1.2"));
    assert_eq!(ctx.requirements[1].source_id.as_deref(), Some("Ф-5"));
    assert_eq!(ctx.requirements[2].source_id, None);
}

#[tokio::test]
async fn segment_stage_handles_table_only_document_without_llm_call() {
    struct FailIfCalledProvider;

    #[async_trait]
    impl LlmProvider for FailIfCalledProvider {
        fn model(&self) -> &str {
            "fail"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "segment" {
                panic!("LLM не должен вызываться для документа только из таблиц");
            }
            Ok(LlmResponse {
                json: json!([]),
                usage: Usage::default(),
                model: self.model().into(),
            })
        }
    }

    let source = "## Часть B. REST API\n\n### Query-параметры\n\n| Parameter | Format |\n| --- | --- |\n| `amount` | integer |\n";
    let document = ParserRegistry::default()
        .parse(Path::new("table_only.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &FailIfCalledProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert!(ctx.requirements.is_empty());
    assert_eq!(ctx.context_items.len(), 1);
    assert_eq!(ctx.context_items[0].id, "C-001");
    assert_eq!(
        ctx.context_items[0].part.as_deref(),
        Some("Часть B. REST API")
    );
    assert_eq!(
        ctx.context_items[0].section.as_deref(),
        Some("Query-параметры")
    );
    assert_eq!(ctx.context_items[0].table.as_ref().unwrap().index, 1);
    assert_eq!(
        ctx.context_items[0].location,
        SourceSpan {
            start_line: 7,
            end_line: 7,
            page: None,
        }
    );
}

#[tokio::test]
async fn sample_mock_keeps_api_context_out_of_the_requirement_count() {
    let document = ParserRegistry::default()
        .parse(
            Path::new("requirements.md"),
            include_bytes!("../../../samples/requirements.md"),
        )
        .unwrap();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &llm::MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert!(
        (18..=30).contains(&ctx.requirements.len()),
        "{}",
        ctx.requirements.len()
    );
    for id in ["Ф-1", "Ф-2", "Ф-3", "Ф-4"] {
        assert_eq!(
            ctx.requirements
                .iter()
                .filter(|req| req.source_id.as_deref() == Some(id))
                .count(),
            1
        );
    }
    assert!(
        ctx.requirements
            .iter()
            .all(|req| !req.text.starts_with('{') && !req.quote.contains("REST type |"))
    );
    assert!(
        ctx.context_items
            .iter()
            .any(|item| item.quote.contains("REST type | GET"))
    );
    assert!(
        ctx.context_items
            .iter()
            .any(|item| item.quote.contains("\"result\":\"ERROR\""))
    );
    assert!(
        ctx.context_items
            .iter()
            .all(|item| ctx.document.contains_quote(item.location, &item.quote))
    );
}

#[test]
fn joins_behavior_rows_in_a_passport_under_an_arbitrary_heading() {
    let source = concat!(
        "## Настройка метода\n\n",
        "| Поле | Значение |\n",
        "| --- | --- |\n",
        "| Description | Сохраняет настройку. |\n",
        "| Author | Команда продукта |\n",
        "| Description | Возвращает результат. |\n",
    );
    let doc = ParserRegistry::default()
        .parse(Path::new("api.md"), source.as_bytes())
        .unwrap();
    let requirements = extract_table_requirements(&doc);
    assert_eq!(requirements.len(), 1);
    let req = &requirements[0];
    assert_eq!(req.text, "Сохраняет настройку.\nВозвращает результат.");
    assert_eq!((req.location.start_line, req.location.end_line), (5, 7));
    assert!(doc.contains_quote(req.location, &req.quote));
    assert!(!doc.contains_quote(
        SourceSpan {
            end_line: 6,
            ..req.location
        },
        &req.quote
    ));
}

#[tokio::test]
async fn business_status_prose_stays_in_requirements_while_http_statuses_are_context() {
    let source = concat!(
        "# Спецификация заявки\n\n",
        "## Статусная модель заявки\n\n",
        "Система переводит заявку в статус «Одобрена» после решения.\n\n",
        "HTTP 429 возвращается при превышении лимита запросов.\n\n",
        "## Статусы\n\n",
        "Статус «На рассмотрении» доступен только сотруднику.\n\n",
        "**400.** Запрос содержит ошибку.\n\n",
        "### 403. Forbidden\n\n",
        "#### Пример\n\n",
        "Для чужой заявки возвращается 404.\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("statuses.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::default();
    struct StatusProvider;
    #[async_trait]
    impl LlmProvider for StatusProvider {
        fn model(&self) -> &str {
            "test-statuses"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage != "segment" {
                return llm::MockProvider.generate(request).await;
            }
            let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
            let blocks = input["blocks"].as_array().unwrap();
            let refs: Vec<_> = [
                "Система переводит заявку",
                "Статус «На рассмотрении»",
                "HTTP 429 возвращается",
            ]
            .iter()
            .map(|prefix| {
                let block = blocks
                    .iter()
                    .find(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with(*prefix))
                    })
                    .unwrap_or_else(|| panic!("Строка не передана в нарезку: {prefix}"));
                json!({
                    "block_id": block["id"], "kind":"requirement",
                    "start_line":block["location"]["start_line"],
                    "end_line":block["location"]["end_line"],
                })
            })
            .collect();
            Ok(LlmResponse {
                json: json!(refs),
                usage: Usage::default(),
                model: self.model().into(),
            })
        }
    }

    let mut ctx = RunContext::new(document, &StatusProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.requirements.len(), 3);
    assert!(
        ctx.requirements
            .iter()
            .any(|r| r.quote.starts_with("HTTP 429"))
    );
    assert_eq!(
        ctx.context_items
            .iter()
            .map(|item| item.quote.as_str())
            .collect::<Vec<_>>(),
        [
            "400. Запрос содержит ошибку.",
            "403. Forbidden",
            "Для чужой заявки возвращается 404.",
        ]
    );
}
