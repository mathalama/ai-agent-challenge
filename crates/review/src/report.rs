use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashSet},
    fmt::Write,
};

use doc::Block;
use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};

use crate::{
    Answer, Criterion, Finding, FindingId, Question, RelatedId, Requirement, RequirementId,
    RunContext, RunPassport, Scope, Severity,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub schema_version: ReportSchemaVersion,
    pub passport: RunPassport,
    pub blocks: Vec<Block>,
    pub requirements: Vec<Requirement>,
    pub findings: Vec<Finding>,
    pub summary: ReportSummary,
    pub readiness: Readiness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete_stages: Vec<String>,
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub finding_answers: BTreeMap<FindingId, Vec<Answer>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requirement_answers: BTreeMap<RequirementId, Vec<Answer>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub standalone_answers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unmatched_answers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum ReportSchemaVersion {
    #[serde(rename = "1")]
    V1,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportSummary {
    pub requirement_count: usize,
    pub finding_count: usize,
    pub verified_count: usize,
    /// Идентификаторы критичных дефектов: находки критической важности и находки высокой важности по критериям verifiable, consistent, complete и set_complete.
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
    fn evaluate<'a>(
        checks_complete: bool,
        mut findings: impl Iterator<Item = &'a Finding> + Clone,
    ) -> Self {
        if findings.clone().any(needs_fix) {
            Self::Blocked
        } else if !checks_complete
            || findings.any(|finding| !finding.verified && finding.severity >= Severity::High)
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

fn is_critical_defect(finding: &Finding) -> bool {
    finding.severity == Severity::Critical
        || (finding.severity == Severity::High
            && finding.all_criteria().into_iter().any(|criterion| {
                matches!(
                    criterion,
                    Criterion::Verifiable
                        | Criterion::Consistent
                        | Criterion::Complete
                        | Criterion::SetComplete
                )
            }))
}

impl Report {
    pub fn json_schema() -> Schema {
        SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<Self>()
    }

    pub fn from_run(ctx: RunContext<'_>, passport: RunPassport) -> Self {
        Self::with_answers(ctx, passport, &BTreeMap::new())
    }

    pub fn with_answers(
        ctx: RunContext<'_>,
        passport: RunPassport,
        answers: &BTreeMap<String, String>,
    ) -> Self {
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
                .filter(|finding| is_critical_defect(finding))
                .map(|finding| finding.id.clone())
                .collect(),
        };
        let readiness = Readiness::evaluate(
            ctx.checks_complete && !ctx.requirements.is_empty(),
            ctx.findings.iter(),
        );
        let mut report = Self {
            schema_version: ReportSchemaVersion::V1,
            passport,
            blocks: ctx.document.blocks,
            requirements: ctx.requirements,
            findings: ctx.findings,
            summary,
            readiness,
            incomplete_stages: ctx.incomplete_stages,
            questions: ctx.questions,
            finding_answers: BTreeMap::new(),
            requirement_answers: BTreeMap::new(),
            standalone_answers: BTreeMap::new(),
            unmatched_answers: answers.clone(),
        };
        for question in std::mem::take(&mut report.questions) {
            match report.unmatched_answers.remove(&question.id) {
                Some(text) if !text.trim().is_empty() => report.record_answer(&question, &text),
                _ => report.questions.push(question),
            }
        }
        report
    }

    /// Снимает открытый вопрос и прикрепляет ответ к связанным находкам и требованиям.
    pub fn answer_question(&mut self, id: &str, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let Some(index) = self.questions.iter().position(|question| question.id == id) else {
            return false;
        };
        let question = self.questions.remove(index);
        self.record_answer(&question, text);
        true
    }

    fn record_answer(&mut self, question: &Question, text: &str) {
        if question.related.is_empty() {
            self.standalone_answers
                .insert(question.id.clone(), text.to_owned());
            return;
        }
        for related in &question.related {
            let answers = match related {
                RelatedId::Finding(id) => self.finding_answers.entry(id.clone()).or_default(),
                RelatedId::Requirement(id) => {
                    self.requirement_answers.entry(id.clone()).or_default()
                }
            };
            answers.push(Answer {
                question_id: question.id.clone(),
                text: text.to_owned(),
            });
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
                markdown_cell(&requirement.id),
                markdown_cell(requirement.source_id.as_deref().unwrap_or("—")),
                markdown_cell(&requirement_section(requirement)),
                markdown_cell(&requirement.text),
            )
            .expect("запись в строку");
        }

        output.push_str(
            "\n<a name=\"requirement-findings\"></a>\n\n## 3. Проверка каждого требования\n",
        );
        for requirement in &report.requirements {
            writeln!(
                output,
                "\n### {}\n\n> {}\n\n{}\n",
                markdown(&requirement.id),
                markdown_block(&requirement.quote),
                requirement_location(requirement)
            )
            .expect("запись в строку");
            write_findings(&mut output, report, report.findings.iter().enumerate().filter(|(_, finding)| {
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
            write_findings(&mut output, report, unmatched);
        }

        output.push_str("\n<a name=\"set-findings\"></a>\n\n## 4. Проверка набора требований\n\n");
        write_findings(
            &mut output,
            report,
            report
                .findings
                .iter()
                .enumerate()
                .filter(|(_, finding)| matches!(finding.scope, Scope::Set)),
        );

        let summary = &report.summary;
        let critical: Vec<_> = report
            .findings
            .iter()
            .enumerate()
            .filter(|(_, finding)| finding.verified && is_critical_defect(finding))
            .map(|(index, finding)| format!("[{}](#finding-{})", markdown(&finding.id), index + 1))
            .collect();
        let blocking_critical: Vec<_> = report
            .findings
            .iter()
            .enumerate()
            .filter(|(_, finding)| finding.verified && finding.severity == Severity::Critical)
            .map(|(index, finding)| format!("[{}](#finding-{})", markdown(&finding.id), index + 1))
            .collect();
        let critical_summary = if critical.is_empty() {
            "Критичные дефекты (непроверяемые, противоречивые, неполные) — нет".to_owned()
        } else {
            format!(
                "Критичные дефекты (непроверяемые, противоречивые, неполные) — {}: {}",
                critical.len(),
                critical.join(", ")
            )
        };
        writeln!(
            output,
            "\n## 5. Сводка\n\n- Требований в отчёте — {}\n- Нарушений — {} (с подтверждёнными цитатами — {})\n- {critical_summary}",
            summary.requirement_count, summary.finding_count, summary.verified_count,
        )
        .expect("запись в строку");
        if !report.finding_answers.is_empty() {
            writeln!(
                output,
                "- Находок с ответами — {}",
                report.finding_answers.len()
            )
            .expect("запись в строку");
        }

        output.push_str("\n## 6. Итог\n\n");
        output.push_str(match report.readiness {
            Readiness::Blocked => "**Не готово.**\n\n",
            Readiness::ChecksIncomplete => {
                "**Готовность не подтверждена.** Нужно завершить проверки.\n"
            }
            Readiness::Ready => {
                "**Готово.** По результатам проверки можно начинать разработку и тестирование.\n"
            }
        });
        if matches!(report.readiness, Readiness::Blocked) {
            write_blocking_reason(&mut output, &report.findings, &blocking_critical);
        }
        if !report.incomplete_stages.is_empty() {
            let stages = report
                .incomplete_stages
                .iter()
                .map(|stage| markdown(stage))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                output,
                "\nПроверка неполная. Стадии, ограниченные длиной ответа модели — {stages}."
            )
            .expect("запись в строку");
        }
        write_priority_fixes(&mut output, report);
        if passport.model == "mock" {
            output.push_str(
                "\nУчебный прогон использует фиксированные ответы модели. Детерминированные правила проверяют весь документ.\n",
            );
        }

        output.push_str("\n## 7. Уточняющие вопросы автору документа\n\n");
        if report.questions.is_empty() {
            output.push_str("Вопросы отсутствуют.\n");
        }
        let mut questions: Vec<_> = report.questions.iter().collect();
        questions.sort_by_key(|question| Reverse(question.priority));
        let (main, additional) = questions.split_at(questions.len().min(10));
        let mut topics = Vec::new();
        for question in main {
            if !topics.contains(&question.topic) {
                topics.push(question.topic);
            }
        }
        for topic in topics {
            writeln!(output, "### {}\n", topic.russian_name()).expect("запись в строку");
            for question in main.iter().filter(|question| question.topic == topic) {
                write_question(&mut output, report, question);
            }
            output.push('\n');
        }
        if !additional.is_empty() {
            output.push_str("### Дополнительные вопросы\n\n");
            for question in additional {
                write_question(&mut output, report, question);
            }
        }

        if !report.requirement_answers.is_empty() {
            output.push_str("\n### Ответы по требованиям\n\n");
            for (id, answers) in &report.requirement_answers {
                for answer in answers {
                    writeln!(
                        output,
                        "- **{}** · ответ {} — {}",
                        markdown(id),
                        markdown(&answer.question_id),
                        markdown(&answer.text)
                    )
                    .expect("запись в строку");
                }
            }
        }

        for (title, answers) in [
            ("Ответы на общие вопросы", &report.standalone_answers),
            ("Ответы без текущего вопроса", &report.unmatched_answers),
        ] {
            if !answers.is_empty() {
                writeln!(output, "\n### {title}\n").expect("запись в строку");
                for (id, text) in answers {
                    writeln!(output, "- **{}** — {}", markdown(id), markdown(text))
                        .expect("запись в строку");
                }
            }
        }

        Ok(output)
    }
}

fn write_question(output: &mut String, report: &Report, question: &Question) {
    writeln!(
        output,
        "- **{}** — {} Приоритет — {}. Основание — {}",
        markdown(&question.id),
        sentence(&question.text),
        question.priority.russian_name(),
        sentence(&question.why),
    )
    .expect("запись в строку");
    if !question.options.is_empty() {
        output.push_str("  - Варианты ответа\n");
        for (index, option) in question.options.iter().enumerate() {
            writeln!(output, "    {}. {}", index + 1, markdown_block(option))
                .expect("запись в строку");
        }
    }
    let related = question
        .related
        .iter()
        .map(|related| match related {
            RelatedId::Finding(id) => report
                .findings
                .iter()
                .position(|finding| &finding.id == id)
                .map_or_else(
                    || markdown(id),
                    |index| format!("[{}](#finding-{})", markdown(id), index + 1),
                ),
            RelatedId::Requirement(id) => markdown(id),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if !related.is_empty() {
        writeln!(output, "  - Связи — {related}").expect("запись в строку");
    }
    for evidence in &question.evidence {
        writeln!(
            output,
            "  - «{}» ({})",
            markdown(&evidence.quote),
            location(evidence.location)
        )
        .expect("запись в строку");
    }
}

fn write_blocking_reason(output: &mut String, findings: &[Finding], critical: &[String]) {
    if critical.is_empty() {
        let count = findings.iter().filter(|finding| needs_fix(finding)).count();
        writeln!(
            output,
            "Подтверждённых дефектов средней важности и выше — {count}."
        )
    } else {
        writeln!(
            output,
            "Подтверждённых критичных дефектов — {} ({}).",
            critical.len(),
            critical.join(", ")
        )
    }
    .expect("запись в строку");
}

fn write_priority_fixes(output: &mut String, report: &Report) {
    const PRIORITY_LIMIT: usize = 7;

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
    let total = fixes.len();
    for (index, finding, affected) in fixes.into_iter().take(PRIORITY_LIMIT) {
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
    if total > PRIORITY_LIMIT {
        writeln!(
            output,
            "\nПоказано {PRIORITY_LIMIT} из {total} подтверждённых дефектов средней важности и выше. Полный список — в разделах [3](#requirement-findings) и [4](#set-findings)."
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

fn write_findings<'a>(
    output: &mut String,
    report: &Report,
    findings: impl Iterator<Item = (usize, &'a Finding)>,
) {
    let mut findings = findings.peekable();
    if findings.peek().is_none() {
        output.push_str("Замечания в этом прогоне отсутствуют.\n");
    }
    for (index, finding) in findings {
        let criteria = finding
            .all_criteria()
            .into_iter()
            .map(|criterion| format!("{} ({})", criterion.russian_name(), criterion.reference()))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            "- <a name=\"finding-{}\"></a>**{}** — {}, {}. {}",
            index + 1,
            markdown(&finding.id),
            criteria,
            finding.severity.russian_name(),
            markdown(&finding.explanation),
        )
        .expect("запись в строку");
        let detectors = finding.all_detectors();
        if detectors.len() > 1 || !finding.merged_ids.is_empty() {
            let sources = detectors
                .into_iter()
                .map(|detector| markdown(&detector.display_label()))
                .collect::<Vec<_>>()
                .join(", ");
            let ids = finding
                .all_ids()
                .into_iter()
                .map(markdown)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(output, "  Источники — {sources} (находки: {ids}).").expect("запись в строку");
        } else {
            writeln!(
                output,
                "  Источник — {}.",
                markdown(&finding.detector.display_label())
            )
            .expect("запись в строку");
        }
        if let Some(answers) = report.finding_answers.get(&finding.id) {
            for answer in answers {
                writeln!(
                    output,
                    "  - Ответ {} — {}",
                    markdown(&answer.question_id),
                    markdown(&answer.text)
                )
                .expect("запись в строку");
            }
        }
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

fn sentence(value: &str) -> String {
    let value = value.trim();
    let mut formatted = markdown(value);
    if !value.is_empty() && !value.ends_with(['.', '?', '!', '…']) {
        formatted.push('.');
    }
    formatted
}

fn markdown_block(value: &str) -> String {
    let trimmed = value.trim_start_matches([' ', '\t']);
    let mut formatted = markdown(value);
    if trimmed.starts_with('>') {
        formatted.insert(value.len() - trimmed.len(), '\\');
    }
    formatted
}

fn markdown_cell(value: &str) -> String {
    markdown(value).replace('|', "\\|")
}

fn markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for (index, character) in value.char_indices() {
        match character {
            '<' if value[index + 1..].starts_with(|next: char| {
                next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?')
            }) && value[index + 1..]
                .split('\n')
                .next()
                .is_some_and(|line| line.contains('>')) =>
            {
                escaped.push_str("\\<");
            }
            '&' if starts_with_entity(&value[index + 1..]) => escaped.push_str("\\&"),
            '\n' => escaped.push_str("<br>"),
            '\r' => {}
            '\\' | '*' | '_' | '`' | '[' | ']' | '~' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

fn starts_with_entity(value: &str) -> bool {
    let name = value.strip_prefix('#').unwrap_or(value);
    let len = name
        .bytes()
        .take(33)
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    (1..=32).contains(&len) && name.as_bytes().get(len) == Some(&b';')
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
            additional_criteria: Vec::new(),
            evidence: Vec::new(),
            explanation: format!("Описание {id}"),
            suggestion: None,
            detector: Detector::Rule("test".into()),
            additional_detectors: Vec::new(),
            merged_ids: Vec::new(),
            fallback_evidence: false,
        }
    }

    #[test]
    fn markdown_preserves_sentence_endings_and_comparisons() {
        assert_eq!(sentence("Сумма >= 10."), "Сумма >= 10.");
        assert_eq!(sentence("Сумма < 10?"), "Сумма < 10?");
        assert_eq!(sentence("Сумма < 10"), "Сумма < 10.");
        assert_eq!(sentence("Готово!"), "Готово!");
        assert_eq!(markdown("Флаг A | B"), "Флаг A | B");
        assert_eq!(markdown_cell("Флаг A | B"), r"Флаг A \| B");
        assert_eq!(markdown("<script>"), r"\<script>");
        assert_eq!(markdown("A & B, &lt;, &#62;"), r"A & B, \&lt;, \&#62;");
        assert_eq!(markdown_block("> 10 000 ₸"), r"\> 10 000 ₸");
    }

    #[test]
    fn markdown_groups_the_ten_highest_priority_questions_before_the_remainder() {
        use crate::QuestionTopic;

        let questions = (0..11)
            .map(|index| Question {
                id: format!("Q-{index}"),
                text: "Сумма < 10?".into(),
                why: "Проверка при сумме >= 10.".into(),
                priority: if index == 0 {
                    Severity::Low
                } else {
                    Severity::High
                },
                topic: QuestionTopic::Calculation,
                options: vec!["> 10 000 ₸".into(), "Символ &lt;".into()],
                evidence: Vec::new(),
                related: Vec::new(),
            })
            .collect();
        let report = Report {
            schema_version: ReportSchemaVersion::V1,
            blocks: Vec::new(),
            passport: RunPassport {
                document_title: String::new(),
                date: String::new(),
                model: "test".into(),
                agent_version: String::new(),
            },
            requirements: Vec::new(),
            findings: Vec::new(),
            summary: ReportSummary {
                requirement_count: 0,
                finding_count: 0,
                verified_count: 0,
                critical_findings: Vec::new(),
            },
            readiness: Readiness::ChecksIncomplete,
            incomplete_stages: Vec::new(),
            questions,
            finding_answers: BTreeMap::new(),
            requirement_answers: BTreeMap::new(),
            standalone_answers: BTreeMap::new(),
            unmatched_answers: BTreeMap::new(),
        };
        let rendered = MarkdownRenderer.render(&report).unwrap();
        let (main, rest) = rendered.split_once("### Дополнительные вопросы").unwrap();
        assert!(main.contains("### Расчёт"));
        assert_eq!(main.matches("- **Q-").count(), 10);
        assert!(rest.contains("**Q-0**"));
        assert!(!main.contains("**Q-0**"));
        assert!(main.contains("Сумма < 10? Приоритет"));
        assert!(main.contains("Проверка при сумме >= 10.\n"));
        assert!(main.contains(r"1. \> 10 000 ₸"));
        assert!(main.contains(r"2. Символ \&lt;"));
    }

    #[test]
    fn saved_answers_keep_blockers_and_show_their_text_in_both_formats() {
        let document = doc::ParserRegistry::default()
            .parse(std::path::Path::new("test.md"), "# Требования".as_bytes())
            .unwrap();
        let checks = crate::CheckRegistry::default();
        let mut ctx = RunContext::new(document, &llm::MockProvider, &checks);
        ctx.findings
            .push(finding("critical", Severity::Critical, true));
        ctx.questions.push(Question {
            id: "Q-1".into(),
            text: "Какое правило действует?".into(),
            why: "От этого зависит реализация.".into(),
            priority: Severity::Critical,
            topic: Default::default(),
            options: Vec::new(),
            evidence: Vec::new(),
            related: vec![RelatedId::Finding("critical".into())],
        });
        let mut requirement_question = ctx.questions[0].clone();
        requirement_question.id = "Q-2".into();
        requirement_question.related = vec![RelatedId::Requirement("R-1".into())];
        ctx.questions.push(requirement_question.clone());
        let mut standalone_question = ctx.questions[0].clone();
        standalone_question.id = "Q-3".into();
        standalone_question.related.clear();
        ctx.questions.push(standalone_question.clone());
        let passport = RunPassport {
            document_title: "Требования".into(),
            date: String::new(),
            model: "test".into(),
            agent_version: String::new(),
        };

        let mut report = Report::with_answers(
            ctx,
            passport,
            &BTreeMap::from([
                ("Q-1".into(), "не знаю".into()),
                ("Q-2".into(), "30 дней".into()),
                ("Q-3".into(), "По согласованию".into()),
                ("Q-old".into(), "предыдущее решение".into()),
            ]),
        );

        assert!(matches!(report.readiness, Readiness::Blocked));
        assert_eq!(report.summary.critical_findings, ["critical"]);
        assert!(report.questions.is_empty());
        let markdown = MarkdownRenderer.render(&report).unwrap();
        assert!(markdown.contains("Ответ Q-1 — не знаю"));
        assert!(markdown.contains("**R-1** · ответ Q-2 — 30 дней"));
        assert!(markdown.contains("**Q-3** — По согласованию"));
        assert!(markdown.contains("Находок с ответами — 1"));
        assert!(!markdown.contains("Показано 7 из"));
        assert!(markdown.contains("[critical](#finding-1)"));
        assert!(markdown.contains("**Q-old** — предыдущее решение"));
        let json: serde_json::Value =
            serde_json::from_str(&JsonRenderer.render(&report).unwrap()).unwrap();
        assert_eq!(
            json["finding_answers"]["critical"][0],
            serde_json::json!({"question_id": "Q-1", "text": "не знаю"}),
        );
        let stored = json["requirement_answers"]["R-1"].clone();
        assert_eq!(json["standalone_answers"]["Q-3"], "По согласованию");
        assert!(json["unmatched_answers"].get("Q-3").is_none());
        assert_eq!(
            stored,
            serde_json::json!([{"question_id": "Q-2", "text": "30 дней"}])
        );
        report.requirement_answers.clear();
        report.questions.push(requirement_question);
        assert!(report.answer_question("Q-2", "30 дней"));
        assert_eq!(
            serde_json::to_value(&report.requirement_answers["R-1"]).unwrap(),
            stored
        );
        report.questions.push(standalone_question);
        assert!(report.answer_question("Q-3", "Уточнено"));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["standalone_answers"]["Q-3"], "Уточнено");
    }

    #[test]
    fn readiness_requires_complete_checks_without_verified_blockers() {
        let low = finding("low", Severity::Low, true);
        let unverified = finding("unverified", Severity::Medium, false);
        assert!(matches!(
            Readiness::evaluate(true, [&low, &unverified].into_iter()),
            Readiness::Ready
        ));
        for severity in [Severity::Medium, Severity::High, Severity::Critical] {
            let blockers = [finding("blocker", severity, true)];
            assert!(matches!(
                Readiness::evaluate(true, blockers.iter()),
                Readiness::Blocked
            ));
            assert!(matches!(
                Readiness::evaluate(false, blockers.iter()),
                Readiness::Blocked
            ));
        }
        for severity in [Severity::High, Severity::Critical] {
            let unverified = finding("unverified", severity, false);
            assert!(matches!(
                Readiness::evaluate(true, [&unverified].into_iter()),
                Readiness::ChecksIncomplete
            ));
        }
        assert!(matches!(
            Readiness::evaluate(false, std::iter::empty()),
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
                    section: None,
                    table: None,
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
            schema_version: ReportSchemaVersion::V1,
            blocks: Vec::new(),
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
                finding("extra-1", Severity::Medium, true),
                finding("extra-2", Severity::Medium, true),
                finding("extra-3", Severity::Medium, true),
            ],
            summary: ReportSummary {
                requirement_count: 3,
                finding_count: 10,
                verified_count: 9,
                critical_findings: vec!["critical".into(), "unverified".into()],
            },
            readiness: Readiness::Blocked,
            incomplete_stages: Vec::new(),
            questions: Vec::new(),
            finding_answers: BTreeMap::new(),
            requirement_answers: BTreeMap::new(),
            standalone_answers: BTreeMap::new(),
            unmatched_answers: BTreeMap::new(),
        };

        let markdown = MarkdownRenderer.render(&report).unwrap();
        let (before_verdict, summary) = markdown.split_once("## 6. Итог").unwrap();
        let (_, overview) = before_verdict.split_once("## 5. Сводка").unwrap();
        assert!(overview.contains(
            "Критичные дефекты (непроверяемые, противоречивые, неполные) — 3: [high-single](#finding-1), [high-multiple](#finding-3), [critical](#finding-4)"
        ));
        let (summary, _) = summary.split_once("## 7.").unwrap();
        assert!(summary.contains("**Не готово.**"));
        assert!(
            summary.contains("Подтверждённых критичных дефектов — 1 ([critical](#finding-4)).")
        );
        assert!(!summary.contains("unverified"));
        assert!(summary.contains(
            "Показано 7 из 8 подтверждённых дефектов средней важности и выше. Полный список — в разделах [3](#requirement-findings) и [4](#set-findings)."
        ));
        for anchor in ["requirement-findings", "set-findings"] {
            assert!(markdown.contains(&format!("<a name=\"{anchor}\"></a>")));
        }
        assert!(!markdown.contains("Находок с ответами — 0"));
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
                "- [extra-1](#finding-8) — средняя важность, затронуто требований — 0. Описание extra-1",
                "- [extra-2](#finding-9) — средняя важность, затронуто требований — 0. Описание extra-2",
            ]
        );
        for index in 1..=10 {
            assert!(markdown.contains(&format!("<a name=\"finding-{index}\"></a>")));
        }

        let mut report = report;
        report.findings[3].severity = Severity::High;
        let markdown = MarkdownRenderer.render(&report).unwrap();
        assert!(
            markdown
                .contains("**Не готово.**\n\nПодтверждённых дефектов средней важности и выше — 8.")
        );
    }

    #[test]
    fn critical_defects_summary_follows_severity_and_criterion_rules() {
        let document = doc::ParserRegistry::default()
            .parse(std::path::Path::new("test.md"), "# Требования".as_bytes())
            .unwrap();
        let checks = crate::CheckRegistry::default();
        let mut ctx = RunContext::new(document, &llm::MockProvider, &checks);

        let mut high_consistent = finding("high-consistent", Severity::High, true);
        high_consistent.criterion = Criterion::Consistent;

        let mut high_ranked = finding("high-ranked", Severity::High, true);
        high_ranked.criterion = Criterion::Ranked;

        let mut critical_ranked = finding("critical-ranked", Severity::Critical, true);
        critical_ranked.criterion = Criterion::Ranked;

        let mut unverified_high = finding("unverified-high", Severity::High, false);
        unverified_high.criterion = Criterion::Verifiable;

        let mut unverified_critical = finding("unverified-critical", Severity::Critical, false);
        unverified_critical.criterion = Criterion::Appropriate;

        ctx.findings = vec![
            high_consistent,
            high_ranked,
            critical_ranked,
            unverified_high,
            unverified_critical,
        ];
        let passport = RunPassport {
            document_title: "Требования".into(),
            date: String::new(),
            model: "test".into(),
            agent_version: String::new(),
        };
        let report = Report::from_run(ctx, passport);

        assert_eq!(
            report.summary.critical_findings,
            [
                "high-consistent",
                "critical-ranked",
                "unverified-high",
                "unverified-critical",
            ]
        );
        let markdown = MarkdownRenderer.render(&report).unwrap();
        let (before_verdict, _) = markdown.split_once("## 6. Итог").unwrap();
        let (_, overview) = before_verdict.split_once("## 5. Сводка").unwrap();
        assert!(overview.contains(
            "Критичные дефекты (непроверяемые, противоречивые, неполные) — 2: [high-consistent](#finding-1), [critical-ranked](#finding-3)"
        ));
        assert!(!overview.contains("high-ranked"));
        assert!(!overview.contains("unverified-high"));
        assert!(!overview.contains("unverified-critical"));

        let mut empty_report = report;
        empty_report.findings.retain(|f| f.id == "high-ranked");
        let empty_md = MarkdownRenderer.render(&empty_report).unwrap();
        assert!(
            empty_md.contains("Критичные дефекты (непроверяемые, противоречивые, неполные) — нет")
        );
    }
}
