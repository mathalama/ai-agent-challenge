use std::{cmp::Reverse, collections::HashSet, fmt::Write};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Finding, FindingId, Question, Requirement, RunContext, RunPassport, Scope, Severity};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub passport: RunPassport,
    pub requirements: Vec<Requirement>,
    pub findings: Vec<Finding>,
    pub summary: ReportSummary,
    pub readiness: Readiness,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportSummary {
    pub requirement_count: usize,
    pub finding_count: usize,
    pub verified_count: usize,
    pub critical_findings: Vec<FindingId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Blocked,
    ChecksIncomplete,
    Ready,
}

impl Readiness {
    fn evaluate(checks_complete: bool, findings: &[Finding]) -> Self {
        if findings.iter().any(needs_fix) {
            Self::Blocked
        } else if !checks_complete
            || findings
                .iter()
                .any(|finding| !finding.verified && finding.severity >= Severity::High)
        {
            Self::ChecksIncomplete
        } else {
            Self::Ready
        }
    }
}

fn needs_fix(finding: &Finding) -> bool {
    finding.verified && finding.severity >= Severity::Medium
}

impl Report {
    pub fn from_run(ctx: RunContext<'_>, passport: RunPassport) -> Self {
        let summary = ReportSummary {
            requirement_count: ctx.requirements.len(),
            finding_count: ctx.findings.len(),
            verified_count: ctx
                .findings
                .iter()
                .filter(|finding| finding.verified)
                .count(),
            critical_findings: ctx
                .findings
                .iter()
                .filter(|finding| finding.severity == Severity::Critical)
                .map(|finding| finding.id.clone())
                .collect(),
        };
        let readiness = Readiness::evaluate(
            ctx.checks_complete && !ctx.requirements.is_empty(),
            &ctx.findings,
        );
        Self {
            passport,
            requirements: ctx.requirements,
            findings: ctx.findings,
            summary,
            readiness,
            questions: ctx.questions,
        }
    }
}

pub trait ReportRenderer {
    fn render(&self, report: &Report) -> Result<String, serde_json::Error>;
}

pub struct JsonRenderer;

impl ReportRenderer for JsonRenderer {
    fn render(&self, report: &Report) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(report).map(|json| json + "\n")
    }
}

pub struct MarkdownRenderer;

impl ReportRenderer for MarkdownRenderer {
    fn render(&self, report: &Report) -> Result<String, serde_json::Error> {
        let passport = &report.passport;
        let mut output = format!(
            "# Отчёт о проверке требований\n\n## 1. Паспорт прогона\n\n\
            - Документ — {}\n- Дата — {}\n- Модель — {}\n- Версия агента — {}\n",
            markdown(&passport.document_title),
            markdown(&passport.date),
            markdown(&passport.model),
            markdown(&passport.agent_version),
        );
        output.push_str("\n## 2. Нарезка требований\n\n| ID в отчёте | ID в документе | Краткая формулировка |\n| --- | --- | --- |\n");
        for requirement in &report.requirements {
            writeln!(
                output,
                "| {} | {} | {} |",
                markdown(&requirement.id),
                markdown(requirement.source_id.as_deref().unwrap_or("—")),
                markdown(&requirement.text),
            )
            .expect("запись в строку");
        }

        output.push_str("\n## 3. Проверка каждого требования\n");
        for requirement in &report.requirements {
            writeln!(
                output,
                "\n### {}\n\n> {}\n\n{}\n",
                markdown(&requirement.id),
                markdown(&requirement.quote),
                location(requirement.location)
            )
            .expect("запись в строку");
            write_findings(&mut output, report.findings.iter().enumerate().filter(|(_, finding)| {
                matches!(&finding.scope, Scope::Requirement(id) if *id == requirement.id)
            }));
        }
        if report.requirements.is_empty() {
            output.push_str("\nНет требований для вывода.\n");
        }

        let requirement_ids: HashSet<_> = report.requirements.iter().map(|req| &req.id).collect();
        let mut unmatched = report.findings.iter().enumerate().filter(|(_, finding)| {
            matches!(&finding.scope, Scope::Requirement(id) if !requirement_ids.contains(id))
        }).peekable();
        if unmatched.peek().is_some() {
            output.push_str("\n### Находки без привязки к требованию\n\n");
            write_findings(&mut output, unmatched);
        }

        output.push_str("\n## 4. Проверка набора требований\n\n");
        write_findings(
            &mut output,
            report
                .findings
                .iter()
                .enumerate()
                .filter(|(_, finding)| matches!(finding.scope, Scope::Set)),
        );

        let summary = &report.summary;
        writeln!(output,
            "\n## 5. Сводка\n\n- Требований в отчёте — {}\n- Нарушений — {} (с подтверждёнными цитатами — {})\n- Критичные дефекты — {}",
            summary.requirement_count, summary.finding_count, summary.verified_count,
            if summary.critical_findings.is_empty() { "список пуст".into() } else {
                summary.critical_findings.iter().map(|id| markdown(id)).collect::<Vec<_>>().join(", ")
            },
        ).expect("запись в строку");

        output.push_str("\n## 6. Итог\n\n");
        output.push_str(match report.readiness {
            Readiness::Blocked => "**Не готово.**\n",
            Readiness::ChecksIncomplete => {
                "**Готовность не подтверждена.** Нужно завершить проверки.\n"
            }
            Readiness::Ready => {
                "**Готово.** По результатам проверки можно начинать разработку и тестирование.\n"
            }
        });
        write_priority_fixes(&mut output, report);
        if passport.model == "mock" {
            output.push_str(
                "\nУчебный прогон модели охватывает одно требование. Детерминированные правила проверяют весь документ.\n",
            );
        }

        output.push_str("\n## 7. Уточняющие вопросы автору документа\n\n");
        if report.questions.is_empty() {
            output.push_str("Вопросы отсутствуют.\n");
        }
        for question in &report.questions {
            let related = question
                .related
                .iter()
                .map(|id| match id {
                    crate::RelatedId::Finding(id) | crate::RelatedId::Requirement(id) => {
                        markdown(id)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                output,
                "- {}. {}. Приоритет — {}. Основание — {}.",
                markdown(&question.id),
                markdown(&question.text),
                question.priority.russian_name(),
                markdown(&question.why),
            )
            .expect("запись в строку");
            if !related.is_empty() {
                writeln!(output, "  Связи — {related}.").expect("запись в строку");
            }
        }

        Ok(output)
    }
}

fn write_priority_fixes(output: &mut String, report: &Report) {
    let mut fixes: Vec<_> = report
        .findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| needs_fix(finding))
        .map(|(index, finding)| {
            let affected = affected_requirement_count(finding, &report.requirements);
            (index, finding, affected)
        })
        .collect();
    fixes.sort_by_key(|(_, finding, affected)| (Reverse(finding.severity), Reverse(*affected)));

    if !fixes.is_empty() {
        output.push_str("\n### Что исправить в первую очередь\n\n");
    }
    for (index, finding, affected) in fixes {
        writeln!(
            output,
            "- [{}](#finding-{}) — {} важность, затронуто требований — {}. {}",
            markdown(&finding.id),
            index + 1,
            finding.severity.russian_name(),
            affected,
            markdown(
                finding
                    .suggestion
                    .as_deref()
                    .unwrap_or(&finding.explanation)
            ),
        )
        .expect("запись в строку");
    }
}

fn affected_requirement_count(finding: &Finding, requirements: &[Requirement]) -> usize {
    match &finding.scope {
        Scope::Requirement(id) => usize::from(requirements.iter().any(|req| req.id == *id)),
        Scope::Set => requirements
            .iter()
            .filter(|requirement| {
                let span = requirement.location;
                finding.evidence.iter().any(|evidence| {
                    let evidence_span = evidence.location;
                    span.page == evidence_span.page
                        && span.start_line <= evidence_span.end_line
                        && evidence_span.start_line <= span.end_line
                })
            })
            .map(|requirement| &requirement.id)
            .collect::<HashSet<_>>()
            .len(),
    }
}

fn write_findings<'a>(output: &mut String, findings: impl Iterator<Item = (usize, &'a Finding)>) {
    let mut findings = findings.peekable();
    if findings.peek().is_none() {
        output.push_str("Замечания в этом прогоне отсутствуют.\n");
    }
    for (index, finding) in findings {
        writeln!(
            output,
            "- <a name=\"finding-{}\"></a>**{}** — {} ({}), {}. {}",
            index + 1,
            markdown(&finding.id),
            finding.criterion.russian_name(),
            finding.criterion.reference(),
            finding.severity.russian_name(),
            markdown(&finding.explanation),
        )
        .expect("запись в строку");
        if !finding.verified {
            output.push_str("  Доказательства не подтверждены.\n");
        }
        for evidence in &finding.evidence {
            writeln!(
                output,
                "  - «{}» ({})",
                markdown(&evidence.quote),
                location(evidence.location)
            )
            .expect("запись в строку");
        }
        if let Some(suggestion) = &finding.suggestion {
            writeln!(output, "  Исправление — {}", markdown(suggestion)).expect("запись в строку");
        }
    }
}

fn location(span: doc::SourceSpan) -> String {
    let lines = format!("строки {}–{}", span.start_line, span.end_line);
    match span.page {
        Some(page) => format!("страница {page}, {lines}"),
        None => lines,
    }
}

fn markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\n' => escaped.push_str("<br>"),
            '\r' => {}
            '\\' | '|' | '*' | '_' | '`' | '[' | ']' | '~' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use doc::SourceSpan;

    use super::*;
    use crate::{Criterion, Detector, Evidence};

    fn finding(id: &str, severity: Severity, verified: bool) -> Finding {
        Finding {
            id: id.into(),
            severity,
            verified,
            scope: Scope::Set,
            criterion: Criterion::Consistent,
            evidence: Vec::new(),
            explanation: format!("Описание {id}"),
            suggestion: None,
            detector: Detector::Rule("test".into()),
        }
    }

    #[test]
    fn readiness_requires_complete_checks_without_verified_blockers() {
        let low = finding("low", Severity::Low, true);
        let unverified = finding("unverified", Severity::Medium, false);
        assert!(matches!(
            Readiness::evaluate(true, &[low, unverified]),
            Readiness::Ready
        ));
        for severity in [Severity::Medium, Severity::High, Severity::Critical] {
            let blockers = [finding("blocker", severity, true)];
            assert!(matches!(
                Readiness::evaluate(true, &blockers),
                Readiness::Blocked
            ));
            assert!(matches!(
                Readiness::evaluate(false, &blockers),
                Readiness::Blocked
            ));
        }
        for severity in [Severity::High, Severity::Critical] {
            let unverified = finding("unverified", severity, false);
            assert!(matches!(
                Readiness::evaluate(true, &[unverified]),
                Readiness::ChecksIncomplete
            ));
        }
        assert!(matches!(
            Readiness::evaluate(false, &[]),
            Readiness::ChecksIncomplete
        ));
    }

    #[test]
    fn markdown_links_fixes_in_severity_and_impact_order() {
        let requirements =
            [("R-1", 1, None), ("R-2", 2, None), ("R-3", 2, Some(2))].map(|(id, line, page)| {
                Requirement {
                    id: id.into(),
                    source_id: None,
                    text: String::new(),
                    quote: String::new(),
                    location: SourceSpan {
                        start_line: line,
                        end_line: line,
                        page,
                    },
                    part: None,
                }
            });
        let mut high_single = finding("high-single", Severity::High, true);
        high_single.scope = Scope::Requirement("R-1".into());
        let mut orphan = finding("orphan", Severity::Medium, true);
        orphan.scope = Scope::Requirement("R-404".into());
        let mut high_multiple = finding("high-multiple", Severity::High, true);
        high_multiple.suggestion = Some("Устранить противоречие".into());
        high_multiple.evidence = [0, 1, 0]
            .map(|index| Evidence {
                quote: String::new(),
                location: requirements[index].location,
            })
            .into();

        let report = Report {
            passport: RunPassport {
                document_title: String::new(),
                date: String::new(),
                model: "test".into(),
                agent_version: String::new(),
            },
            requirements: requirements.into(),
            findings: vec![
                high_single,
                finding("medium", Severity::Medium, true),
                high_multiple,
                finding("critical", Severity::Critical, true),
                finding("unverified", Severity::Critical, false),
                finding("low", Severity::Low, true),
                orphan,
            ],
            summary: ReportSummary {
                requirement_count: 3,
                finding_count: 7,
                verified_count: 6,
                critical_findings: Vec::new(),
            },
            readiness: Readiness::Blocked,
            questions: Vec::new(),
        };

        let markdown = MarkdownRenderer.render(&report).unwrap();
        let (_, summary) = markdown.split_once("## 6. Итог").unwrap();
        let (summary, _) = summary.split_once("## 7.").unwrap();
        assert!(summary.contains("**Не готово.**"));
        assert_eq!(
            summary
                .lines()
                .filter(|line| line.starts_with("- "))
                .collect::<Vec<_>>(),
            [
                "- [critical](#finding-4) — критическая важность, затронуто требований — 0. Описание critical",
                "- [high-multiple](#finding-3) — высокая важность, затронуто требований — 2. Устранить противоречие",
                "- [high-single](#finding-1) — высокая важность, затронуто требований — 1. Описание high-single",
                "- [medium](#finding-2) — средняя важность, затронуто требований — 0. Описание medium",
                "- [orphan](#finding-7) — средняя важность, затронуто требований — 0. Описание orphan",
            ]
        );
        for index in [1, 2, 3, 4, 7] {
            assert!(markdown.contains(&format!("<a name=\"finding-{index}\"></a>")));
        }
    }
}
