use std::path::Path;

use async_trait::async_trait;
use doc::{ParserRegistry, SourceSpan};
use llm::MockProvider;
use review::{
    Check, CheckPhase, CheckRegistry, Criterion, Detector, Evidence, Finding, Pipeline, RunContext,
    Scope, Severity, StageResult,
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
    let source = "Первая строка\r\nДословная строка\r\nПоследняя строка";
    let document = ParserRegistry::default()
        .parse(Path::new("source.txt"), source.as_bytes())
        .unwrap();
    let mut checks = CheckRegistry::default();
    checks.register(SampleCheck);
    let mut ctx = RunContext::new(document, source, &MockProvider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();
    let verified: Vec<_> = ctx
        .findings
        .iter()
        .filter(|finding| finding.verified)
        .map(|finding| finding.id.as_str())
        .collect();
    assert_eq!(verified, ["F-1"]);
}
