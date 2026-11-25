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

    // 2. Общие параметры API (часть B): таблица №5, есть Description и REST type.
    let api_general: Vec<_> = requirements
        .iter()
        .filter(|req| req.section.as_deref() == Some("Общие параметры"))
        .collect();
    assert_eq!(api_general.len(), 12);
    assert!(
        api_general
            .iter()
            .all(|req| req.part.as_deref() == Some("Часть B. REST API"))
    );
    let description = api_general
        .iter()
        .find(|req| req.text.starts_with("Description:"))
        .expect("Нет строки Description");
    assert_eq!(description.location.start_line, 103);
    assert!(description.quote.contains("чтение порога"));
    let rest_type = api_general
        .iter()
        .find(|req| req.text == "REST type: GET")
        .expect("Нет строки REST type");
    assert_eq!(rest_type.location.start_line, 104);
    assert_eq!(rest_type.table.as_ref().unwrap().index, 5);
    assert_eq!(rest_type.table.as_ref().unwrap().row, 2);

    // 3. Query-параметры API (часть B): таблица №6, cardId, amount, enabled (строки 142–144).
    let query_params: Vec<_> = requirements
        .iter()
        .filter(|req| req.section.as_deref() == Some("Параметры запроса / Query-параметры"))
        .collect();
    assert_eq!(query_params.len(), 3);
    assert_eq!(
        query_params
            .iter()
            .map(|req| req.location.start_line)
            .collect::<Vec<_>>(),
        [142, 143, 144]
    );
    assert!(query_params[1].text.contains("Parameter: amount"));
    assert!(query_params[1].text.contains("Format: integer"));
    assert_eq!(query_params[1].table.as_ref().unwrap().index, 6);
    assert_eq!(
        query_params[1].table.as_ref().unwrap().title.as_deref(),
        Some("Query-параметры")
    );

    // 4. Параметры ответа API (часть B): таблица №7, result, amount, enabled, message (строки 155–158).
    let response_params: Vec<_> = requirements
        .iter()
        .filter(|req| req.section.as_deref() == Some("Ответ"))
        .collect();
    assert_eq!(response_params.len(), 4);
    assert_eq!(
        response_params
            .iter()
            .map(|req| req.location.start_line)
            .collect::<Vec<_>>(),
        [155, 156, 157, 158]
    );
    assert!(response_params[1].text.contains("Parameter: amount"));
    assert!(response_params[1].text.contains("Format: string"));
    assert_eq!(response_params[1].table.as_ref().unwrap().index, 7);

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
        "| Поле | Значение |\n",
        "| --- | --- |\n",
        "| Продукт | Мобильный банк |\n\n",
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
    assert_eq!(requirements[0].table.as_ref().unwrap().index, 2);
    assert_eq!(requirements[1].section.as_deref(), Some("3. Ревью заявки"));
    assert_eq!(requirements[1].table.as_ref().unwrap().index, 3);

    // Заголовок «Часть ответа с ошибками» — раздел, а не часть документа (A/B).
    assert_eq!(requirements[2].part, None);
    assert_eq!(
        requirements[2].section.as_deref(),
        Some("4. Часть ответа с ошибками")
    );
    assert_eq!(requirements[2].table.as_ref().unwrap().index, 4);
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
        let json = if request.stage == "segment" {
            // Модель возвращает Ф-1..Ф-4 без source_id (для Ф-2 даже без префикса в цитате),
            // абзац ответа на строке 151 со слегка смещённым диапазоном (151..=153, касающимся таблицы),
            // и попытку продублировать строку таблицы на строке 63.
            json!([
                {
                    "text": "Держатель активной карты KZT задаёт порог от 1 000 до 1 000 000 ₸.",
                    "quote": "Ф-1. Держатель активной карты KZT задаёт целый порог от 1 000 до 1 000 000 ₸ включительно. По умолчанию уведомление выключено.",
                    "location": { "start_line": 37, "end_line": 37, "page": null }
                },
                {
                    "text": "Месяц определяется по дате совершения покупки в Алматы.",
                    "quote": "Месяц определяется по дате совершения покупки в Алматы.",
                    "location": { "start_line": 38, "end_line": 38, "page": null }
                },
                {
                    "text": "При достижении порога отправляется максимум один push за календарный месяц.",
                    "quote": "Ф-3. Когда расходы переходят от суммы ниже порога к сумме, равной порогу или выше, клиент получает push. На карту приходится максимум один такой push за календарный месяц.",
                    "location": { "start_line": 39, "end_line": 39, "page": null }
                },
                {
                    "text": "При изменении порога накопленная сумма сохраняется.",
                    "quote": "Ф-4. При изменении порога накопленная сумма сохраняется. Если она ниже нового порога, следующее достижение вызывает push независимо от истории предыдущей настройки.",
                    "location": { "start_line": 40, "end_line": 40, "page": null }
                },
                {
                    "text": "Дубль строки критериев приёмки от модели",
                    "quote": "Итог 50 000 ₸ и один push",
                    "location": { "start_line": 63, "end_line": 63, "page": null }
                },
                {
                    "text": "Успешный ответ — 200 или 201, неуспешный — от 400.",
                    "quote": "Если результат 200 или 201 — успешный. Если результат 400, 401 и далее — неуспешный.",
                    "location": { "start_line": 151, "end_line": 153, "page": null }
                }
            ])
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

    // Всего 4 функциональных + 1 абзац ответа + 22 табличных = 27 требований с нумерацией R-001..R-027.
    assert_eq!(ctx.requirements.len(), 27);
    assert_eq!(ctx.requirements[0].id, "R-001");
    assert_eq!(ctx.requirements[26].id, "R-027");

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
    assert!(rendered.contains("REST type: GET"));
    assert!(rendered.contains("таблица 2 «Критерии приемки», строка 1"));
    assert!(rendered.contains("таблица 5 «Общие параметры», строка 2"));
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
                json!([
                    {
                        "text": "Система рассчитывает лимит по календарному месяцу.",
                        "quote": "Система рассчитывает лимит по календарному месяцу.",
                        "location": { "start_line": 5, "end_line": 5, "page": null }
                    },
                    {
                        "text": "Требование Ф-5: сумма не превышает дневной лимит.",
                        "quote": "Требование Ф-5: сумма не превышает дневной лимит.",
                        "location": { "start_line": 7, "end_line": 7, "page": null }
                    }
                ])
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
        "Требование Ф-5: сумма не превышает дневной лимит.\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("ids.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &HeadingIdProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.requirements.len(), 2);
    assert_eq!(ctx.requirements[0].source_id.as_deref(), Some("ФТ-1.2"));
    assert_eq!(ctx.requirements[1].source_id.as_deref(), Some("Ф-5"));
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

    assert_eq!(ctx.requirements.len(), 1);
    assert_eq!(ctx.requirements[0].id, "R-001");
    assert_eq!(
        ctx.requirements[0].part.as_deref(),
        Some("Часть B. REST API")
    );
    assert_eq!(
        ctx.requirements[0].section.as_deref(),
        Some("Query-параметры")
    );
    assert_eq!(ctx.requirements[0].table.as_ref().unwrap().index, 1);
    assert_eq!(
        ctx.requirements[0].location,
        SourceSpan {
            start_line: 7,
            end_line: 7,
            page: None,
        }
    );
}
