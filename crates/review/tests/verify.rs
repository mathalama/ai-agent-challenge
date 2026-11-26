use std::path::Path;

use async_trait::async_trait;
use doc::{ParserRegistry, SourceSpan};
use llm::{LlmProvider, LlmRequest, LlmResponse, MockProvider, Usage};
use review::{
    Check, CheckPhase, CheckRegistry, Criterion, Detector, Evidence, Finding, MarkdownRenderer,
    Pipeline, Readiness, Report, ReportRenderer, RunContext, RunPassport, Scope, Severity,
    StageResult,
};

struct SampleCheck;

#[async_trait]
impl Check for SampleCheck {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    async fn run(&self, _ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        Ok([
            ("F-1", "Дословная строка", 2),
            ("F-2", "Дословная строка", 3),
            ("F-3", "", 2),
            ("F-4", "Ф-1. Держатель \"активной карты\"", 5),
            ("F-5", "Текст ячейки", 7),
        ]
        .into_iter()
        .map(|(id, quote, line)| Finding {
            id: id.into(),
            scope: Scope::Set,
            criterion: Criterion::Consistent,
            additional_criteria: Vec::new(),
            severity: Severity::Critical,
            evidence: vec![Evidence {
                quote: quote.into(),
                location: SourceSpan {
                    start_line: line,
                    end_line: line,
                    page: None,
                },
            }],
            explanation: "Замечание для проверки доказательств".into(),
            suggestion: None,
            detector: Detector::Rule("sample".into()),
            additional_detectors: Vec::new(),
            merged_ids: Vec::new(),
            fallback_evidence: false,
            verified: true,
        })
        .collect())
    }
}

#[tokio::test]
async fn verify_checks_quote_in_its_declared_span() {
    let source = concat!(
        "Первая строка\r\nДословная строка\r\nПоследняя строка\r\n\r\n",
        "* **Ф-1.** Держатель «активной\u{a0}карты»\r\n\r\n",
        "| Текст **ячейки** |\r\n| --- |\r\n"
    );
    let document = ParserRegistry::default()
        .parse(Path::new("source.md"), source.as_bytes())
        .unwrap();
    let mut checks = CheckRegistry::default();
    checks.register(SampleCheck);
    let mut ctx = RunContext::new(document, &MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();
    let verified: Vec<_> = ctx
        .findings
        .iter()
        .filter(|finding| finding.verified)
        .map(|finding| finding.id.as_str())
        .collect();
    assert_eq!(verified, ["F-1", "F-4", "F-5"]);
}

struct ReviewProvider(Option<(Severity, &'static str)>, usize);

#[async_trait]
impl LlmProvider for ReviewProvider {
    fn model(&self) -> &str {
        "test"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage == "segment" {
            let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
            assert_eq!(input["blocks"].as_array().unwrap().len(), self.1);
            return MockProvider.generate(request).await;
        }
        let mut findings = Vec::new();
        if request.stage == "set_checks"
            && let Some((severity, quote)) = self.0
        {
            findings.push(Finding {
                id: "F-1".into(),
                scope: Scope::Set,
                criterion: Criterion::Appropriate,
                additional_criteria: Vec::new(),
                severity,
                evidence: vec![Evidence {
                    quote: quote.into(),
                    location: SourceSpan {
                        start_line: 3,
                        end_line: 3,
                        page: None,
                    },
                }],
                explanation: "Замечание".into(),
                suggestion: None,
                detector: Detector::Llm(self.model().into()),
                additional_detectors: Vec::new(),
                merged_ids: Vec::new(),
                fallback_evidence: false,
                verified: true,
            });
        }
        Ok(LlmResponse {
            reasoning: None,
            json: serde_json::to_value(
                findings
                    .into_iter()
                    .map(|finding| {
                        serde_json::json!({
                            "requirement_id": null,
                            "severity": finding.severity,
                            "criterion": finding.criterion,
                            "evidence": finding.evidence,
                            "explanation": finding.explanation,
                            "suggestion": finding.suggestion,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn review_readiness_requires_the_rules_stage_to_run_registered_checks() {
    for (rules_phase, finding, expected) in [
        (None, None, "checks_incomplete"),
        (Some(CheckPhase::Set), None, "checks_incomplete"),
        (
            None,
            Some((Severity::Critical, "Первое требование")),
            "blocked",
        ),
        (Some(CheckPhase::Rules), None, "ready"),
        (
            Some(CheckPhase::Rules),
            Some((Severity::Low, "Первое требование")),
            "ready",
        ),
        (
            Some(CheckPhase::Rules),
            Some((Severity::High, "Первое требование")),
            "blocked",
        ),
        (
            Some(CheckPhase::Rules),
            Some((Severity::Critical, "Вымышленная цитата")),
            "checks_incomplete",
        ),
        (
            Some(CheckPhase::Rules),
            Some((Severity::Medium, "Первое требование")),
            "blocked",
        ),
        (
            Some(CheckPhase::Rules),
            Some((Severity::Critical, "Первое требование")),
            "blocked",
        ),
    ] {
        let document = ParserRegistry::default()
            .parse(
                Path::new("input.md"),
                "# Документ требований (версия 1.0 от 2026-03-10)\n\n* **Ф-1.** Первое требование\n\n* **Ф-2.** Второе требование\n\n## 5. Тестирование и приёмка:\n\nПроверить, что пустой ввод вызывает ошибку.".as_bytes(),
            )
            .unwrap();
        let provider = ReviewProvider(finding, 5);
        let checks = match rules_phase {
            Some(CheckPhase::Rules) => CheckRegistry::with_builtin_rules(),
            Some(phase) => {
                let mut checks = CheckRegistry::default();
                checks.register(EmptyCheck(phase));
                checks
            }
            None => CheckRegistry::default(),
        };
        let mut ctx = RunContext::new(document, &provider, &checks);
        Pipeline::default().run(&mut ctx).await.unwrap();
        let passport = RunPassport {
            document_title: ctx.document.title.clone(),
            date: String::new(),
            model: provider.model().into(),
            agent_version: String::new(),
        };
        let readiness: Readiness = Report::from_run(ctx, passport).readiness;
        assert_eq!(serde_json::to_value(readiness).unwrap(), expected);
    }
}

#[tokio::test]
async fn readiness_requires_testing_section_coverage() {
    let source = "# Спецификация\n\n* **Ф-1.** Сохранять изменения.";
    let checks = CheckRegistry::with_builtin_rules();
    for (path, expected) in [("input.md", "blocked"), ("input.txt", "checks_incomplete")] {
        let document = ParserRegistry::default()
            .parse(Path::new(path), source.as_bytes())
            .unwrap();
        let provider = ReviewProvider(None, 2);
        let mut ctx = RunContext::new(document, &provider, &checks);
        Pipeline::default().run(&mut ctx).await.unwrap();
        let passport = RunPassport {
            document_title: ctx.document.title.clone(),
            date: String::new(),
            model: provider.model().into(),
            agent_version: String::new(),
        };
        let report = Report::from_run(ctx, passport);
        assert_eq!(
            serde_json::to_value(report.readiness).unwrap(),
            expected,
            "{path}"
        );
        if path == "input.md" {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding.id == "testing-section-missing" && finding.verified)
            );
        }
    }
}

struct EmptyCheck(CheckPhase);

#[async_trait]
impl Check for EmptyCheck {
    fn phase(&self) -> CheckPhase {
        self.0
    }

    async fn run(&self, _ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        Ok(Vec::new())
    }
}

struct LanguageCheck;

#[async_trait]
impl Check for LanguageCheck {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    async fn run(&self, _ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let evidence = vec![Evidence {
            quote: "Второе требование".into(),
            location: SourceSpan {
                start_line: 4,
                end_line: 4,
                page: None,
            },
        }];
        Ok(vec![
            Finding {
                id: "RULE-EN".into(),
                scope: Scope::Set,
                criterion: Criterion::Unambiguous,
                additional_criteria: Vec::new(),
                severity: Severity::High,
                evidence: evidence.clone(),
                explanation: "English explanation without Cyrillic".into(),
                suggestion: Some("Rewrite clearly".into()),
                detector: Detector::Rule("lang".into()),
                additional_detectors: Vec::new(),
                merged_ids: Vec::new(),
                fallback_evidence: false,
                verified: false,
            },
            Finding {
                id: "RULE-RU".into(),
                scope: Scope::Set,
                criterion: Criterion::Appropriate,
                additional_criteria: Vec::new(),
                severity: Severity::Medium,
                evidence,
                explanation: "В методе GET переданы лишние детали реализации".into(),
                suggestion: Some("Move HTTP details to technical spec".into()),
                detector: Detector::Rule("lang".into()),
                additional_detectors: Vec::new(),
                merged_ids: Vec::new(),
                fallback_evidence: false,
                verified: false,
            },
        ])
    }
}

struct LanguageAndPromptProvider {
    seen_prompts: std::sync::Mutex<Vec<(String, String)>>,
}

impl LanguageAndPromptProvider {
    fn new() -> Self {
        Self {
            seen_prompts: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmProvider for LanguageAndPromptProvider {
    fn model(&self) -> &str {
        "test-prompts"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        self.seen_prompts
            .lock()
            .unwrap()
            .push((request.stage.clone(), request.system_prompt.clone()));
        let json = match request.stage.as_str() {
            "segment" => {
                let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
                serde_json::json!(input["blocks"].as_array().unwrap().iter()
                    .filter(|block| block["kind"] == "list_item")
                    .map(|block| serde_json::json!({"block_id": block["id"], "start_line": block["location"]["start_line"], "end_line": block["location"]["end_line"], "kind": "requirement"}))
                    .collect::<Vec<_>>())
            }
            "facts" => {
                let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
                let rows = input["requirements"].as_array().unwrap();
                serde_json::json!(
                    rows.iter()
                        .map(|row| serde_json::json!({
                            "requirement_id": row["id"], "entity": "отчётность",
                            "quote": null,
                            "data": {"kind": "policy", "statement": row["text"]}
                        }))
                        .collect::<Vec<_>>()
                )
            }
            "requirement_checks" => serde_json::json!([
                {
                    "requirement_id": "R-001",
                    "criterion": "verifiable",
                    "severity": "high",
                    "evidence": [{
                        "quote": "Первое требование",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Requirement is not verifiable.",
                    "suggestion": "Add numeric thresholds."
                },
                {
                    "requirement_id": "R-001",
                    "criterion": "appropriate",
                    "severity": "medium",
                    "evidence": [{
                        "quote": "Первое требование",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Использование HTTP GET навязывает проектное решение.",
                    "suggestion": "Use POST instead."
                },
                {
                    "requirement_id": "R-001",
                    "criterion": "complete",
                    "severity": "medium",
                    "evidence": [{
                        "quote": "Первое требование",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Не описана реакция на ошибку ввода.",
                    "suggestion": "Указать код ошибки HTTP 400 и текст сообщения."
                },
                {
                    "requirement_id": "R-001",
                    "criterion": "verifiable",
                    "severity": "high",
                    "evidence": [{
                        "quote": "Первое требование",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Не описан сценарий 보안-тестирования.",
                    "suggestion": null
                },
                {
                    "requirement_id": "R-001",
                    "criterion": "ranked",
                    "severity": "high",
                    "evidence": [{
                        "quote": "Первое требование",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Не определены параметры σ и Δ.",
                    "suggestion": "Указать значения для 安全."
                }
            ]),
            "clarify" => serde_json::json!([
                {
                    "text": "What error code should be returned?",
                    "why": "Needed for client integration.",
                    "topic": "api",
                    "options": ["HTTP 400", "HTTP 422"],
                    "finding_ids": ["requirement_checks-001"]
                },
                {
                    "text": "Какой код ошибки возвращать при невалидном вводе?",
                    "why": "English rationale without Cyrillic.",
                    "topic": "api",
                    "options": ["HTTP 400", "HTTP 422"],
                    "finding_ids": ["requirement_checks-001"]
                },
                {
                    "text": "Какой HTTP-статус возвращать при ошибке валидации: 400 или 422?",
                    "why": "Влияет на обработку ошибок в мобильном приложении.",
                    "topic": "api",
                    "options": ["HTTP 400", "HTTP 422"],
                    "finding_ids": ["requirement_checks-002"]
                },
                {
                    "text": " какой  HTTP-статус возвращать при ошибке валидации: 400 или 422? ",
                    "why": "Влияет на интеграцию.",
                    "topic": "api",
                    "options": ["HTTP 422", "HTTP 400", "HTTP 409", "HTTP 500"],
                    "finding_ids": ["requirement_checks-002", "unknown-finding"]
                }
            ]),
            _ => serde_json::json!([]),
        };
        Ok(LlmResponse {
            reasoning: None,
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn validates_finding_and_question_languages_and_attaches_domain_context() {
    let document = ParserRegistry::default()
        .parse(
            Path::new("input.md"),
            "# Документ требований\n\n* **Ф-1.** Первое требование (σ и Δ)\n* **Ф-2.** Второе требование\n"
                .as_bytes(),
        )
        .unwrap();
    let provider = LanguageAndPromptProvider::new();
    let mut checks = CheckRegistry::default();
    checks.register(LanguageCheck);
    let mut ctx = RunContext::new(document, &provider, &checks);

    Pipeline::default().run(&mut ctx).await.unwrap();

    let ids: Vec<_> = ctx.findings.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "RULE-RU",
            "requirement_checks-001",
            "requirement_checks-002",
            "requirement_checks-004"
        ]
    );
    assert!(ctx.findings[0].suggestion.is_none());
    assert!(ctx.findings[1].suggestion.is_none());
    assert_eq!(
        ctx.findings[2].suggestion.as_deref(),
        Some("Указать код ошибки HTTP 400 и текст сообщения.")
    );
    assert!(ctx.findings[3].verified);
    assert_eq!(
        ctx.findings[3].explanation,
        "Не определены параметры σ и Δ."
    );
    assert!(ctx.findings[3].suggestion.is_none());
    assert_eq!(ctx.questions.len(), 1);
    assert!(ctx.questions[0].id.starts_with("Q-"));
    assert_eq!(ctx.questions[0].priority, Severity::Medium);
    assert_eq!(ctx.questions[0].related.len(), 1);
    assert_eq!(
        ctx.questions[0].options,
        ["HTTP 400", "HTTP 422", "HTTP 409"]
    );
    assert_eq!(ctx.questions[0].evidence[0].quote, "Первое требование");
    assert!(ctx.questions[0].text.contains("HTTP-статус"));

    let prompts = provider.seen_prompts.lock().unwrap();
    let stages: Vec<_> = prompts.iter().map(|(stage, _)| stage.as_str()).collect();
    assert_eq!(
        stages,
        [
            "segment",
            "facts",
            "requirement_checks",
            "set_checks",
            "consistency",
            "ambiguity",
            "clarify",
        ]
    );
    let forbidden_sample_fragments = [
        "Уведомление о расходах по карте",
        "AlertsController",
        "saveLimit",
        "cardId",
        "limit_kzt",
        "demo-42",
        "заполняются тестировщиками",
        "исходный итог",
        "прежнее состояние",
        "Алматы",
        "48 500",
        "19 900",
        "Проверьте сумму",
        "Карта не найдена",
    ];
    for (stage, prompt) in prompts.iter() {
        assert!(
            prompt.contains("UTC+5") && prompt.contains("НБ РК") && prompt.contains("100 тиынов"),
            "Стадия {stage} не содержит контекст предметной области"
        );
        assert!(
            prompt.contains("Примеры")
                && prompt.contains("Плохо")
                && prompt.contains("Как исправить"),
            "Стадия {stage} не содержит примеров «плохо → почему → как исправить»"
        );
        for fragment in forbidden_sample_fragments {
            assert!(
                !prompt.contains(fragment),
                "Промпт стадии {stage} содержит фрагмент из samples/requirements.md: {fragment}"
            );
        }
    }
    let req_prompt = &prompts
        .iter()
        .find(|(stage, _)| stage == "requirement_checks")
        .unwrap()
        .1;
    assert!(req_prompt.contains("ISO/IEC/IEEE 29148:2018 § 5.2.5"));
    assert!(req_prompt.contains("appropriate"));
    assert!(req_prompt.contains("лишние проектные решения"));

    let set_prompt = &prompts
        .iter()
        .find(|(stage, _)| stage == "set_checks")
        .unwrap()
        .1;
    assert!(set_prompt.contains("ISO/IEC/IEEE 29148:2018 § 5.2.6"));
    assert!(set_prompt.contains("IEEE Std 830-1998 § 4.3"));
}
struct DuplicateStagesProvider;

#[async_trait]
impl LlmProvider for DuplicateStagesProvider {
    fn model(&self) -> &str {
        "test-dedup"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage == "segment" {
            return MockProvider.generate(request).await;
        }
        let json = match request.stage.as_str() {
            "requirement_checks" => serde_json::json!([
                {
                    "requirement_id": "R-001",
                    "criterion": "complete",
                    "severity": "medium",
                    "evidence": [{
                        "quote": "Повторная отправка сохраняет прежний статус операции.",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Не указан код ответа при повторной отправке.",
                    "suggestion": "Зафиксировать идемпотентный ответ HTTP 200."
                }
            ]),
            "ambiguity" => serde_json::json!([
                {
                    "requirement_id": "R-001",
                    "criterion": "unambiguous",
                    "severity": "high",
                    "evidence": [{
                        "quote": "сохраняет прежний статус операции",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Фраза допускает два прочтения: статус до ошибки или последний сохранённый статус.",
                    "suggestion": "Явно описать, какой именно статус возвращается."
                }
            ]),
            _ => serde_json::json!([]),
        };
        Ok(LlmResponse {
            reasoning: None,
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn pipeline_merges_duplicate_cross_stage_findings_before_verify_and_renders_sources() {
    let source = concat!(
        "# Спецификация (версия 1.0 от 2026-03-10)\n\n",
        "* **Ф-1.** Повторная отправка сохраняет прежний статус операции.\n\n",
        "## Тестирование и приёмка\n\n",
        "Проверить повторную доставку одного и того же события.\n"
    );
    let document = ParserRegistry::default()
        .parse(Path::new("spec.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &DuplicateStagesProvider, &checks);

    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.findings.len(), 1, "{:#?}", ctx.findings);
    let merged = &ctx.findings[0];
    assert!(merged.verified);
    assert_eq!(merged.severity, Severity::High);
    assert_eq!(
        merged.all_criteria(),
        vec![Criterion::Complete, Criterion::Unambiguous]
    );
    assert_eq!(merged.all_detectors().len(), 2);
    assert_eq!(
        merged.all_ids(),
        vec!["requirement_checks-001", "ambiguity-001"]
    );

    let passport = RunPassport {
        document_title: ctx.document.title.clone(),
        date: "2026-03-10".into(),
        model: "test-dedup".into(),
        agent_version: "0.1.0".into(),
    };
    let report = Report::from_run(ctx, passport);
    let md = MarkdownRenderer.render(&report).unwrap();
    assert!(md.contains("Источники —"));
    assert!(md.contains("requirement\\_checks"));
    assert!(md.contains("ambiguity"));
    assert!(md.contains("Полнота требования"));
    assert!(md.contains("Однозначность"));
}

struct UnverifiedParaphraseProvider;

#[async_trait]
impl LlmProvider for UnverifiedParaphraseProvider {
    fn model(&self) -> &str {
        "test-paraphrase"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage == "segment" {
            return MockProvider.generate(request).await;
        }
        let json = match request.stage.as_str() {
            "ambiguity" => serde_json::json!([
                {
                    "requirement_id": "R-001",
                    "criterion": "unambiguous",
                    "severity": "medium",
                    "evidence": [{
                        "quote": "Сервис отвечает быстро на запросы клиентов",
                        "location": {"start_line": 3, "end_line": 3, "page": null}
                    }],
                    "explanation": "Формулировка «быстро» не задаёт измеримого SLA.",
                    "suggestion": "Указать предельное время ответа в миллисекундах."
                }
            ]),
            _ => serde_json::json!([]),
        };
        Ok(LlmResponse {
            reasoning: None,
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn verified_rule_finding_keeps_its_quote_when_model_paraphrases_it() {
    let source = concat!(
        "# Спецификация (версия 1.0 от 2026-03-10)\n\n",
        "* **Ф-1.** Сервис отвечает быстро на запросы пользователя.\n\n",
        "## Тестирование и приёмка\n\n",
        "Проверить время ответа сервиса под нагрузкой.\n"
    );
    let document = ParserRegistry::default()
        .parse(Path::new("spec.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &UnverifiedParaphraseProvider, &checks);

    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.findings.len(), 2, "{:#?}", ctx.findings);
    let rule_finding = &ctx.findings[0];
    assert!(matches!(&rule_finding.detector, Detector::Rule(id) if id == "vague-terms"));
    assert!(rule_finding.verified);
    assert_eq!(rule_finding.severity, Severity::Medium);
    assert!(rule_finding.merged_ids.is_empty());
    assert_eq!(ctx.findings[1].id, "ambiguity-001");
    assert!(!ctx.findings[1].verified);
    assert_eq!(rule_finding.evidence.len(), 1);
    assert_eq!(
        rule_finding.evidence[0].quote,
        "Ф-1. Сервис отвечает быстро на запросы пользователя."
    );

    let passport = RunPassport {
        document_title: ctx.document.title.clone(),
        date: "2026-03-10".into(),
        model: "test-paraphrase".into(),
        agent_version: "0.1.0".into(),
    };
    let report = Report::from_run(ctx, passport);
    assert!(matches!(report.readiness, Readiness::Blocked));
}

#[tokio::test]
async fn weak_document_keeps_separate_missing_section_and_version_findings() {
    let source = "# Спецификация\n\n* **Ф-1.** Сохранять настройки профиля.\n";
    let document = ParserRegistry::default()
        .parse(Path::new("spec.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &MockProvider, &checks);

    Pipeline::default().run(&mut ctx).await.unwrap();

    let ids: Vec<&str> = ctx.findings.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"testing-section-missing"), "{ids:?}");
    assert!(ids.contains(&"document-version-date-missing"), "{ids:?}");
    assert!(ids.contains(&"acceptance-criteria-missing"), "{ids:?}");
    assert!(ctx.findings.iter().all(|f| f.verified));
}

struct DuplicateProvider;

#[async_trait]
impl LlmProvider for DuplicateProvider {
    fn model(&self) -> &str {
        "test-duplicates"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        let draft = |requirement_id: Option<&str>, criterion, severity, line, quote| {
            serde_json::json!({
                "requirement_id": requirement_id,
                "criterion": criterion,
                "severity": severity,
                "evidence": [{"quote": quote, "location": {
                    "start_line": line, "end_line": line, "page": null
                }}],
                "explanation": "Проверка требований.",
                "suggestion": null
            })
        };
        let json = match request.stage.as_str() {
            "requirement_checks" => serde_json::json!([
                draft(
                    Some("R-001"),
                    "complete",
                    "high",
                    1,
                    "Комиссия для ТСП (TBD)."
                ),
                draft(
                    Some("R-002"),
                    "verifiable",
                    "high",
                    2,
                    "Сервис выдерживает высокую нагрузку."
                ),
                draft(
                    Some("R-003"),
                    "appropriate",
                    "critical",
                    3,
                    "Полные номера карт записываются в журнал."
                ),
            ]),
            "set_checks" => serde_json::json!([
                draft(None, "set_complete", "high", 1, "Комиссия для ТСП (TBD)."),
                draft(
                    None,
                    "verifiable",
                    "high",
                    2,
                    "Сервис выдерживает высокую нагрузку."
                ),
                draft(
                    None,
                    "appropriate",
                    "critical",
                    3,
                    "Полные номера карт записываются в журнал."
                ),
            ]),
            "ambiguity" => serde_json::json!([
                draft(None, "unambiguous", "medium", 4, "В разумные сроки."),
                draft(None, "unambiguous", "medium", 5, "По возможности."),
                draft(None, "unambiguous", "medium", 6, "При необходимости."),
                draft(None, "unambiguous", "medium", 7, "ER-3. И т. п."),
                draft(
                    None,
                    "unambiguous",
                    "medium",
                    8,
                    "Интерфейс должен быть удобным."
                ),
                draft(
                    None,
                    "unambiguous",
                    "medium",
                    9,
                    "Журнал хранится достаточно долго."
                ),
            ]),
            _ => return MockProvider.generate(request).await,
        };
        Ok(LlmResponse {
            reasoning: None,
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn merges_single_requirement_duplicates_and_counts_critical_findings_once() {
    let source = concat!(
        "* FR-7. Комиссия для ТСП (TBD).\n",
        "* NFR-2. Сервис выдерживает высокую нагрузку.\n",
        "* NFR-4. Полные номера карт записываются в журнал.\n",
        "* FR-11. В разумные сроки.\n",
        "* FR-12. По возможности.\n",
        "* FR-5. При необходимости.\n",
        "* ER-3. И т. п.\n",
        "* NFR-5. Интерфейс должен быть удобным.\n",
        "* NFR-6. Журнал хранится достаточно долго.\n",
    );
    let document = ParserRegistry::default()
        .parse(Path::new("duplicates.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &DuplicateProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    for (stage, count, offset) in [("set_checks", 3, 0), ("ambiguity", 6, 3)] {
        for index in 1..=count {
            let id = format!("{stage}-{index:03}");
            let expected_scope = if id == "set_checks-001" {
                Scope::Set
            } else {
                Scope::Requirement(format!("R-{:03}", offset + index))
            };
            assert!(
                ctx.findings.iter().any(|finding| {
                    finding.verified
                        && finding.scope == expected_scope
                        && finding.merged_ids.contains(&id)
                }),
                "{id}: {:#?}",
                ctx.findings
            );
        }
    }

    let report = Report::from_run(
        ctx,
        RunPassport {
            document_title: "Требования".into(),
            date: String::new(),
            model: "test".into(),
            agent_version: String::new(),
        },
    );
    assert_eq!(
        report.summary.critical_findings,
        [
            "requirement_checks-001",
            "requirement_checks-002",
            "requirement_checks-003",
        ]
    );
    let markdown = MarkdownRenderer.render(&report).unwrap();
    assert!(markdown.contains("Критичные дефекты (непроверяемые, противоречивые, неполные) — 3:"));
    assert!(markdown.contains("Подтверждённых критичных дефектов — 1"));
}

#[tokio::test]
async fn keeps_the_pii_finding_without_a_convenience_warning() {
    let source = "# Требования\n\n* NFR-4. Для удобства отладки полные номера карт и счетов клиентов записываются в журнал приложения в открытом виде.";
    let document = ParserRegistry::default()
        .parse(Path::new("logging.md"), source.as_bytes())
        .unwrap();
    let provider = ReviewProvider(
        Some((
            Severity::Critical,
            "полные номера карт и счетов клиентов записываются в журнал приложения в открытом виде.",
        )),
        2,
    );
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &provider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    let pii = ctx
        .findings
        .iter()
        .find(|finding| finding.all_ids().contains(&"set_checks-001"))
        .unwrap_or_else(|| panic!("{:#?}", ctx.findings));
    assert!(pii.verified);
    assert_eq!(pii.scope, Scope::Requirement("R-001".into()));
    assert_eq!(pii.severity, Severity::Critical);
    assert!(
        !ctx.findings
            .iter()
            .flat_map(Finding::all_detectors)
            .any(|detector| { matches!(detector, Detector::Rule(id) if id == "vague-terms") })
    );
}
