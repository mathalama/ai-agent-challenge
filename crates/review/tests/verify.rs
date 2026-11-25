use std::path::Path;

use async_trait::async_trait;
use doc::{ParserRegistry, SourceSpan};
use llm::{LlmProvider, LlmRequest, LlmResponse, MockProvider, Usage};
use review::{
    Check, CheckPhase, CheckRegistry, Criterion, Detector, Evidence, Finding, Pipeline, Readiness,
    Report, RunContext, RunPassport, Scope, Severity, StageResult,
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

struct ReviewProvider(Option<(Severity, &'static str)>);

#[async_trait]
impl LlmProvider for ReviewProvider {
    fn model(&self) -> &str {
        "test"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage == "segment" {
            let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
            assert_eq!(input["blocks"].as_array().unwrap().len(), 3);
            return MockProvider.generate(request).await;
        }
        let mut findings = Vec::new();
        if request.stage == "set_checks"
            && let Some((severity, quote)) = self.0
        {
            findings.push(Finding {
                id: "F-1".into(),
                scope: Scope::Set,
                criterion: Criterion::Consistent,
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
                verified: true,
            });
        }
        Ok(LlmResponse {
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
async fn review_readiness_requires_coverage_and_clear_serious_findings() {
    for (finding, expected) in [
        (None, "ready"),
        (Some((Severity::High, "Первое требование")), "blocked"),
        (Some((Severity::Critical, "Первое требование")), "blocked"),
        (
            Some((Severity::Critical, "Вымышленная цитата")),
            "checks_incomplete",
        ),
        (Some((Severity::Medium, "Первое требование")), "ready"),
    ] {
        let document = ParserRegistry::default()
            .parse(
                Path::new("input.md"),
                "# Документ требований\n\n* **Ф-1.** Первое требование\n\n* **Ф-2.** Второе требование".as_bytes(),
            )
            .unwrap();
        let provider = ReviewProvider(finding);
        let checks = CheckRegistry::default();
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
