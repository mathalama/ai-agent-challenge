use std::fmt::Write;

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
        let serious = || {
            ctx.findings
                .iter()
                .filter(|finding| finding.severity >= Severity::High)
        };
        let readiness = if serious().any(|finding| finding.verified) {
            Readiness::Blocked
        } else if !ctx.checks_complete || ctx.requirements.is_empty() || serious().next().is_some()
        {
            Readiness::ChecksIncomplete
        } else {
            Readiness::Ready
        };
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
        output.push_str("\n## 2. Нарезка требований\n\n| ID в отчёте | ID в документе | Раздел | Краткая формулировка |\n| --- | --- | --- | --- |\n");
        for requirement in &report.requirements {
            writeln!(
                output,
                "| {} | {} | {} | {} |",
                markdown(&requirement.id),
                markdown(requirement.source_id.as_deref().unwrap_or("—")),
                markdown(&requirement_section(requirement)),
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
                requirement_location(requirement)
            )
            .expect("запись в строку");
            write_findings(&mut output, report.findings.iter().filter(|finding| {
                matches!(&finding.scope, Scope::Requirement(id) if *id == requirement.id)
            }));
        }
        if report.requirements.is_empty() {
            output.push_str("\nНет требований для вывода.\n");
        }

        output.push_str("\n## 4. Проверка набора требований\n\n");
        write_findings(
            &mut output,
            report
                .findings
                .iter()
                .filter(|finding| matches!(finding.scope, Scope::Set)),
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
            Readiness::Blocked => "Начинать разработку и тестирование рано. Исправьте подтверждённые дефекты высокой и критической важности.\n",
            Readiness::ChecksIncomplete => "Готовность к разработке и тестированию не подтверждена. Нужно завершить проверки.\n",
            Readiness::Ready => "По результатам проверки можно начинать разработку и тестирование.\n",
        });
        if passport.model == "mock" {
            output.push_str(
                "\nУчебный прогон показывает работу каркаса на одном примере требования.\n",
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

fn write_findings<'a>(output: &mut String, findings: impl Iterator<Item = &'a Finding>) {
    let mut findings = findings.peekable();
    if findings.peek().is_none() {
        output.push_str("Замечания в этом прогоне отсутствуют.\n");
    }
    for finding in findings {
        writeln!(
            output,
            "- **{}** — {} ({}), {}. {}",
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

fn requirement_section(requirement: &Requirement) -> String {
    match (requirement.part.as_deref(), requirement.section.as_deref()) {
        (Some(part), Some(section)) => format!("{part} / {section}"),
        (Some(part), None) => part.to_owned(),
        (None, Some(section)) => section.to_owned(),
        (None, None) => "—".to_owned(),
    }
}

fn requirement_location(requirement: &Requirement) -> String {
    let mut parts = Vec::new();
    if let Some(part) = requirement.part.as_deref() {
        parts.push(markdown(part));
    }
    if let Some(section) = requirement.section.as_deref() {
        parts.push(markdown(section));
    }
    if let Some(table) = &requirement.table {
        let label = match table.title.as_deref() {
            Some(title) => format!(
                "таблица {} «{}», строка {}",
                table.index,
                markdown(title),
                table.row
            ),
            None => format!("таблица {}, строка {}", table.index, table.row),
        };
        parts.push(label);
    }
    parts.push(location(requirement.location));
    parts.join(" · ")
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
