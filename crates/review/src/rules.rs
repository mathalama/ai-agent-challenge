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

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        ctx.document
            .source_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("txt") && !ext.eq_ignore_ascii_case("text"))
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut blocks = ctx.document.blocks.iter().peekable();
        let mut findings = Vec::new();
        let mut found_section = false;
        while let Some(heading) = blocks.next() {
            let BlockContent::Heading { level, text } = &heading.content else {
                continue;
            };
            if !is_testing_heading(text) {
                continue;
            }
            found_section = true;

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
                findings.push(testing_finding(
                    format!("testing-section-{}", heading.id.0),
                    evidence,
                    "Раздел тестирования пуст или содержит только заглушки.",
                ));
            }
        }
        if !found_section {
            let evidence = ctx.document.blocks.iter().find_map(|block| {
                let quote = match &block.content {
                    BlockContent::Heading { text, .. }
                    | BlockContent::Paragraph { text }
                    | BlockContent::ListItem { text }
                    | BlockContent::Code { text, .. } => text,
                    BlockContent::Table { rows, .. } => {
                        rows.iter().flatten().find(|cell| !cell.trim().is_empty())?
                    }
                };
                (!quote.trim().is_empty()).then(|| Evidence {
                    quote: quote.clone(),
                    location: block.location,
                })
            });
            findings.push(testing_finding(
                "testing-section-missing".into(),
                evidence.into_iter().collect(),
                "В документе отсутствует раздел тестирования.",
            ));
        }
        Ok(findings)
    }
}

fn is_testing_heading(text: &str) -> bool {
    let title = text
        .trim_start_matches(|c: char| {
            c.is_whitespace()
                || c.is_ascii_digit()
                || c.is_ascii_punctuation()
                || matches!(c, '—' | '–' | '«')
        })
        .trim_end_matches(|c: char| {
            c.is_whitespace() || c.is_ascii_punctuation() || matches!(c, '—' | '–' | '»' | '…')
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('ё', "е");
    matches!(
        title.as_str(),
        "требования к тестированию"
            | "тестирование"
            | "тестирование требований"
            | "тестирование и приемка"
            | "требования к тестированию и приемке"
            | "testing"
            | "testing requirements"
            | "test requirements"
            | "testing and acceptance"
    )
}

fn testing_finding(id: String, evidence: Vec<Evidence>, explanation: &str) -> Finding {
    Finding {
        id,
        scope: Scope::Set,
        criterion: Criterion::SetComplete,
        severity: Severity::Medium,
        evidence,
        explanation: explanation.into(),
        suggestion: Some("Указать сценарии тестирования и ожидаемые результаты.".into()),
        detector: Detector::Rule("testing-section".into()),
        verified: false,
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
            ("## 5. Требования к тестированию\n\nTBD", 1),
            ("## Тестирование:\n\nTBD", 1),
            ("## 3.2) Тестирование и приёмка\n\nTBD", 1),
            ("## Общие сведения\n\nПороги задаются в тенге.", 1),
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
