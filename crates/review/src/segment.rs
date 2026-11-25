use doc::{Block, BlockContent, Document, SourceSpan};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{Requirement, TableRef};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RequirementDraft {
    pub(crate) text: String,
    pub(crate) quote: String,
    pub(crate) location: SourceSpan,
}

#[derive(Debug, Clone, Default)]
struct BlockContext {
    part: Option<String>,
    section: Option<String>,
    heading: Option<String>,
}

/// Детерминированно извлекает идентификатор требования из текста блока, заголовка или ячейки
/// (например, `Ф-1`, `НФ-2`, `ФТ-1.2`, `REQ-001`, `"Требование Ф-5: ..."`).
pub fn extract_source_id(text: &str) -> Option<String> {
    extract_source_id_at_start(text).or_else(|| extract_source_id_mid_sentence(text))
}

fn extract_source_id_at_start(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    if rest.is_empty() {
        return None;
    }

    // Убираем ведущий маркер списка (`* `, `- `, `• `, `– `, `— `) или нумерацию заголовка (`1.2. `).
    if let Some(after_bullet) = strip_list_bullet(rest) {
        rest = after_bullet.trim_start();
    } else if let Some(after_num) = strip_leading_numbering(rest) {
        rest = after_num.trim_start();
    }

    parse_id_at(rest)
}

fn extract_source_id_mid_sentence(text: &str) -> Option<String> {
    let mut words_seen = 0usize;
    let mut prev_word = String::new();
    let mut chars = text.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let slice = &text[index..];
        let at_word_start = index == 0
            || text[..index]
                .chars()
                .next_back()
                .is_some_and(|prev| prev.is_whitespace() || matches!(prev, '(' | '[' | '«' | '"'));

        if at_word_start
            && (1..=3).contains(&words_seen)
            && is_intro_label(&prev_word)
            && let Some(id) = parse_id_at(slice)
        {
            return Some(id);
        }

        // Пропускаем текущее слово для отслеживания предыдущего слова и позиции.
        let mut word_end = index + ch.len_utf8();
        while let Some(&(next_idx, next_ch)) = chars.peek() {
            if next_ch.is_whitespace() {
                break;
            }
            word_end = next_idx + next_ch.len_utf8();
            chars.next();
        }
        prev_word = text[index..word_end]
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
            .to_lowercase();
        if !prev_word.is_empty() {
            words_seen += 1;
            if words_seen > 3 {
                break;
            }
        }
    }
    None
}

fn parse_id_at(input: &str) -> Option<String> {
    let mut rest = trim_leading_emphasis(input);
    let closing = match rest.chars().next()? {
        '[' => {
            rest = rest[1..].trim_start();
            Some(']')
        }
        '(' => {
            rest = rest[1..].trim_start();
            Some(')')
        }
        _ => None,
    };

    let (id, after_id) = scan_id_token(rest)?;
    if let Some(expected_close) = closing {
        let trimmed = trim_leading_emphasis(after_id.trim_start());
        if !trimmed.starts_with(expected_close) {
            return None;
        }
        let after_close = &trimmed[expected_close.len_utf8()..];
        if after_close
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
            || is_id_range_continuation(after_close)
        {
            return None;
        }
    } else if is_id_range_continuation(after_id) || !has_valid_id_boundary(after_id) {
        return None;
    }

    Some(id)
}

fn scan_id_token(input: &str) -> Option<(String, &str)> {
    let mut rest = input;
    let mut prefix_end = 0;
    let mut prefix_chars = 0usize;
    for (index, ch) in rest.char_indices() {
        if ch.is_alphabetic() && ch.is_uppercase() {
            prefix_end = index + ch.len_utf8();
            prefix_chars += 1;
            if prefix_chars > 6 {
                return None;
            }
        } else {
            break;
        }
    }
    if prefix_chars == 0 {
        return None;
    }
    let prefix = &rest[..prefix_end];
    if is_technical_acronym(prefix) {
        return None;
    }
    rest = &rest[prefix_end..];

    let sep_char = rest.chars().next()?;
    let normalized_sep = match sep_char {
        '-' | '\u{2011}' | '–' => '-',
        '_' => '_',
        _ => return None,
    };
    rest = &rest[sep_char.len_utf8()..];

    let number_len = scan_id_number(rest)?;
    let number = &rest[..number_len];
    rest = &rest[number_len..];

    Some((format!("{prefix}{normalized_sep}{number}"), rest))
}

fn is_id_range_continuation(rest: &str) -> bool {
    let rest = trim_leading_emphasis(rest).trim_start();
    let after_connector = rest
        .strip_prefix('–')
        .or_else(|| rest.strip_prefix('—'))
        .or_else(|| rest.strip_prefix('-'))
        .or_else(|| rest.strip_prefix('…'))
        .or_else(|| rest.strip_prefix("..."))
        .or_else(|| rest.strip_prefix(".."));
    let Some(after_connector) = after_connector else {
        return false;
    };
    let next = trim_leading_emphasis(after_connector.trim_start())
        .trim_start_matches(['[', '('])
        .trim_start();
    scan_id_token(next).is_some()
}

fn strip_list_bullet(text: &str) -> Option<&str> {
    let first = text.chars().next()?;
    if !matches!(first, '*' | '-' | '•' | '–' | '—') {
        return None;
    }
    let after = &text[first.len_utf8()..];
    if after.chars().next().is_some_and(char::is_whitespace) {
        Some(after)
    } else {
        None
    }
}

fn strip_leading_numbering(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 {
        return None;
    }
    while index + 1 < bytes.len() && bytes[index] == b'.' && bytes[index + 1].is_ascii_digit() {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    if index < bytes.len() && matches!(bytes[index], b'.' | b')') {
        index += 1;
    }
    if index < bytes.len()
        && text[index..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        Some(&text[index..])
    } else {
        None
    }
}

fn trim_leading_emphasis(mut text: &str) -> &str {
    loop {
        let next = text.trim_start_matches(['*', '_', '`']);
        if next.len() == text.len() {
            return text;
        }
        text = next;
    }
}

fn scan_id_number(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 {
        return None;
    }
    while index + 1 < bytes.len() && bytes[index] == b'.' && bytes[index + 1].is_ascii_digit() {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    Some(index)
}

fn has_valid_id_boundary(rest: &str) -> bool {
    let rest = trim_leading_emphasis(rest);
    if rest.is_empty() {
        return true;
    }
    let first = rest.chars().next().expect("непустой остаток");
    match first {
        '.' => {
            let after_dot = trim_leading_emphasis(&rest[1..]);
            after_dot.is_empty()
                || after_dot
                    .chars()
                    .next()
                    .is_some_and(|ch| !ch.is_alphanumeric())
        }
        ':' | ')' | ']' | '—' | '–' => true,
        ch if ch.is_whitespace() => {
            let after_ws = rest.trim_start();
            if after_ws.is_empty() {
                return true;
            }
            if let Some(after_dash) = after_ws
                .strip_prefix('-')
                .or_else(|| after_ws.strip_prefix('—'))
                .or_else(|| after_ws.strip_prefix('–'))
                && after_dash.chars().next().is_some_and(char::is_whitespace)
            {
                return true;
            }
            after_ws
                .chars()
                .next()
                .is_some_and(|next| next.is_uppercase())
        }
        _ => false,
    }
}

fn is_intro_label(word: &str) -> bool {
    matches!(
        word,
        "требование"
            | "пункт"
            | "правило"
            | "критерий"
            | "сценарий"
            | "кейс"
            | "номер"
            | "код"
            | "id"
            | "requirement"
            | "rule"
            | "item"
            | "case"
    )
}

fn is_technical_acronym(prefix: &str) -> bool {
    matches!(
        prefix,
        "UTF"
            | "ISO"
            | "IEC"
            | "IEEE"
            | "HTTP"
            | "HTTPS"
            | "TLS"
            | "SSL"
            | "SHA"
            | "MD"
            | "ASCII"
            | "RFC"
            | "COVID"
            | "GOST"
            | "ГОСТ"
            | "UUID"
            | "JWT"
            | "OAUTH"
            | "PCI"
            | "BASE"
            | "AES"
            | "RSA"
            | "HMAC"
            | "POSIX"
    )
}

/// Детерминированно превращает строки таблиц документа (критерии приёмки, параметры API,
/// статусы и т.п.) в отдельные требования со ссылкой на таблицу, строку, часть и раздел.
pub fn extract_table_requirements(document: &Document) -> Vec<Requirement> {
    let contexts = index_block_contexts(document);
    let mut requirements = Vec::new();
    let mut table_index = 0usize;
    for (block, context) in document.blocks.iter().zip(contexts.iter()) {
        let BlockContent::Table { rows, row_lines } = &block.content else {
            continue;
        };
        table_index += 1;
        if rows.len() < 2 || is_non_requirement_table(context, &rows[0], &rows[1..]) {
            continue;
        }
        let headers = &rows[0];
        for (row_index, row) in rows.iter().enumerate().skip(1) {
            if row.iter().all(|cell| cell.trim().is_empty()) {
                continue;
            }
            let quote = format_row_quote(row);
            let text = format_row_text(headers, row);
            let source_id = row
                .first()
                .and_then(|cell| extract_source_id(cell))
                .or_else(|| extract_source_id(&quote));
            let location = row_lines
                .get(row_index)
                .map_or(block.location, |&line| SourceSpan {
                    start_line: line,
                    end_line: line,
                    page: block.location.page,
                });
            requirements.push(Requirement {
                id: format!("R-{:03}", requirements.len() + 1),
                source_id,
                text,
                quote,
                location,
                part: context.part.clone(),
                section: context.section.clone(),
                table: Some(TableRef {
                    block_id: block.id,
                    index: table_index,
                    row: row_index,
                    title: context.heading.clone(),
                }),
            });
        }
    }
    requirements
}

pub(crate) fn non_table_blocks(document: &Document) -> Vec<&Block> {
    document
        .blocks
        .iter()
        .filter(|block| !matches!(block.content, BlockContent::Table { .. }))
        .collect()
}

pub(crate) fn has_non_table_content(document: &Document) -> bool {
    document.blocks.iter().any(|block| {
        matches!(
            block.content,
            BlockContent::Paragraph { .. }
                | BlockContent::ListItem { .. }
                | BlockContent::Code { .. }
        )
    })
}

pub(crate) fn merge_requirements(
    document: &Document,
    drafts: Vec<RequirementDraft>,
) -> Vec<Requirement> {
    let contexts = index_block_contexts(document);
    let table_requirements = extract_table_requirements(document);

    let mut merged = Vec::with_capacity(drafts.len() + table_requirements.len());
    for draft in drafts {
        if draft.text.trim().is_empty() && draft.quote.trim().is_empty() {
            continue;
        }
        if is_duplicate_table_draft(document, &draft) {
            continue;
        }
        let matched = find_matching_block(document, draft.location, &draft.quote);
        let context = matched
            .or_else(|| find_preceding_block(document, draft.location))
            .map(|index| &contexts[index]);
        let source_id = extract_source_id(&draft.quote)
            .or_else(|| extract_source_id(&draft.text))
            .or_else(|| {
                matched.and_then(|index| {
                    extract_source_id_from_block(&document.blocks[index], draft.location)
                })
            })
            .or_else(|| {
                context
                    .and_then(|ctx| ctx.heading.as_deref())
                    .and_then(extract_source_id)
            });
        merged.push(Requirement {
            id: String::new(),
            source_id,
            text: draft.text,
            quote: draft.quote,
            location: draft.location,
            part: context.and_then(|ctx| ctx.part.clone()),
            section: context.and_then(|ctx| ctx.section.clone()),
            table: None,
        });
    }

    merged.extend(table_requirements);
    merged.sort_by_key(|requirement| {
        (
            requirement.location.page.unwrap_or(0),
            requirement.location.start_line,
            requirement.location.end_line,
        )
    });
    for (index, requirement) in merged.iter_mut().enumerate() {
        requirement.id = format!("R-{:03}", index + 1);
    }
    merged
}

fn index_block_contexts(document: &Document) -> Vec<BlockContext> {
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut contexts = Vec::with_capacity(document.blocks.len());
    let title = document.title.trim();

    for block in &document.blocks {
        if let BlockContent::Heading { level, text } = &block.content {
            let trimmed = text.trim().to_owned();
            while stack
                .last()
                .is_some_and(|(top_level, _)| *top_level >= *level)
            {
                stack.pop();
            }
            if !trimmed.is_empty() {
                stack.push((*level, trimmed));
            }
        }
        contexts.push(build_block_context(&stack, title));
    }
    contexts
}

fn build_block_context(stack: &[(u8, String)], title: &str) -> BlockContext {
    let part_index = stack.iter().rposition(|(_, text)| is_part_heading(text));
    let part = part_index.map(|index| stack[index].1.clone());

    let section_parts: Vec<&str> = stack
        .iter()
        .enumerate()
        .filter(|(index, (level, text))| {
            if Some(*index) == part_index {
                return false;
            }
            if *index == 0 && *level == 1 && text == title {
                return false;
            }
            true
        })
        .map(|(_, (_, text))| text.as_str())
        .collect();

    let heading = section_parts
        .last()
        .copied()
        .map(str::to_owned)
        .or_else(|| part.clone());
    let section = (!section_parts.is_empty()).then(|| section_parts.join(" / "));

    BlockContext {
        part,
        section,
        heading,
    }
}

fn is_part_heading(text: &str) -> bool {
    let trimmed = text.trim();
    let after_prefix = strip_case_insensitive_prefix(trimmed, "часть ")
        .or_else(|| strip_case_insensitive_prefix(trimmed, "part "));
    let Some(rest) = after_prefix else {
        return false;
    };
    let rest = rest.trim_start();
    let token_end = rest
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '.' | ':' | '—' | '–' | '-' | ')'))
        .unwrap_or(rest.len());
    let token = &rest[..token_end];
    if token.is_empty() {
        return false;
    }
    let single_uppercase_letter = {
        let mut chars = token.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() && ch.is_uppercase())
            && chars.next().is_none()
    };
    let numeric = token.bytes().all(|byte| byte.is_ascii_digit());
    let roman = token.len() <= 4 && token.bytes().all(|byte| matches!(byte, b'I' | b'V' | b'X'));
    single_uppercase_letter || numeric || roman
}

fn strip_case_insensitive_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut text_chars = text.chars();
    for expected in prefix.chars() {
        let actual = text_chars.next()?;
        if actual.to_lowercase().ne(expected.to_lowercase()) {
            return None;
        }
    }
    Some(text_chars.as_str())
}

fn normalize_heading_title(heading: &str) -> String {
    let trimmed = heading.trim();
    let stripped = strip_leading_numbering(trimmed).unwrap_or(trimmed).trim();
    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_non_requirement_table(
    context: &BlockContext,
    headers: &[String],
    data_rows: &[Vec<String>],
) -> bool {
    let title = context.heading.as_deref().map(normalize_heading_title);
    let lower_headers: Vec<String> = headers
        .iter()
        .map(|header| header.trim().to_lowercase())
        .collect();

    // 1. Паспорт / общие сведения документа (включая нумерованные «1. Общие сведения»
    // и любые варианты колонок вроде «Наименование | Значение» или «Реквизит | Значение»).
    let is_passport_heading = matches!(
        title.as_deref(),
        Some(
            "общие сведения"
                | "паспорт документа"
                | "сведения о документе"
                | "метаданные"
                | "метаданные документа"
                | "общая информация"
        )
    );
    if is_passport_heading {
        return true;
    }

    // 2. Чек-лист ревью требований (по заголовкам таблицы или явному чек-листу ревью).
    let is_review_checklist_headers = lower_headers
        .iter()
        .any(|h| h == "проверено" || h == "статус проверки")
        && lower_headers
            .iter()
            .any(|h| h.contains("проверка") || h.contains("создано") || h.contains("примечан"));
    let is_review_checklist_heading = matches!(
        title.as_deref(),
        Some(
            "ревью требований"
                | "чек-лист"
                | "чеклист"
                | "чек-лист ревью"
                | "чек-лист проверки требований"
                | "review checklist"
        )
    );
    let has_yes_no_template = data_rows.iter().any(|row| {
        row.iter().any(|cell| {
            let value = cell.trim().to_lowercase();
            value == "да / нет" || value == "да/нет" || value.starts_with("да / нет")
        })
    });
    if is_review_checklist_headers || (is_review_checklist_heading && has_yes_no_template) {
        return true;
    }

    // 3. Блок согласующих / лист согласования (по колонкам подписи или точному названию раздела).
    let is_signoff_headers = lower_headers
        .iter()
        .any(|h| h == "подпись" || h.contains("фио ответственного"));
    let is_signoff_heading = matches!(
        title.as_deref(),
        Some(
            "блок согласующих"
                | "лист согласования"
                | "согласующие"
                | "согласование документа"
                | "утверждение документа"
                | "подписи"
        )
    );
    if is_signoff_headers || is_signoff_heading {
        return true;
    }

    // 4. История изменений / версии документа.
    let is_changelog_heading = matches!(
        title.as_deref(),
        Some(
            "история изменений"
                | "история версий"
                | "лист регистрации изменений"
                | "версии документа"
                | "changelog"
                | "revision history"
        )
    );
    let is_changelog_headers = lower_headers.iter().any(|h| h == "версия")
        && lower_headers.iter().any(|h| h == "дата" || h == "автор")
        && lower_headers
            .iter()
            .any(|h| h.contains("изменен") || h.contains("описан"));
    is_changelog_heading || is_changelog_headers
}

fn is_key_value_headers(headers: &[String]) -> bool {
    if headers.len() != 2 {
        return false;
    }
    let key = headers[0].trim().to_lowercase();
    let value = headers[1].trim().to_lowercase();
    let is_key = matches!(
        key.as_str(),
        "поле"
            | "параметр"
            | "ключ"
            | "атрибут"
            | "свойство"
            | "field"
            | "key"
            | "name"
            | "property"
    );
    let is_value = matches!(
        value.as_str(),
        "значение" | "описание" | "value" | "description"
    );
    is_key && is_value
}

fn format_row_quote(row: &[String]) -> String {
    row.iter()
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn format_row_text(headers: &[String], row: &[String]) -> String {
    if is_key_value_headers(headers) {
        let key = row.first().map(|cell| cell.trim()).unwrap_or_default();
        let value = row.get(1).map(|cell| cell.trim()).unwrap_or_default();
        if !key.is_empty() && !value.is_empty() {
            return format!("{key}: {value}");
        }
    }

    let mut parts = Vec::new();
    for (index, cell) in row.iter().enumerate() {
        let value = cell.trim();
        if value.is_empty() {
            continue;
        }
        let header = headers
            .get(index)
            .map(|item| item.trim())
            .unwrap_or_default();
        if header.is_empty() {
            parts.push(value.to_owned());
        } else {
            parts.push(format!("{header}: {value}"));
        }
    }
    parts.join("; ")
}

fn is_duplicate_table_draft(document: &Document, draft: &RequirementDraft) -> bool {
    if matches_non_table_block(document, draft.location, &draft.quote) {
        return false;
    }
    document.blocks.iter().any(|block| {
        if !matches!(block.content, BlockContent::Table { .. })
            || block.location.page != draft.location.page
            || block.location.start_line > draft.location.end_line
            || draft.location.start_line > block.location.end_line
        {
            return false;
        }
        let strictly_inside = block.location.start_line <= draft.location.start_line
            && draft.location.end_line <= block.location.end_line;
        strictly_inside || table_block_contains_quote(block, draft.location, &draft.quote)
    })
}

fn matches_non_table_block(document: &Document, span: SourceSpan, quote: &str) -> bool {
    let normalized = normalize(quote);
    if normalized.is_empty() {
        return false;
    }
    document.blocks.iter().any(|block| {
        block.location.page == span.page
            && block.location.start_line <= span.end_line
            && span.start_line <= block.location.end_line
            && block_plain_text(&block.content)
                .is_some_and(|text| normalize(&text).contains(&normalized))
    })
}

fn table_block_contains_quote(block: &Block, span: SourceSpan, quote: &str) -> bool {
    let normalized = normalize(quote);
    if normalized.is_empty() {
        return false;
    }
    let BlockContent::Table { rows, row_lines } = &block.content else {
        return false;
    };
    let includes = |line| (span.start_line..=span.end_line).contains(&line);
    let covers_block = includes(block.location.start_line) && includes(block.location.end_line);
    let matches = |text: &str| normalize(text).contains(&normalized);
    rows.iter().enumerate().any(|(index, row)| {
        let non_empty: Vec<_> = row
            .iter()
            .map(|cell| cell.trim())
            .filter(|cell| !cell.is_empty())
            .collect();
        (covers_block || row_lines.get(index).is_some_and(|line| includes(*line)))
            && (row.iter().any(|cell| matches(cell))
                || matches(&row.join(" "))
                || matches(&row.join(" | "))
                || matches(&non_empty.join(" | ")))
    })
}

fn find_matching_block(document: &Document, span: SourceSpan, quote: &str) -> Option<usize> {
    let mut best: Option<(bool, usize, usize)> = None;
    let normalized_quote = normalize(quote);

    for (index, block) in document.blocks.iter().enumerate() {
        if matches!(block.content, BlockContent::Table { .. })
            || block.location.page != span.page
            || block.location.start_line > span.end_line
            || span.start_line > block.location.end_line
        {
            continue;
        }
        let block_text = block_plain_text(&block.content);
        let quote_matches = !normalized_quote.is_empty()
            && block_text
                .as_deref()
                .is_some_and(|text| normalize(text).contains(&normalized_quote));
        let span_len = block
            .location
            .end_line
            .saturating_sub(block.location.start_line);

        let is_better = match best {
            None => true,
            Some((best_match, best_len, _)) => {
                (quote_matches && !best_match)
                    || (quote_matches == best_match && span_len < best_len)
            }
        };
        if is_better {
            best = Some((quote_matches, span_len, index));
        }
    }
    best.map(|(_, _, index)| index)
}

fn find_preceding_block(document: &Document, span: SourceSpan) -> Option<usize> {
    document
        .blocks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, block)| {
            block.location.page == span.page && block.location.start_line <= span.start_line
        })
        .map(|(index, _)| index)
}

fn extract_source_id_from_block(block: &Block, span: SourceSpan) -> Option<String> {
    let text = match &block.content {
        BlockContent::Paragraph { text } | BlockContent::ListItem { text } => text,
        _ => return None,
    };
    let line_count = block
        .location
        .end_line
        .saturating_sub(block.location.start_line)
        + 1;
    if text.lines().count() == line_count
        && (span.start_line > block.location.start_line || span.end_line < block.location.end_line)
    {
        let selected: Vec<&str> = text
            .lines()
            .enumerate()
            .filter(|(offset, _)| {
                let line = block.location.start_line + offset;
                (span.start_line..=span.end_line).contains(&line)
            })
            .map(|(_, line)| line)
            .collect();
        if let Some(first) = selected.first() {
            return extract_source_id(first);
        }
    }
    extract_source_id(text)
}

fn block_plain_text(content: &BlockContent) -> Option<String> {
    match content {
        BlockContent::Heading { text, .. }
        | BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => Some(text.clone()),
        BlockContent::Table { .. } => None,
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['«', '»', '“', '”', '„', '‟', '‹', '›'], "\"")
        .replace(['‘', '’', '‚', '‛'], "'")
}
