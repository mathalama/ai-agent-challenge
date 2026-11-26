use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use doc::{Document, DocumentError, ParserRegistry};
use llm::{LlmProvider, LlmRequest, LlmResponse, MockProvider, Usage};
use review::{
    CheckRegistry, DocumentKind, MarkdownRenderer, Pipeline, Readiness, Report, ReportRenderer,
    RunContext, RunPassport, StageError,
};
use serde_json::json;

fn passport(title: &str) -> RunPassport {
    RunPassport {
        document_title: title.into(),
        date: "2026-11-26".into(),
        model: "mock".into(),
        agent_version: "0.1.0".into(),
    }
}

#[tokio::test]
async fn empty_md_and_txt_fail_without_calling_model() {
    let registry = ParserRegistry::default();

    for (name, bytes) in [
        ("empty.md", b"".as_slice()),
        ("spaces.md", b"   \n\t\n  ".as_slice()),
        ("empty.txt", b"   \r\n  ".as_slice()),
    ] {
        let error = registry.parse(Path::new(name), bytes).unwrap_err();
        assert!(
            matches!(error, DocumentError::EmptyDocument),
            "ожидали EmptyDocument для {name}, получили {error:?}"
        );
    }

    struct PanicProvider(AtomicUsize);

    #[async_trait]
    impl LlmProvider for PanicProvider {
        fn model(&self) -> &str {
            "panic"
        }

        async fn generate(&self, _request: &LlmRequest) -> llm::Result<LlmResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("модель не должна вызываться для пустого документа");
        }
    }

    let provider = PanicProvider(AtomicUsize::new(0));
    let checks = CheckRegistry::with_builtin_rules();
    let empty_doc = Document {
        title: "empty".into(),
        source_path: "empty.md".into(),
        blocks: Vec::new(),
    };
    let mut ctx = RunContext::new(empty_doc, &provider, &checks);
    let error = Pipeline::default().run(&mut ctx).await.unwrap_err();
    assert!(matches!(
        error,
        StageError::Stage {
            stage: "triage",
            source,
        } if matches!(*source, StageError::EmptyDocument)
    ));
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_requirements_document_stops_after_triage_without_formatting_findings() {
    let recipe = "\
# Рецепт борща

## Ингредиенты

- Свёкла — 2 шт.
- Капуста — 300 г
- Картофель — 3 шт.

## Приготовление

1. Сварить мясной бульон в течение полутора часов.
2. Натереть свёклу и обжарить с луком и морковью.
3. Добавить овощи в кастрюлю и варить ещё 20 минут.
";
    let document = ParserRegistry::default()
        .parse(Path::new("borscht.md"), recipe.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.trace.len(), 1);
    assert_eq!(ctx.trace[0].stage, "triage");
    assert!(ctx.requirements.is_empty());
    assert!(ctx.findings.is_empty());

    let report = Report::from_run(ctx, passport("Рецепт борща"));
    assert_eq!(report.readiness, Readiness::NotRequirements);
    let triage = report.triage.as_ref().expect("должно быть решение triage");
    assert_eq!(triage.kind, DocumentKind::NotRequirements);
    assert!(!triage.reason.is_empty());
    assert!(!triage.evidence.quote.is_empty());

    let md = MarkdownRenderer.render(&report).unwrap();
    assert!(md.contains("**Документ не является спецификацией требований.**"));
    assert!(md.contains(&triage.reason));
    assert!(md.contains(&triage.evidence.quote));
}

#[tokio::test]
async fn stub_document_continues_and_explains_missing_requirements() {
    let stub = "\
# Спецификация нового сервиса

## 1. Общие сведения

## 2. Функциональные требования

## 3. Тестирование
";
    let document = ParserRegistry::default()
        .parse(Path::new("stub.md"), stub.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.trace.len(), 10);
    let report = Report::from_run(ctx, passport("Спецификация нового сервиса"));
    assert_ne!(report.readiness, Readiness::NotRequirements);
    assert_eq!(
        report.triage.as_ref().map(|t| t.kind),
        Some(DocumentKind::Stub)
    );

    let md = MarkdownRenderer.render(&report).unwrap();
    assert!(md.contains("В документе нет содержательных требований:"));
}

#[tokio::test]
async fn unverified_triage_quote_is_ignored_and_pipeline_continues() {
    struct HallucinatedTriageProvider;

    #[async_trait]
    impl LlmProvider for HallucinatedTriageProvider {
        fn model(&self) -> &str {
            "hallucinated-triage"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "triage" {
                return Ok(LlmResponse {
                    json: json!({
                        "kind": "not_requirements",
                        "reason": "Это кулинарный рецепт, а не спецификация требований.",
                        "evidence": {
                            "quote": "Несуществующая цитата про борщ",
                            "location": { "start_line": 1, "end_line": 1, "page": null }
                        }
                    }),
                    usage: Usage::default(),
                    model: self.model().into(),
                    reasoning: None,
                });
            }
            MockProvider.generate(request).await
        }
    }

    let source = "\
# Спецификация уведомлений

- Ф-1. Система должна отправлять push при превышении порога.
";
    let document = ParserRegistry::default()
        .parse(Path::new("spec.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &HallucinatedTriageProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert!(ctx.triage.is_none());
    assert_eq!(ctx.trace.len(), 10);
    assert_eq!(ctx.requirements.len(), 1);
    let report = Report::from_run(ctx, passport("Спецификация уведомлений"));
    assert_ne!(report.readiness, Readiness::NotRequirements);
    assert!(report.triage.is_none());
}

#[tokio::test]
async fn real_requirements_document_passes_triage_and_runs_full_pipeline() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = workspace.join("samples/requirements.md");
    let bytes = std::fs::read(&path).unwrap();
    let document = ParserRegistry::default().parse(&path, &bytes).unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.trace.len(), 10);
    assert_eq!(
        ctx.triage.as_ref().map(|t| t.kind),
        Some(DocumentKind::Requirements)
    );
    let report = Report::from_run(ctx, passport("Уведомление о расходах по карте"));
    assert_eq!(report.readiness, Readiness::Blocked);
    assert!(report.triage.is_none());
    assert!(!report.requirements.is_empty());
    assert!(!report.findings.is_empty());
}

#[tokio::test]
async fn malformed_triage_response_does_not_fail_or_mark_incomplete() {
    struct BrokenTriageProvider;

    #[async_trait]
    impl LlmProvider for BrokenTriageProvider {
        fn model(&self) -> &str {
            "broken-triage"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "triage" {
                return Ok(LlmResponse {
                    json: json!({
                        "kind": "other",
                        "reason": "Неожиданный тип ответа"
                    }),
                    usage: Usage::default(),
                    model: self.model().into(),
                    reasoning: None,
                });
            }
            MockProvider.generate(request).await
        }
    }

    let source = "\
# Спецификация сервиса

- Ф-1. Система должна проверять подпись запроса.
";
    let document = ParserRegistry::default()
        .parse(Path::new("spec.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &BrokenTriageProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert!(ctx.triage.is_none());
    assert_eq!(ctx.trace.len(), 10);
    assert_eq!(ctx.requirements.len(), 1);
    let report = Report::from_run(ctx, passport("Спецификация сервиса"));
    assert!(report.incomplete_stages.is_empty());
    assert_ne!(report.readiness, Readiness::NotRequirements);
}

#[tokio::test]
async fn false_not_requirements_is_overridden_when_document_has_requirement_markers() {
    struct FalseNegativeTriageProvider;

    #[async_trait]
    impl LlmProvider for FalseNegativeTriageProvider {
        fn model(&self) -> &str {
            "false-negative-triage"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "triage" {
                return Ok(LlmResponse {
                    json: json!({
                        "kind": "not_requirements",
                        "reason": "Начало документа похоже на сопроводительное письмо.",
                        "evidence": {
                            "quote": "Сопроводительное письмо к пакету документов.",
                            "location": { "start_line": 3, "end_line": 3, "page": null }
                        }
                    }),
                    usage: Usage::default(),
                    model: self.model().into(),
                    reasoning: None,
                });
            }
            MockProvider.generate(request).await
        }
    }

    let source = "\
# Титульный лист

Сопроводительное письмо к пакету документов.

## Согласование

Лист согласования версии 1.0 от 2026-11-26.

## Требования

- ФТ-1. Сервис должен сохранять аудит операций.
";
    let document = ParserRegistry::default()
        .parse(Path::new("cover.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &FalseNegativeTriageProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert!(ctx.triage.is_none());
    assert_eq!(ctx.trace.len(), 10);
    assert_eq!(ctx.requirements.len(), 1);
    let report = Report::from_run(ctx, passport("Титульный лист"));
    assert_ne!(report.readiness, Readiness::NotRequirements);
}

#[tokio::test]
async fn stub_decision_is_discarded_when_requirements_are_extracted() {
    struct StubWithRequirementsProvider;

    #[async_trait]
    impl LlmProvider for StubWithRequirementsProvider {
        fn model(&self) -> &str {
            "stub-with-reqs"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "triage" {
                return Ok(LlmResponse {
                    json: json!({
                        "kind": "stub",
                        "reason": "В начале документа стоит пометка TODO.",
                        "evidence": {
                            "quote": "TODO: заполнить реквизиты",
                            "location": { "start_line": 3, "end_line": 3, "page": null }
                        }
                    }),
                    usage: Usage::default(),
                    model: self.model().into(),
                    reasoning: None,
                });
            }
            MockProvider.generate(request).await
        }
    }

    let source = "\
# Спецификация платежей

TODO: заполнить реквизиты

## Требования

- Ф-1. Система должна отклонять отрицательную сумму платежа.
";
    let document = ParserRegistry::default()
        .parse(Path::new("mixed.md"), source.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &StubWithRequirementsProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.requirements.len(), 1);
    let report = Report::from_run(ctx, passport("Спецификация платежей"));
    assert!(report.triage.is_none());
    let md = MarkdownRenderer.render(&report).unwrap();
    assert!(!md.contains("В документе нет содержательных требований"));
}

#[tokio::test]
async fn prose_with_modal_words_and_standard_codes_keeps_not_requirements() {
    struct RecipeTriageProvider;

    #[async_trait]
    impl LlmProvider for RecipeTriageProvider {
        fn model(&self) -> &str {
            "recipe-triage"
        }

        async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
            if request.stage == "triage" {
                return Ok(LlmResponse {
                    json: json!({
                        "kind": "not_requirements",
                        "reason": "Документ описывает кулинарный рецепт, а не требования к системе.",
                        "evidence": {
                            "quote": "Тесто должно подойти в тёплом месте 40 минут.",
                            "location": { "start_line": 5, "end_line": 5, "page": null }
                        }
                    }),
                    usage: Usage::default(),
                    model: self.model().into(),
                    reasoning: None,
                });
            }
            MockProvider.generate(request).await
        }
    }

    let recipe = "\
# Пирог с капустой

Ингредиенты по ГОСТ-12345 и мука сорта ISO-9001.

Тесто должно подойти в тёплом месте 40 минут.

Свёкла должна сохранить цвет при тушении.
";
    let document = ParserRegistry::default()
        .parse(Path::new("pie.md"), recipe.as_bytes())
        .unwrap();
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, &RecipeTriageProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(ctx.trace.len(), 1);
    assert!(ctx.findings.is_empty());
    let report = Report::from_run(ctx, passport("Пирог с капустой"));
    assert_eq!(report.readiness, Readiness::NotRequirements);
    assert!(report.triage.is_some());
}
