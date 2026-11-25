use async_trait::async_trait;
use doc::{Block, BlockContent, SourceSpan};

use crate::{
    Check, CheckPhase, Criterion, Detector, Evidence, Finding, RunContext, Scope, Severity,
    StageResult,
};

fn has_headings(ctx: &RunContext<'_>) -> bool {
    ctx.document
        .blocks
        .iter()
        .any(|block| matches!(block.content, BlockContent::Heading { .. }))
}

fn rule_finding(
    rule_id: &str,
    id: String,
    scope: Scope,
    criterion: Criterion,
    evidence: Vec<Evidence>,
    explanation: impl Into<String>,
    suggestion: impl Into<String>,
) -> Finding {
    Finding {
        id,
        scope,
        criterion,
        additional_criteria: Vec::new(),
        severity: Severity::Medium,
        evidence,
        explanation: explanation.into(),
        suggestion: Some(suggestion.into()),
        detector: Detector::Rule(rule_id.into()),
        additional_detectors: Vec::new(),
        merged_ids: Vec::new(),
        verified: false,
    }
}

fn first_document_evidence(ctx: &RunContext<'_>) -> Vec<Evidence> {
    ctx.document
        .blocks
        .iter()
        .find_map(block_evidence)
        .into_iter()
        .collect()
}

fn block_evidence(block: &Block) -> Option<Evidence> {
    let quote = match &block.content {
        BlockContent::Heading { text, .. }
        | BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => text.trim().to_string(),
        BlockContent::Table { rows, .. } => rows
            .iter()
            .find_map(|row| {
                let joined = row_quote(row);
                (!joined.is_empty()).then_some(joined)
            })
            .unwrap_or_default(),
    };
    (!quote.is_empty()).then_some(Evidence {
        quote,
        location: block.location,
    })
}

fn row_quote(row: &[String]) -> String {
    row.iter()
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn table_row_span(block: &Block, row_lines: &[usize], row_idx: usize) -> SourceSpan {
    row_lines
        .get(row_idx)
        .map_or(block.location, |&line| SourceSpan {
            start_line: line,
            end_line: line,
            page: block.location.page,
        })
}

fn normalize_title(text: &str) -> String {
    text.to_lowercase().replace('ё', "е")
}

fn title_words(text: &str) -> Vec<String> {
    normalize_title(text)
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn is_placeholder(text: &str) -> bool {
    text.lines().all(|line| {
        let trimmed = line.trim();
        let normalized = trimmed
            .trim_matches(|ch| matches!(ch, '[' | ']' | '<' | '>' | '(' | ')' | '*' | '_'))
            .trim_end_matches(['.', '!', '…', '?', ':'])
            .trim()
            .to_lowercase()
            .replace('ё', "е");
        if normalized.is_empty() {
            return true;
        }
        if normalized
            .chars()
            .all(|ch| matches!(ch, '-' | '–' | '—' | '.' | '…' | '?' | '/'))
        {
            return true;
        }
        if matches!(
            normalized.as_str(),
            "tbd"
                | "todo"
                | "tba"
                | "n/a"
                | "na"
                | "none"
                | "wip"
                | "draft"
                | "placeholder"
                | "coming soon"
                | "to be defined"
                | "to be determined"
                | "to be added"
                | "later"
                | "будет уточнено"
                | "будет определено"
                | "будет добавлено"
                | "будет заполнено"
                | "уточняется"
                | "определяется"
                | "в разработке"
                | "в процессе"
                | "не заполнено"
                | "не указано"
                | "не определено"
                | "нет данных"
                | "см. позже"
                | "см позже"
                | "позже"
                | "уточнить позже"
                | "добавить позже"
                | "заполнить позже"
                | "заглушка"
        ) {
            return true;
        }
        if normalized.starts_with("todo:")
            || normalized.starts_with("todo ")
            || normalized.starts_with("tbd:")
            || normalized.starts_with("tbd ")
            || normalized.starts_with("tba:")
            || normalized.starts_with("wip:")
        {
            return true;
        }
        let word_count = normalized.split_whitespace().count();
        word_count <= 6
            && (normalized.starts_with("заполняется ")
                || normalized.starts_with("заполняются ")
                || normalized.starts_with("заполнить ")
                || normalized.starts_with("будет уточн")
                || normalized.starts_with("будет заполн")
                || normalized.starts_with("будет добавл")
                || normalized.starts_with("будет определ")
                || normalized.starts_with("см. позже")
                || normalized.starts_with("см позже")
                || normalized.starts_with("уточняется "))
    })
}

fn is_testing_title(text: &str) -> bool {
    let title = normalize_title(text);
    title.contains("контроля и приемки")
        || title_words(text).iter().any(|word| {
            word.starts_with("тестир")
                || word.starts_with("тест-")
                || word.starts_with("испытан")
                || word.starts_with("test")
                || word.starts_with("верификац")
                || matches!(word.as_str(), "тест" | "тесты" | "тестов" | "qa")
        })
}

fn is_approvers_title(text: &str) -> bool {
    let title = normalize_title(text);
    title.contains("лист согласования")
        || title.contains("блок согласования")
        || title.contains("согласование документа")
        || title.contains("лист утверждения")
        || title.contains("sign-off")
        || title.contains("sign off")
        || title_words(text).iter().any(|word| {
            word.starts_with("согласующ")
                || word.starts_with("утверждающ")
                || word.starts_with("подписант")
                || word.starts_with("approver")
                || word.starts_with("approval")
                || word == "signoff"
        })
}

fn is_review_checklist_title(text: &str) -> bool {
    let title = normalize_title(text);
    title.contains("чек-лист")
        || title.contains("чеклист")
        || title.contains("контрольный список")
        || title.contains("quality gate")
        || title_words(text).iter().any(|word| {
            word == "ревью"
                || word == "review"
                || word.starts_with("checklist")
                || word.starts_with("самопроверк")
        })
}

fn is_acceptance_criteria_title(text: &str) -> bool {
    let title = normalize_title(text);
    title.contains("критерии приемки")
        || title.contains("критерий приемки")
        || title.contains("критерии готовности")
        || title.contains("сценарии приемки")
        || title.contains("условия приемки")
        || title.contains("тестирование и приемка")
        || title.contains("контроля и приемки")
        || title.contains("acceptance criteria")
        || title.contains("definition of done")
        || title_words(text)
            .iter()
            .any(|word| word == "приемка" || word == "приемки" || word == "dod")
}

fn is_requirements_section_title(text: &str) -> bool {
    let title = normalize_title(text);
    title.contains("функциональные требования")
        || title.contains("нефункциональные требования")
        || title.contains("бизнес-требования")
        || title.contains("системные требования")
        || title.contains("пользовательские требования")
        || title.contains("functional requirements")
        || title.contains("non-functional requirements")
        || title_words(text)
            .iter()
            .any(|word| matches!(word.as_str(), "требования" | "requirements"))
}

fn is_unselected_choice(cell: &str) -> bool {
    let normalized = cell
        .trim()
        .to_lowercase()
        .replace('ё', "е")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let compact = normalized.replace(" / ", "/");
    matches!(
        compact.as_str(),
        "да/нет"
            | "да/нет/не требуется"
            | "да/нет/н/п"
            | "да/нет/n/a"
            | "yes/no"
            | "yes/no/n/a"
            | "yes/no/not required"
            | "pass/fail"
            | "ok/fail"
            | "[ ]"
            | "☐"
    )
}

fn is_checklist_status_header(cell: &str) -> bool {
    let norm = normalize_title(cell);
    norm.contains("создано")
        || norm.contains("проверено")
        || norm.contains("статус")
        || norm.contains("выполнено")
        || norm.contains("отметка")
        || norm.contains("checked")
        || norm.contains("status")
        || norm.contains("done")
        || norm.contains("verified")
}

pub(crate) fn is_review_checklist_table(rows: &[Vec<String>]) -> bool {
    if rows.len() < 2 {
        return false;
    }
    let header = &rows[0];
    let has_item_header = header.iter().any(|cell| {
        let norm = normalize_title(cell);
        norm.contains("проверка")
            || norm.contains("пункт")
            || norm.contains("чек-лист")
            || norm.contains("чеклист")
            || norm.contains("checklist")
            || norm.contains("критерий")
    });
    let status_count = header
        .iter()
        .filter(|cell| is_checklist_status_header(cell))
        .count();
    (has_item_header && status_count >= 1) || status_count >= 2
}

fn is_approvers_table(rows: &[Vec<String>]) -> bool {
    let Some(header) = rows.first() else {
        return false;
    };
    header.iter().any(|cell| {
        let norm = normalize_title(cell);
        norm.contains("согласующ")
            || norm.contains("подпись")
            || norm.contains("фио ответственн")
            || norm.contains("approver")
            || norm.contains("sign-off")
    })
}

fn section_slice(blocks: &[Block], heading_idx: usize, level: u8) -> &[Block] {
    let end = blocks[heading_idx + 1..]
        .iter()
        .position(|block| matches!(block.content, BlockContent::Heading { level: next, .. } if next <= level))
        .map_or(blocks.len(), |offset| heading_idx + 1 + offset);
    &blocks[heading_idx + 1..end]
}

fn block_has_non_placeholder_content(block: &Block) -> bool {
    match &block.content {
        BlockContent::Heading { .. } => false,
        BlockContent::Table { rows, .. } => rows
            .iter()
            .skip(1)
            .flatten()
            .any(|cell| !is_placeholder(cell)),
        BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => !is_placeholder(text),
    }
}

// ============================================================================
// 1. TestingSection — раздел тестирования пуст, заполнен заглушкой или отсутствует
// ============================================================================

pub(crate) struct TestingSection;

#[async_trait]
impl Check for TestingSection {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        // Без заголовков границы раздела не определить, поэтому правило не применяется.
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut blocks = ctx.document.blocks.iter().peekable();
        let mut findings = Vec::new();
        let mut found_section = false;
        while let Some(heading) = blocks.next() {
            let BlockContent::Heading { level, text } = &heading.content else {
                continue;
            };
            if !is_testing_title(text) {
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
                        has_content |=
                            rows.iter().skip(1).flatten().any(|cell| !is_placeholder(cell));
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
                findings.push(rule_finding(
                    "testing-section",
                    format!("testing-section-{}", heading.id.0),
                    Scope::Set,
                    Criterion::SetComplete,
                    evidence,
                    "Раздел тестирования пуст или содержит только заглушки.",
                    "Указать сценарии тестирования и ожидаемые результаты.",
                ));
            }
        }
        // Заголовок раздела иногда оформлен жирным абзацем, а не заголовком Markdown.
        found_section |= ctx
            .document
            .blocks
            .iter()
            .any(|block| match &block.content {
                BlockContent::Paragraph { text } | BlockContent::ListItem { text } => {
                    !text.contains('\n')
                        && text.split_whitespace().count() <= 6
                        && is_testing_title(text)
                }
                _ => false,
            });
        if !found_section {
            findings.push(rule_finding(
                "testing-section",
                "testing-section-missing".into(),
                Scope::Set,
                Criterion::SetComplete,
                first_document_evidence(ctx),
                "В документе отсутствует раздел тестирования.",
                "Указать сценарии тестирования и ожидаемые результаты.",
            ));
        }
        Ok(findings)
    }
}

// ============================================================================
// 2. EmptySection — любой другой раздел пуст или заполнен только заглушками
// ============================================================================

pub(crate) struct EmptySection;

#[async_trait]
impl Check for EmptySection {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let blocks = &ctx.document.blocks;
        let mut findings = Vec::new();
        let mut skip_until_level: Option<u8> = None;

        for (idx, block) in blocks.iter().enumerate() {
            let BlockContent::Heading { level, text } = &block.content else {
                continue;
            };
            if let Some(parent_level) = skip_until_level {
                if *level > parent_level {
                    continue;
                }
                skip_until_level = None;
            }
            // Разделы тестирования, согласующих и критериев приёмки проверяются профильными правилами.
            if is_testing_title(text)
                || is_approvers_title(text)
                || is_acceptance_criteria_title(text)
            {
                skip_until_level = Some(*level);
                continue;
            }

            let body = section_slice(blocks, idx, *level);
            let has_subheadings = body
                .iter()
                .any(|child| matches!(child.content, BlockContent::Heading { .. }));
            // Контейнерный заголовок с подразделами не дублируем — проверяем его листовые подразделы.
            if has_subheadings {
                continue;
            }
            let has_content = body.iter().any(block_has_non_placeholder_content);
            if !has_content {
                let mut evidence = vec![Evidence {
                    quote: text.clone(),
                    location: block.location,
                }];
                if let Some(placeholder_ev) = body.iter().find_map(block_evidence) {
                    evidence.push(placeholder_ev);
                }
                findings.push(rule_finding(
                    "empty-section",
                    format!("empty-section-{}", block.id.0),
                    Scope::Set,
                    Criterion::SetComplete,
                    evidence,
                    format!("Раздел «{text}» пуст или заполнен только заглушками."),
                    "Заполнить раздел конкретными требованиями или явно указать, что раздел не применим к задаче.",
                ));
            }
        }
        Ok(findings)
    }
}

// ============================================================================
// 3. DocumentVersionDate — не указана версия или дата документа
// ============================================================================

pub(crate) struct DocumentVersionDate;

#[async_trait]
impl Check for DocumentVersionDate {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut has_version = false;
        let mut has_date = false;
        let mut explicit_missing_evidence: Option<Evidence> = None;
        let mut in_metadata_section: Option<u8> = None;

        for block in &ctx.document.blocks {
            match &block.content {
                BlockContent::Table { rows, row_lines } => {
                    if is_review_checklist_table(rows) {
                        continue;
                    }
                    // Проверяем колоночную таблицу истории версий (столбцы «Версия» и «Дата»).
                    // Столбец «Дата» без «Версии» учитываем только в разделе метаданных/истории документа.
                    if let Some(header) = rows.first() {
                        let version_col = header.iter().position(|c| is_version_label(c));
                        let date_col = if version_col.is_some() || in_metadata_section.is_some() {
                            header.iter().position(|c| is_date_label(c))
                        } else {
                            None
                        };
                        if version_col.is_some() || date_col.is_some() {
                            for (row_idx, row) in rows.iter().enumerate().skip(1) {
                                if let Some(col) = version_col
                                    && let Some(cell) = row.get(col)
                                {
                                    if !is_placeholder(cell) && contains_version_number(cell) {
                                        has_version = true;
                                    } else if explicit_missing_evidence.is_none()
                                        && !row_quote(row).is_empty()
                                    {
                                        explicit_missing_evidence = Some(Evidence {
                                            quote: row_quote(row),
                                            location: table_row_span(block, row_lines, row_idx),
                                        });
                                    }
                                }
                                if let Some(col) = date_col
                                    && let Some(cell) = row.get(col)
                                {
                                    if !is_placeholder(cell) && contains_date_value(cell) {
                                        has_date = true;
                                    } else if explicit_missing_evidence.is_none()
                                        && !row_quote(row).is_empty()
                                    {
                                        explicit_missing_evidence = Some(Evidence {
                                            quote: row_quote(row),
                                            location: table_row_span(block, row_lines, row_idx),
                                        });
                                    }
                                }
                            }
                        }
                    }

                    // Проверяем таблицу ключ–значение (2 столбца: Поле | Значение).
                    if rows.first().is_some_and(|header| header.len() == 2) {
                        for (row_idx, row) in rows.iter().enumerate().skip(1) {
                            if row.len() < 2 {
                                continue;
                            }
                            let key = &row[0];
                            let value = &row[1];
                            let version_key = is_version_label(key);
                            let date_key = is_date_label(key);
                            if version_key {
                                if !is_placeholder(value) && contains_version_number(value) {
                                    has_version = true;
                                } else if explicit_missing_evidence.is_none()
                                    && !row_quote(row).is_empty()
                                {
                                    explicit_missing_evidence = Some(Evidence {
                                        quote: row_quote(row),
                                        location: table_row_span(block, row_lines, row_idx),
                                    });
                                }
                            }
                            if date_key {
                                if !is_placeholder(value) && contains_date_value(value) {
                                    has_date = true;
                                } else if explicit_missing_evidence.is_none()
                                    && !row_quote(row).is_empty()
                                {
                                    explicit_missing_evidence = Some(Evidence {
                                        quote: row_quote(row),
                                        location: table_row_span(block, row_lines, row_idx),
                                    });
                                }
                            }
                        }
                    }
                }
                BlockContent::Heading { level, text } => {
                    if let Some(meta_level) = in_metadata_section
                        && *level <= meta_level
                    {
                        in_metadata_section = None;
                    }
                    if is_document_metadata_title(text) {
                        in_metadata_section = Some(*level);
                    }
                    for line in text.lines() {
                        if mentions_missing_version_or_date(line) {
                            if explicit_missing_evidence.is_none() && !line.trim().is_empty() {
                                explicit_missing_evidence = Some(Evidence {
                                    quote: line.trim().to_string(),
                                    location: block.location,
                                });
                            }
                            continue;
                        }
                        if line_has_document_version(line) {
                            has_version = true;
                        }
                        if line_has_document_date(line) {
                            has_date = true;
                        }
                    }
                }
                BlockContent::Paragraph { text } | BlockContent::ListItem { text } => {
                    for line in text.lines() {
                        if mentions_missing_version_or_date(line) {
                            if explicit_missing_evidence.is_none() && !line.trim().is_empty() {
                                explicit_missing_evidence = Some(Evidence {
                                    quote: line.trim().to_string(),
                                    location: block.location,
                                });
                            }
                            continue;
                        }
                        if line_has_document_version(line) {
                            has_version = true;
                        }
                        if line_has_document_date(line) {
                            has_date = true;
                        }
                    }
                }
                BlockContent::Code { .. } => {}
            }
        }

        if has_version && has_date {
            return Ok(Vec::new());
        }

        let evidence = explicit_missing_evidence
            .map(|ev| vec![ev])
            .unwrap_or_else(|| first_document_evidence(ctx));

        let (id, explanation, suggestion) = match (has_version, has_date) {
            (false, false) => (
                "document-version-date-missing",
                "В документе не указаны версия и дата документа.",
                "Указать номер версии и дату актуальной редакции документа в паспорте или таблице общих сведений.",
            ),
            (false, true) => (
                "document-version-missing",
                "В документе не указана версия документа.",
                "Добавить номер версии документа в общие сведения или историю изменений.",
            ),
            (true, false) => (
                "document-date-missing",
                "В документе не указана дата документа.",
                "Добавить дату актуальной редакции документа в общие сведения или историю изменений.",
            ),
            (true, true) => unreachable!(),
        };

        Ok(vec![rule_finding(
            "document-version-date",
            id.into(),
            Scope::Set,
            Criterion::Conforming,
            evidence,
            explanation,
            suggestion,
        )])
    }
}

fn is_document_metadata_title(text: &str) -> bool {
    let norm = normalize_title(text);
    norm.contains("общие сведения")
        || norm.contains("паспорт документа")
        || norm.contains("история изменений")
        || norm.contains("история версий")
        || norm.contains("версии документа")
        || norm.contains("о документе")
        || norm.contains("document info")
        || norm.contains("revision history")
        || norm.contains("version history")
        || norm.contains("changelog")
        || norm.contains("metadata")
}

fn is_version_label(text: &str) -> bool {
    let norm = normalize_title(text);
    // Исключаем «версионность» в чек-листах, если там не «версия».
    (norm.contains("версия") || norm.contains("редакция") || norm.contains("version"))
        && !norm.contains("версионност")
}

fn is_date_label(text: &str) -> bool {
    let norm = normalize_title(text)
        .trim()
        .trim_matches(|ch: char| matches!(ch, '*' | '_' | ':' | ' '))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    matches!(
        norm.as_str(),
        "дата"
            | "дата документа"
            | "дата создания"
            | "дата обновления"
            | "дата редакции"
            | "дата утверждения"
            | "дата выпуска"
            | "дата актуализации"
            | "дата изменения"
            | "версия/дата"
            | "версия / дата"
            | "версия и дата"
            | "документ (версия/дата)"
            | "date"
            | "document date"
            | "created"
            | "updated"
            | "last updated"
            | "revision date"
            | "version/date"
    )
}

fn mentions_missing_version_or_date(line: &str) -> bool {
    let norm = normalize_title(line);
    let mentions_meta = norm.contains("версия")
        || norm.contains("версии")
        || norm.contains("version")
        || norm.contains("дата");
    let mentions_missing = norm.contains("не заполнен")
        || norm.contains("не указан")
        || norm.contains("отсутствует")
        || norm.contains("not filled")
        || norm.contains("missing");
    mentions_meta && mentions_missing
}

fn contains_version_number(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.')
        .filter(|tok| !tok.is_empty())
        .any(|tok| {
            let stripped = tok.trim_start_matches(['v', 'V', 'в', 'В']);
            let stripped = stripped.trim_start_matches('.');
            !stripped.is_empty()
                && stripped.chars().next().is_some_and(|c| c.is_ascii_digit())
                && stripped.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

fn line_has_document_version(line: &str) -> bool {
    let norm = normalize_title(line);
    let keywords = ["версия", "версии", "редакция", "version", "ver.", "rev."];
    for kw in keywords {
        if let Some(pos) = norm.find(kw) {
            let after = &norm[pos + kw.len()..];
            let snippet: String = after.chars().take(24).collect();
            if !is_placeholder(&snippet) && contains_version_number(&snippet) {
                return true;
            }
        }
    }
    // Поддержка формата «v1.0» / «v.2.1» как отдельного токена.
    norm.split_whitespace().any(|raw| {
        let tok = raw.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | ';' | ':' | '[' | ']'));
        let Some(rest) = tok.strip_prefix("v.").or_else(|| tok.strip_prefix('v')) else {
            return false;
        };
        rest.contains('.')
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
            && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

fn line_has_document_date(line: &str) -> bool {
    let norm = normalize_title(line);
    // Не считаем датой документа произвольные ссылки вида «Приказ № 5 от 01.02.2024».
    let has_date_context = norm.contains("дата документа")
        || norm.contains("дата создания")
        || norm.contains("дата обновления")
        || norm.contains("дата редакции")
        || norm.contains("дата выпуска")
        || norm.contains("дата:")
        || norm.contains("date:")
        || norm.contains("document date")
        || norm.contains("last updated")
        || norm.contains("updated:")
        || line_has_document_version(line);
    has_date_context && contains_date_value(&norm)
}

fn contains_date_value(text: &str) -> bool {
    let norm = normalize_title(text);
    // 1. ISO YYYY-MM-DD или DD.MM.YYYY / DD/MM/YYYY / DD.MM.YY
    for raw in norm.split(|c: char| !c.is_ascii_digit() && c != '-' && c != '.' && c != '/') {
        let token = raw.trim_matches(['.', '-', '/']);
        if is_iso_date(token) || is_dmy_date(token) {
            return true;
        }
    }
    // 2. Текстовая дата с 4-значным годом (например, «15 марта 2026» или «March 15, 2026»).
    let months = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let has_month = months.iter().any(|month| norm.contains(month));
    let has_year = norm
        .split(|c: char| !c.is_ascii_digit())
        .any(|num| num.len() == 4 && (1990..=2100).contains(&num.parse::<u16>().unwrap_or(0)));
    has_month && has_year
}

fn is_iso_date(token: &str) -> bool {
    let parts: Vec<&str> = token.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let Ok(year) = parts[0].parse::<u16>() else {
        return false;
    };
    let Ok(month) = parts[1].parse::<u8>() else {
        return false;
    };
    let Ok(day) = parts[2].parse::<u8>() else {
        return false;
    };
    (1990..=2100).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn is_dmy_date(token: &str) -> bool {
    let sep = if token.contains('.') {
        '.'
    } else if token.contains('/') {
        '/'
    } else {
        return false;
    };
    let parts: Vec<&str> = token.split(sep).collect();
    if parts.len() != 3 || !(1..=2).contains(&parts[0].len()) || !(1..=2).contains(&parts[1].len())
    {
        return false;
    }
    if !(parts[2].len() == 4 || parts[2].len() == 2) {
        return false;
    }
    let Ok(day) = parts[0].parse::<u8>() else {
        return false;
    };
    let Ok(month) = parts[1].parse::<u8>() else {
        return false;
    };
    let Ok(year) = parts[2].parse::<u16>() else {
        return false;
    };
    let valid_year = if parts[2].len() == 4 {
        (1990..=2100).contains(&year)
    } else {
        year <= 99
    };
    valid_year && (1..=12).contains(&month) && (1..=31).contains(&day)
}

// ============================================================================
// 4. ApproversSection — пустой блок согласующих
// ============================================================================

pub(crate) struct ApproversSection;

#[async_trait]
impl Check for ApproversSection {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let blocks = &ctx.document.blocks;
        let mut findings = Vec::new();

        for (idx, block) in blocks.iter().enumerate() {
            let BlockContent::Heading { level, text } = &block.content else {
                continue;
            };
            if !is_approvers_title(text) {
                continue;
            }
            let body = section_slice(blocks, idx, *level);
            let mut has_approvers = false;
            let mut extra_evidence = None;

            for child in body {
                match &child.content {
                    BlockContent::Heading { .. } => {}
                    BlockContent::Table { rows, .. } => {
                        let name_col = rows.first().and_then(|header| {
                            header.iter().position(|cell| {
                                let norm = normalize_title(cell);
                                norm.contains("фио")
                                    || norm.contains("ответственн")
                                    || norm.contains("согласующ")
                                    || norm.contains("имя")
                                    || norm.contains("name")
                                    || norm.contains("approver")
                                    || norm.contains("owner")
                            })
                        });
                        let filled = rows.iter().skip(1).any(|row| {
                            if let Some(col) = name_col {
                                row.get(col).is_some_and(|cell| !is_placeholder(cell))
                            } else {
                                row.iter().any(|cell| !is_placeholder(cell))
                            }
                        });
                        if filled {
                            has_approvers = true;
                        } else if extra_evidence.is_none() {
                            extra_evidence = block_evidence(child);
                        }
                    }
                    BlockContent::Paragraph { text }
                    | BlockContent::ListItem { text }
                    | BlockContent::Code { text, .. } => {
                        if !is_placeholder(text) {
                            has_approvers = true;
                        } else if extra_evidence.is_none() && !text.trim().is_empty() {
                            extra_evidence = Some(Evidence {
                                quote: text.trim().to_string(),
                                location: child.location,
                            });
                        }
                    }
                }
            }

            if !has_approvers {
                let mut evidence = vec![Evidence {
                    quote: text.clone(),
                    location: block.location,
                }];
                if let Some(ev) = extra_evidence {
                    evidence.push(ev);
                }
                findings.push(rule_finding(
                    "approvers-section",
                    format!("approvers-section-{}", block.id.0),
                    Scope::Set,
                    Criterion::Conforming,
                    evidence,
                    "Блок согласующих пуст или не содержит ФИО ответственных.",
                    "Заполнить блок согласующих: указать подразделения, ФИО ответственных и статус согласования.",
                ));
            }
        }
        Ok(findings)
    }
}

// ============================================================================
// 5. ReviewChecklist — чек-лист ревью без проставленных отметок
// ============================================================================

pub(crate) struct ReviewChecklist;

#[async_trait]
impl Check for ReviewChecklist {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let blocks = &ctx.document.blocks;
        let mut findings = Vec::new();
        let mut in_review_section: Option<(u8, Evidence)> = None;
        let mut unchecked_tasks: Vec<Evidence> = Vec::new();
        let mut review_section_block_id: usize = 0;

        let flush_tasks = |findings: &mut Vec<Finding>,
                           section: &Option<(u8, Evidence)>,
                           section_id: usize,
                           tasks: &mut Vec<Evidence>| {
            if tasks.is_empty() {
                return;
            }
            let count = tasks.len();
            let mut evidence = Vec::new();
            if let Some((_, heading_ev)) = section {
                evidence.push(heading_ev.clone());
            }
            evidence.push(tasks[0].clone());
            findings.push(rule_finding(
                "review-checklist",
                format!("review-checklist-tasks-{section_id}"),
                Scope::Set,
                Criterion::Conforming,
                evidence,
                format!("В чек-листе ревью не проставлены отметки ({count} невыполненных пунктов)."),
                "Провести ревью требований и проставить актуальные отметки по каждому пункту чек-листа.",
            ));
            tasks.clear();
        };

        for block in blocks {
            match &block.content {
                BlockContent::Heading { level, text } => {
                    if let Some((sec_level, _)) = &in_review_section
                        && *level <= *sec_level
                    {
                        flush_tasks(
                            &mut findings,
                            &in_review_section,
                            review_section_block_id,
                            &mut unchecked_tasks,
                        );
                        in_review_section = None;
                    }
                    if is_review_checklist_title(text) {
                        in_review_section = Some((
                            *level,
                            Evidence {
                                quote: text.clone(),
                                location: block.location,
                            },
                        ));
                        review_section_block_id = block.id.0;
                    }
                }
                BlockContent::Table { rows, row_lines } => {
                    if rows.len() < 2 {
                        continue;
                    }
                    let in_review = in_review_section.is_some();
                    if !in_review && !is_review_checklist_table(rows) {
                        continue;
                    }
                    let status_cols: Vec<usize> = rows[0]
                        .iter()
                        .enumerate()
                        .filter(|(_, cell)| is_checklist_status_header(cell))
                        .map(|(col, _)| col)
                        .collect();

                    let mut total_rows = 0usize;
                    let mut unmarked_rows = 0usize;
                    let mut first_unmarked: Option<Evidence> = None;

                    for (row_idx, row) in rows.iter().enumerate().skip(1) {
                        if row.iter().all(|c| c.trim().is_empty()) {
                            continue;
                        }
                        total_rows += 1;
                        let has_unselected = row.iter().any(|cell| is_unselected_choice(cell));
                        let has_empty_status = in_review
                            && !status_cols.is_empty()
                            && status_cols.iter().any(|&col| {
                                row.get(col).is_none_or(|cell| {
                                    is_placeholder(cell) || is_unselected_choice(cell)
                                })
                            });
                        if has_unselected || has_empty_status {
                            unmarked_rows += 1;
                            if first_unmarked.is_none() {
                                let quote = row
                                    .first()
                                    .map(|c| c.trim().to_string())
                                    .filter(|q| !q.is_empty())
                                    .unwrap_or_else(|| row_quote(row));
                                if !quote.is_empty() {
                                    first_unmarked = Some(Evidence {
                                        quote,
                                        location: table_row_span(block, row_lines, row_idx),
                                    });
                                }
                            }
                        }
                    }

                    if unmarked_rows > 0 {
                        let mut evidence = Vec::new();
                        if let Some((_, heading_ev)) = &in_review_section {
                            evidence.push(heading_ev.clone());
                        }
                        if let Some(ev) = first_unmarked {
                            evidence.push(ev);
                        }
                        findings.push(rule_finding(
                            "review-checklist",
                            format!("review-checklist-{}", block.id.0),
                            Scope::Set,
                            Criterion::Conforming,
                            evidence,
                            format!(
                                "В чек-листе ревью не проставлены отметки ({unmarked_rows} из {total_rows} пунктов)."
                            ),
                            "Провести ревью требований и проставить конкретный результат («Да», «Нет» или «Не требуется») по каждому пункту чек-листа.",
                        ));
                    }
                }
                BlockContent::ListItem { text } if in_review_section.is_some() => {
                    let trimmed = text.trim();
                    if trimmed.starts_with("[ ]") || trimmed.starts_with('☐') {
                        unchecked_tasks.push(Evidence {
                            quote: trimmed.to_string(),
                            location: block.location,
                        });
                    }
                }
                _ => {}
            }
        }
        flush_tasks(
            &mut findings,
            &in_review_section,
            review_section_block_id,
            &mut unchecked_tasks,
        );
        Ok(findings)
    }
}

// ============================================================================
// 6. AcceptanceCriteria — требование без критериев приёмки
// ============================================================================

pub(crate) struct AcceptanceCriteria;

#[async_trait]
impl Check for AcceptanceCriteria {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    fn applies_to(&self, ctx: &RunContext<'_>) -> bool {
        has_headings(ctx)
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let blocks = &ctx.document.blocks;

        // 1. Проверяем наличие и заполненность общего раздела критериев приёмки.
        let mut found_ac_section = false;
        let mut empty_ac_findings = Vec::new();
        for (idx, block) in blocks.iter().enumerate() {
            let BlockContent::Heading { level, text } = &block.content else {
                continue;
            };
            if !is_acceptance_criteria_title(text) {
                continue;
            }
            found_ac_section = true;
            let body = section_slice(blocks, idx, *level);
            let has_content = body.iter().any(block_has_non_placeholder_content);
            if !has_content {
                let mut evidence = vec![Evidence {
                    quote: text.clone(),
                    location: block.location,
                }];
                if let Some(ev) = body.iter().find_map(block_evidence) {
                    evidence.push(ev);
                }
                empty_ac_findings.push(rule_finding(
                    "acceptance-criteria",
                    format!("acceptance-criteria-empty-{}", block.id.0),
                    Scope::Set,
                    Criterion::Verifiable,
                    evidence,
                    format!("Раздел «{text}» пуст или заполнен только заглушками."),
                    "Добавить проверяемые критерии приёмки в формате «Исходные условия / Действие / Ожидаемый результат» или Given/When/Then.",
                ));
            }
        }
        if found_ac_section {
            return Ok(empty_ac_findings);
        }

        // 2. Если отдельного раздела приёмки нет, проверяем подразделы отдельных требований (например, ### FR-1).
        let req_headings: Vec<(usize, u8, &Block, &str)> = blocks
            .iter()
            .enumerate()
            .filter_map(|(idx, block)| {
                let BlockContent::Heading { level, text } = &block.content else {
                    return None;
                };
                looks_like_requirement_id_prefix(text).then_some((
                    idx,
                    *level,
                    block,
                    text.as_str(),
                ))
            })
            .collect();

        if !req_headings.is_empty() {
            let mut findings = Vec::new();
            for (idx, level, heading, text) in req_headings {
                let body = section_slice(blocks, idx, level);
                let has_inline_ac = body.iter().any(block_contains_inline_ac);
                if !has_inline_ac {
                    let scope = ctx
                        .requirements
                        .iter()
                        .find(|req| {
                            req.location.page == heading.location.page
                                && req.location.start_line <= heading.location.end_line
                                && heading.location.start_line <= req.location.end_line
                        })
                        .map_or(Scope::Set, |req| Scope::Requirement(req.id.clone()));
                    findings.push(rule_finding(
                        "acceptance-criteria",
                        format!("acceptance-criteria-{}", heading.id.0),
                        scope,
                        Criterion::Verifiable,
                        vec![Evidence {
                            quote: text.to_string(),
                            location: heading.location,
                        }],
                        format!("Для требования «{text}» не указаны критерии приёмки."),
                        "Добавить проверяемые критерии приёмки для этого требования.",
                    ));
                }
            }
            return Ok(findings);
        }

        // 3. Проверяем наличие встроенных критериев приёмки в тексте документа.
        let has_any_inline_ac = blocks.iter().any(block_contains_inline_ac);
        if has_any_inline_ac {
            return Ok(Vec::new());
        }

        // 4. Если требования в документе есть, но критериев приёмки нет нигде — фиксируем пробел.
        let req_evidence = find_requirement_evidence(ctx);
        if let Some(evidence) = req_evidence {
            return Ok(vec![rule_finding(
                "acceptance-criteria",
                "acceptance-criteria-missing".into(),
                Scope::Set,
                Criterion::Verifiable,
                vec![evidence],
                "Для требований документа отсутствуют критерии приёмки.",
                "Добавить раздел «Критерии приёмки» или указать проверяемые условия приёмки для каждого требования.",
            )]);
        }

        Ok(Vec::new())
    }
}

fn looks_like_requirement_id_prefix(text: &str) -> bool {
    let trimmed = text.trim();
    let token = trimmed
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ':' | ')' | '—' | '–'))
        .next()
        .unwrap_or("");
    let Some((prefix, number)) = token.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.chars().count() <= 4
        && prefix.chars().all(|c| c.is_alphabetic())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

fn block_contains_inline_ac(block: &Block) -> bool {
    match &block.content {
        BlockContent::Heading { text, .. } => is_acceptance_criteria_title(text),
        BlockContent::Paragraph { text } | BlockContent::ListItem { text } => {
            let norm = normalize_title(text);
            (norm.contains("критерий приемки")
                || norm.contains("критерии приемки")
                || norm.contains("acceptance criteria")
                || norm.contains("ожидаемый результат")
                || norm.contains("given ") && norm.contains("then ")
                || norm.contains("дано ") && norm.contains("тогда "))
                && !is_placeholder(text)
        }
        BlockContent::Table { rows, .. } => {
            rows.first().is_some_and(|header| {
                header.iter().any(|cell| {
                    let norm = normalize_title(cell);
                    norm.contains("ожидаемый результат") || norm.contains("expected result")
                })
            }) && rows
                .iter()
                .skip(1)
                .flatten()
                .any(|cell| !is_placeholder(cell))
        }
        BlockContent::Code { .. } => false,
    }
}

fn find_requirement_evidence(ctx: &RunContext<'_>) -> Option<Evidence> {
    if let Some(req) = ctx.requirements.first()
        && !req.quote.trim().is_empty()
    {
        return Some(Evidence {
            quote: req.quote.clone(),
            location: req.location,
        });
    }
    for block in &ctx.document.blocks {
        match &block.content {
            BlockContent::Heading { text, .. } if is_requirements_section_title(text) => {
                return Some(Evidence {
                    quote: text.clone(),
                    location: block.location,
                });
            }
            BlockContent::ListItem { text } | BlockContent::Paragraph { text }
                if looks_like_requirement_id_prefix(text) =>
            {
                return Some(Evidence {
                    quote: text.trim().to_string(),
                    location: block.location,
                });
            }
            _ => {}
        }
    }
    None
}

// ============================================================================
// 7. RequiredTableFields — обязательное поле таблицы пустое или «—»
// ============================================================================

pub(crate) struct RequiredTableFields;

#[async_trait]
impl Check for RequiredTableFields {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut in_approvers_section: Option<u8> = None;

        for block in &ctx.document.blocks {
            match &block.content {
                BlockContent::Heading { level, text } => {
                    if let Some(sec_level) = in_approvers_section
                        && *level <= sec_level
                    {
                        in_approvers_section = None;
                    }
                    if is_approvers_title(text) {
                        in_approvers_section = Some(*level);
                    }
                }
                BlockContent::Table { rows, row_lines } => {
                    if rows.len() < 2
                        || in_approvers_section.is_some()
                        || is_review_checklist_table(rows)
                        || is_approvers_table(rows)
                    {
                        continue;
                    }
                    let header = &rows[0];
                    if header.len() == 2 && is_key_value_table(header) {
                        check_key_value_table(block, rows, row_lines, &mut findings);
                    } else if header.len() >= 3 {
                        check_multicolumn_table(block, rows, row_lines, &mut findings);
                    }
                }
                _ => {}
            }
        }
        Ok(findings)
    }
}

fn is_key_value_table(header: &[String]) -> bool {
    if header.len() != 2 {
        return false;
    }
    let key_col = normalize_title(&header[0]);
    let val_col = normalize_title(&header[1]);
    let key_ok = key_col.contains("поле")
        || key_col.contains("параметр")
        || key_col.contains("атрибут")
        || key_col.contains("свойств")
        || key_col.contains("название")
        || key_col.contains("field")
        || key_col.contains("parameter")
        || key_col.contains("key")
        || key_col.contains("property")
        || key_col.contains("name");
    let val_ok = val_col.contains("значен")
        || val_col.contains("описан")
        || val_col.contains("value")
        || val_col.contains("description");
    key_ok && val_ok
}

fn is_optional_field_name(name: &str) -> bool {
    let norm = normalize_title(name);
    norm.contains("примечан")
        || norm.contains("комментар")
        || norm.contains("notes")
        || norm.contains("comment")
        || norm.contains("дополнительно")
        || norm.contains("опциональн")
        || norm.contains("optional")
        || norm.contains("db field")
        || norm.contains("поле бд")
        || norm.contains("пример")
        || norm.contains("example")
        || norm.contains("по умолчанию")
        || norm.contains("default")
}

fn is_required_column_header(name: &str) -> bool {
    if is_optional_field_name(name) {
        return false;
    }
    let norm = normalize_title(name);
    if norm.contains('*') || norm.contains("обязательн") || norm.contains("required") {
        return true;
    }
    let keywords = [
        "parameter",
        "параметр",
        "format",
        "формат",
        "type",
        "тип",
        "mandatory",
        "description",
        "описание",
        "url",
        "endpoint",
        "эндпоинт",
        "исходные условия",
        "предусловие",
        "действие",
        "ожидаемый результат",
        "expected result",
        "версия",
        "version",
    ];
    keywords.iter().any(|kw| norm.contains(kw)) || is_date_label(name)
}

fn check_key_value_table(
    block: &Block,
    rows: &[Vec<String>],
    row_lines: &[usize],
    findings: &mut Vec<Finding>,
) {
    for (row_idx, row) in rows.iter().enumerate().skip(1) {
        if row.len() < 2 {
            continue;
        }
        let key = row[0].trim();
        let value = row[1].trim();
        if key.is_empty() || is_optional_field_name(key) {
            continue;
        }
        let quote = row_quote(row);
        if quote.is_empty() {
            continue;
        }
        let span = table_row_span(block, row_lines, row_idx);

        if is_placeholder(value) {
            findings.push(rule_finding(
                "required-table-field",
                format!("required-table-field-{}-{row_idx}", block.id.0),
                Scope::Set,
                Criterion::Complete,
                vec![Evidence {
                    quote,
                    location: span,
                }],
                format!("Обязательное поле таблицы «{key}» не заполнено (значение: «{value}»)."),
                format!("Указать фактическое значение для поля «{key}»."),
            ));
            continue;
        }

        // Проверяем составные значения вида «Dev: /api/v1 . Prod: —».
        for empty_sub in find_empty_inline_subfields(value) {
            findings.push(rule_finding(
                "required-table-field",
                format!(
                    "required-table-field-{}-{row_idx}-{}",
                    block.id.0,
                    empty_sub.to_lowercase().replace(' ', "-")
                ),
                Scope::Set,
                Criterion::Complete,
                vec![Evidence {
                    quote: quote.clone(),
                    location: span,
                }],
                format!(
                    "В поле таблицы «{key}» не заполнено значение для «{empty_sub}» (указана заглушка)."
                ),
                format!("Указать фактическое значение «{empty_sub}» в поле «{key}»."),
            ));
        }
    }
}

fn check_multicolumn_table(
    block: &Block,
    rows: &[Vec<String>],
    row_lines: &[usize],
    findings: &mut Vec<Finding>,
) {
    let header = &rows[0];
    let required_cols: Vec<(usize, &str)> = header
        .iter()
        .enumerate()
        .filter(|(_, col_name)| is_required_column_header(col_name))
        .map(|(idx, col_name)| (idx, col_name.trim()))
        .collect();
    if required_cols.is_empty() {
        return;
    }

    for (row_idx, row) in rows.iter().enumerate().skip(1) {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let quote = row_quote(row);
        if quote.is_empty() {
            continue;
        }
        let span = table_row_span(block, row_lines, row_idx);
        let mut missing_cols: Vec<&str> = Vec::new();
        let mut missing_subfields: Vec<(&str, String)> = Vec::new();

        for &(col_idx, col_name) in &required_cols {
            let value = row.get(col_idx).map(String::as_str).unwrap_or("").trim();
            if is_placeholder(value) {
                missing_cols.push(col_name);
            } else {
                for empty_sub in find_empty_inline_subfields(value) {
                    missing_subfields.push((col_name, empty_sub));
                }
            }
        }

        if !missing_cols.is_empty() {
            let cols_list = missing_cols
                .iter()
                .map(|name| format!("«{name}»"))
                .collect::<Vec<_>>()
                .join(", ");
            let explanation = if missing_cols.len() == 1 {
                format!("Обязательный столбец таблицы {cols_list} не заполнен в строке {row_idx}.")
            } else {
                format!("Обязательные столбцы таблицы {cols_list} не заполнены в строке {row_idx}.")
            };
            findings.push(rule_finding(
                "required-table-field",
                format!("required-table-field-{}-{row_idx}", block.id.0),
                Scope::Set,
                Criterion::Complete,
                vec![Evidence {
                    quote: quote.clone(),
                    location: span,
                }],
                explanation,
                format!("Заполнить обязательные столбцы ({cols_list}) конкретными значениями."),
            ));
        }
        for (col_name, empty_sub) in missing_subfields {
            findings.push(rule_finding(
                "required-table-field",
                format!(
                    "required-table-field-{}-{row_idx}-{}",
                    block.id.0,
                    empty_sub.to_lowercase().replace(' ', "-")
                ),
                Scope::Set,
                Criterion::Complete,
                vec![Evidence {
                    quote: quote.clone(),
                    location: span,
                }],
                format!("В столбце таблицы «{col_name}» не заполнено значение для «{empty_sub}»."),
                format!("Указать фактическое значение «{empty_sub}» в столбце «{col_name}»."),
            ));
        }
    }
}

fn find_empty_inline_subfields(value: &str) -> Vec<String> {
    let mut results = Vec::new();
    for (colon_idx, ch) in value.char_indices() {
        if ch != ':' {
            continue;
        }
        // Пропускаем URL-схемы «://» и время вида «23:58».
        if value[colon_idx..].starts_with("://") {
            continue;
        }
        let prev_char = value[..colon_idx].chars().next_back();
        let next_char = value[colon_idx + 1..].chars().next();
        if prev_char.is_some_and(|c| c.is_ascii_digit())
            && next_char.is_some_and(|c| c.is_ascii_digit())
        {
            continue;
        }
        // Извлекаем метку слева от двоеточия до ближайшего разделителя.
        let left = &value[..colon_idx];
        let label_start = left
            .rfind(['.', ';', ',', '\n', '|'])
            .map_or(0, |pos| pos + 1);
        let label = left[label_start..].trim();
        if label.is_empty()
            || label.split_whitespace().count() > 2
            || !label.chars().next().is_some_and(char::is_alphabetic)
        {
            continue;
        }
        // Извлекаем значение справа от двоеточия до ближайшего разделителя.
        let right = value[colon_idx + 1..].trim_start();
        let mut end = right.len();
        for (idx, rch) in right.char_indices() {
            if matches!(rch, ';' | ',' | '\n' | '|') {
                end = idx;
                break;
            }
            if rch == '.'
                && right[idx + 1..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
            {
                end = idx;
                break;
            }
            if rch == ':' && !right[idx..].starts_with("://") {
                // Откатываемся до начала следующей метки перед двоеточием.
                end = right[..idx].rfind(char::is_whitespace).unwrap_or(0);
                break;
            }
        }
        let segment = right[..end].trim();
        if is_placeholder(segment) {
            results.push(label.to_string());
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use doc::ParserRegistry;
    use llm::MockProvider;

    use super::*;
    use crate::CheckRegistry;

    async fn run_single_rule<C: Check + 'static>(source: &str, check: C) -> Vec<Finding> {
        let document = ParserRegistry::default()
            .parse(Path::new("spec.md"), source.as_bytes())
            .unwrap();
        let mut registry = CheckRegistry::default();
        registry.register(check);
        let ctx = RunContext::new(document, &MockProvider, &registry);
        let findings = registry.run(CheckPhase::Rules, &ctx).await.unwrap();
        for finding in &findings {
            assert!(
                !finding.evidence.is_empty(),
                "Находка {} без доказательств",
                finding.id
            );
            assert!(
                finding.evidence.iter().all(|evidence| {
                    ctx.document
                        .contains_quote(evidence.location, &evidence.quote)
                }),
                "Неподтверждённая цитата в находке {}: {:?}",
                finding.id,
                finding.evidence
            );
        }
        findings
    }

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
            let findings = run_single_rule(source, TestingSection).await;
            assert_eq!(findings.len(), expected, "{source}");
            for finding in findings {
                assert!(
                    matches!(finding.detector, Detector::Rule(ref id) if id == "testing-section")
                );
                assert_eq!(finding.severity, Severity::Medium);
            }
        }
    }

    #[tokio::test]
    async fn empty_section_rule_fires_on_empty_or_placeholder_and_passes_on_filled() {
        let bad = concat!(
            "# Спецификация платежей\n\n",
            "## Безопасность\n\n",
            "## Интеграция\n\n",
            "Заполняется архитектором позже.\n\n",
            "## Откат\n\n",
            "см. позже\n\n",
            "## Мониторинг\n\n",
            "Метрики отправляются в Prometheus каждые 15 секунд.\n"
        );
        let findings = run_single_rule(bad, EmptySection).await;
        assert_eq!(findings.len(), 3, "{findings:#?}");
        for finding in &findings {
            assert!(matches!(finding.detector, Detector::Rule(ref id) if id == "empty-section"));
            assert_eq!(finding.criterion, Criterion::SetComplete);
        }

        let good = concat!(
            "# Спецификация платежей\n\n",
            "## Архитектура\n\n",
            "### Сервисы\n\n",
            "Платежи проходят через шлюз эквайринга.\n\n",
            "## Path-параметры\n\n",
            "Не используются.\n"
        );
        let findings = run_single_rule(good, EmptySection).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn version_date_rule_fires_when_missing_and_passes_when_present() {
        let missing_both = concat!(
            "# Регламент переводов\n\n",
            "## Описание\n\n",
            "Переводы выполняются в тенге согласно Приказу № 5 от 01.02.2024.\n\n",
            "| Дата операции | Сумма |\n| --- | --- |\n| 2026-01-01 | 5000 |\n"
        );
        let findings = run_single_rule(missing_both, DocumentVersionDate).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "document-version-date-missing");

        let missing_date =
            "# Регламент переводов\n\n| Поле | Значение |\n| --- | --- |\n| Версия | 1.2 |\n";
        let findings = run_single_rule(missing_date, DocumentVersionDate).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "document-date-missing");

        let present_table = concat!(
            "# Регламент переводов\n\n",
            "| Поле | Значение |\n| --- | --- |\n",
            "| Версия документа | 2.0 |\n",
            "| Дата обновления | 2026-04-12 |\n"
        );
        let findings = run_single_rule(present_table, DocumentVersionDate).await;
        assert!(findings.is_empty(), "{findings:#?}");

        let present_inline = "# Регламент переводов (версия 1.4 от 15.05.2026)\n\nТекст.\n";
        let findings = run_single_rule(present_inline, DocumentVersionDate).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn approvers_rule_fires_on_empty_table_and_passes_when_filled() {
        let empty = concat!(
            "# Документ\n\n",
            "## Лист согласования\n\n",
            "| Подразделение | ФИО ответственного | Подпись |\n",
            "| --- | --- | --- |\n",
            "| | | |\n"
        );
        let findings = run_single_rule(empty, ApproversSection).await;
        assert_eq!(findings.len(), 1);
        assert!(
            matches!(findings[0].detector, Detector::Rule(ref id) if id == "approvers-section")
        );

        let filled = concat!(
            "# Документ\n\n",
            "## Лист согласования\n\n",
            "| Подразделение | ФИО ответственного | Подпись |\n",
            "| --- | --- | --- |\n",
            "| Архитектура | Ахметов К.С. | Согласовано |\n"
        );
        let findings = run_single_rule(filled, ApproversSection).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn review_checklist_rule_fires_on_unmarked_and_passes_on_completed() {
        let unmarked = concat!(
            "# Документ\n\n",
            "## Чек-лист ревью\n\n",
            "| Проверка | Статус |\n",
            "| --- | --- |\n",
            "| Описаны коды ошибок | Да / Нет |\n",
            "| Проверена безопасность | Да / Нет / Не требуется |\n"
        );
        let findings = run_single_rule(unmarked, ReviewChecklist).await;
        assert_eq!(findings.len(), 1);
        assert!(findings[0].explanation.contains("2 из 2"));

        let unmarked_tasks = concat!(
            "# Документ\n\n",
            "## Ревью спецификации\n\n",
            "* [ ] Проверить ограничения API\n",
            "* [ ] Согласовать модель данных\n"
        );
        let findings = run_single_rule(unmarked_tasks, ReviewChecklist).await;
        assert_eq!(findings.len(), 1);

        let completed = concat!(
            "# Документ\n\n",
            "## Чек-лист ревью\n\n",
            "| Проверка | Статус |\n",
            "| --- | --- |\n",
            "| Описаны коды ошибок | Да |\n",
            "| Проверена безопасность | Не требуется |\n"
        );
        let findings = run_single_rule(completed, ReviewChecklist).await;
        assert!(findings.is_empty(), "{findings:#?}");

        // Таблица с «Да / Нет» вне раздела ревью (описание типа поля) не является чек-листом.
        let boolean_field_table = concat!(
            "# Документ\n\n",
            "## Параметры карточки\n\n",
            "| Поле | Значение |\n",
            "| --- | --- |\n",
            "| Признак блокировки | Да / Нет |\n"
        );
        let findings = run_single_rule(boolean_field_table, ReviewChecklist).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn acceptance_criteria_rule_fires_when_missing_and_passes_when_present() {
        let missing = concat!(
            "# Сервис квитанций\n\n",
            "## Функциональные требования\n\n",
            "* **FR-1.** Система формирует PDF-квитанцию после оплаты.\n"
        );
        let findings = run_single_rule(missing, AcceptanceCriteria).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].criterion, Criterion::Verifiable);

        let partial = concat!(
            "# Сервис квитанций\n\n",
            "### FR-1. Формирование PDF\n\n",
            "Система формирует PDF.\n\n",
            "Критерий приёмки: файл открывается и содержит номер платежа.\n\n",
            "### FR-2. Отправка на почту\n\n",
            "Система отправляет письмо клиенту.\n"
        );
        let findings = run_single_rule(partial, AcceptanceCriteria).await;
        assert_eq!(findings.len(), 1);
        assert!(findings[0].explanation.contains("FR-2"));

        let present = concat!(
            "# Сервис квитанций\n\n",
            "## Функциональные требования\n\n",
            "* **FR-1.** Система формирует PDF-квитанцию после оплаты.\n\n",
            "## Acceptance Criteria\n\n",
            "* После оплаты клиент получает PDF размером не более 2 МБ.\n"
        );
        let findings = run_single_rule(present, AcceptanceCriteria).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn required_table_fields_rule_catches_dashes_and_inline_prod_url_and_ignores_optional() {
        let bad = concat!(
            "# Контракт API\n\n",
            "| Поле | Значение |\n",
            "| --- | --- |\n",
            "| Endpoint | Dev: `/v1/orders` . Prod: — |\n",
            "| OpenAPI | — |\n",
            "| Примечания | — |\n\n",
            "| Parameter | Type | Description | DB field |\n",
            "| --- | --- | --- | --- |\n",
            "| `orderId` | string | Идентификатор заказа | `order_id` |\n",
            "| `status` | — | Статус заказа | — |\n"
        );
        let findings = run_single_rule(bad, RequiredTableFields).await;
        assert_eq!(findings.len(), 3, "{findings:#?}");
        assert!(findings.iter().any(|f| f.explanation.contains("Prod")));
        assert!(findings.iter().any(|f| f.explanation.contains("OpenAPI")));
        assert!(findings.iter().any(|f| f.explanation.contains("Type")));

        let good = concat!(
            "# Контракт API\n\n",
            "| Поле | Значение |\n",
            "| --- | --- |\n",
            "| Endpoint | Dev: https://dev.local/v1/orders; Prod: https://api.local/v1/orders |\n",
            "| OpenAPI | https://api.local/docs |\n",
            "| Примечания | — |\n\n",
            "| Parameter | Type | Description | DB field |\n",
            "| --- | --- | --- | --- |\n",
            "| `status` | string | Статус ответа | — |\n"
        );
        let findings = run_single_rule(good, RequiredTableFields).await;
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[tokio::test]
    async fn builtin_rules_detect_expected_defects_in_sample_requirements() {
        let sample_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/requirements.md");
        let bytes = std::fs::read(&sample_path).unwrap();
        let document = ParserRegistry::default()
            .parse(&sample_path, &bytes)
            .unwrap();
        let registry = CheckRegistry::with_builtin_rules();
        let ctx = RunContext::new(document, &MockProvider, &registry);
        let findings = registry.run(CheckPhase::Rules, &ctx).await.unwrap();

        let has_rule = |rule_id: &str| {
            findings
                .iter()
                .any(|f| matches!(&f.detector, Detector::Rule(id) if id == rule_id))
        };
        assert!(
            has_rule("testing-section"),
            "Не найден пустой раздел тестирования"
        );
        assert!(
            has_rule("document-version-date"),
            "Не найдена незаполненная версия/дата документа"
        );
        assert!(
            has_rule("approvers-section"),
            "Не найден пустой блок согласующих"
        );
        assert!(
            has_rule("review-checklist"),
            "Не найден незаполненный чек-лист ревью"
        );
        assert!(
            findings.iter().any(|f| {
                matches!(&f.detector, Detector::Rule(id) if id == "required-table-field")
                    && f.explanation.contains("Prod")
            }),
            "Не найден незаполненный Prod URL: {findings:#?}"
        );
        for finding in &findings {
            assert!(
                finding
                    .evidence
                    .iter()
                    .all(|ev| { ctx.document.contains_quote(ev.location, &ev.quote) }),
                "Неподтверждённая цитата в {}: {:?}",
                finding.id,
                finding.evidence
            );
        }
    }
}
