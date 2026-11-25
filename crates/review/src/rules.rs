use async_trait::async_trait;
use doc::BlockContent;

use crate::{
    Check, CheckPhase, Criterion, Detector, Evidence, Finding, RunContext, Scope, Severity,
    StageResult,
};

pub(crate) struct TestingSection;

#[async_trait]
impl Check for TestingSection {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut blocks = ctx.document.blocks.iter().peekable();
        let mut findings = Vec::new();
        while let Some(heading) = blocks.next() {
            let BlockContent::Heading { level, text } = &heading.content else {
                continue;
            };
            if !matches!(
                text.trim().to_lowercase().as_str(),
                "требования к тестированию"
                    | "тестирование"
                    | "тестирование требований"
                    | "testing"
                    | "testing requirements"
                    | "test requirements"
            ) {
                continue;
            }

            let mut evidence = vec![Evidence {
                quote: text.clone(),
                location: heading.location,
            }];
            let mut has_content = false;
            while let Some(block) = blocks.next_if(|block| {
                !matches!(&block.content, BlockContent::Heading { level: next, .. } if next <= level)
            }) {
                match &block.content {
                    BlockContent::Heading { .. } => {}
                    BlockContent::Table { rows, .. } => {
                        // У Markdown-таблицы первая строка содержит названия колонок.
                        has_content |= rows.iter().skip(1).flatten().any(|cell| !is_placeholder(cell));
                    }
                    BlockContent::Paragraph { text }
                    | BlockContent::ListItem { text }
                    | BlockContent::Code { text, .. } => {
                        if !is_placeholder(text) {
                            has_content = true;
                        } else if evidence.len() == 1 && !text.trim().is_empty() {
                            evidence.push(Evidence {
                                quote: text.clone(),
                                location: block.location,
                            });
                        }
                    }
                }
            }
            if !has_content {
                findings.push(Finding {
                    id: format!("testing-section-{}", heading.id.0),
                    scope: Scope::Set,
                    criterion: Criterion::SetComplete,
                    severity: Severity::Medium,
                    evidence,
                    explanation: "Раздел тестирования пуст или содержит только заглушки.".into(),
                    suggestion: Some(
                        "Указать сценарии тестирования и ожидаемые результаты.".into(),
                    ),
                    detector: Detector::Rule("testing-section".into()),
                    verified: false,
                });
            }
        }
        Ok(findings)
    }
}

fn is_placeholder(text: &str) -> bool {
    text.lines().all(|line| {
        let normalized = line.trim().trim_end_matches(['.', '!', '…']).to_lowercase();
        matches!(
            normalized.as_str(),
            "" | "—"
                | "–"
                | "-"
                | "tbd"
                | "todo"
                | "n/a"
                | "будет уточнено"
                | "см. позже"
                | "заполняется тестировщиками"
                | "заполняются тестировщиками"
        ) || normalized.starts_with("todo:")
            || normalized.starts_with("tbd:")
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use doc::ParserRegistry;
    use llm::MockProvider;

    use super::*;
    use crate::CheckRegistry;

    #[tokio::test]
    async fn testing_rule_distinguishes_empty_placeholder_and_completed_sections() {
        for (source, expected) in [
            (
                "# Спецификация\n\n## Testing\n\n## Выпуск\n\nВыпуск через CI.",
                1,
            ),
            ("## Тестирование\n\n### План\n\n* TBD\n* Будет уточнено.", 1),
            (
                "## Требования к тестированию\n\n### Тестирование требований\n\n| Сценарий | Результат |\n| --- | --- |\n| — | TBD |",
                1,
            ),
            (
                "## Testing requirements\n\n### Cases\n\nПри пустом вводе показывается сообщение.",
                0,
            ),
            (
                "## Test requirements\n\n| Input | Expected |\n| --- | --- |\n| 0 | Error |",
                0,
            ),
            ("## Общие сведения\n\nTODO", 0),
        ] {
            let document = ParserRegistry::default()
                .parse(Path::new("input.md"), source.as_bytes())
                .unwrap();
            let registry = CheckRegistry::with_builtin_rules();
            let ctx = RunContext::new(document, &MockProvider, &registry);
            let findings = registry.run(CheckPhase::Rules, &ctx).await.unwrap();
            assert_eq!(findings.len(), expected, "{source}");
            for finding in findings {
                assert!(
                    matches!(finding.detector, Detector::Rule(ref id) if id == "testing-section")
                );
                assert_eq!(finding.severity, Severity::Medium);
                assert!(finding.evidence.iter().all(|evidence| {
                    ctx.document
                        .contains_quote(evidence.location, &evidence.quote)
                }));
            }
        }
    }
}
