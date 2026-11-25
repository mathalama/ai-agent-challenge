use std::collections::HashSet;

use doc::{Block, BlockContent, BlockId, Document, SourceSpan};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{ContextItem, ContextKind, Requirement, TableRef};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RequirementDraft {
    block_id: BlockId,
    start_line: usize,
    end_line: usize,
    kind: DraftKind,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DraftKind {
    Requirement,
}

#[derive(Debug, Clone, Default)]
struct BlockContext {
    part: Option<String>,
    section: Option<String>,
    heading: Option<String>,
    http_status_list: bool,
    http_status_scenario: bool,
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

/// Извлекает требования из таблиц без обращения к модели.
pub fn extract_table_requirements(document: &Document) -> Vec<Requirement> {
    let mut requirements = extract_tables(document, &index_block_contexts(document)).0;
    assign_requirement_ids(&mut requirements);
    requirements
}

fn extract_tables(
    document: &Document,
    contexts: &[BlockContext],
) -> (Vec<Requirement>, Vec<ContextItem>) {
    let mut requirements = Vec::new();
    let mut items = Vec::new();
    let mut table_index = 0;
    for (block, context) in document.blocks.iter().zip(contexts) {
        let BlockContent::Table { rows, row_lines } = &block.content else {
            continue;
        };
        table_index += 1;
        if rows.len() < 2 {
            continue;
        }
        let headers = &rows[0];
        let passport = is_api_passport(headers, &rows[1..]);
        let kind = if passport {
            Some(ContextKind::ApiPassport)
        } else if is_api_parameter_table(context, headers) {
            Some(ContextKind::ApiParameter)
        } else if is_non_requirement_table(context, headers, &rows[1..]) {
            continue;
        } else {
            None
        };
        let row_location = |index: usize| {
            row_lines
                .get(index)
                .map_or(block.location, |&line| SourceSpan {
                    start_line: line,
                    end_line: line,
                    page: block.location.page,
                })
        };
        let table_ref = |row| TableRef {
            block_id: block.id,
            index: table_index,
            row,
            title: context.heading.clone(),
        };
        let mut descriptions = Vec::new();
        for (index, row) in rows.iter().enumerate().skip(1) {
            if row.iter().all(|cell| cell.trim().is_empty()) {
                continue;
            }
            let quote = format_row_quote(row);
            let location = row_location(index);
            if passport
                && row.first().is_some_and(|cell| {
                    matches!(
                        normalize_heading_title(cell).as_str(),
                        "description" | "описание"
                    )
                })
                && row.get(1).is_some_and(|cell| {
                    !cell.trim().is_empty() && !matches!(cell.trim(), "—" | "-")
                })
            {
                descriptions.push(index);
            } else if let Some(kind) = kind {
                items.push(context_item(
                    kind,
                    quote,
                    location,
                    context,
                    Some(table_ref(index)),
                ));
            } else {
                let source_id = row
                    .first()
                    .and_then(|cell| extract_source_id(cell))
                    .or_else(|| extract_source_id(&quote));
                requirements.push(requirement(
                    format_row_text(headers, row),
                    quote,
                    location,
                    source_id,
                    context,
                    Some(table_ref(index)),
                ));
            }
        }
        if let (Some(&first), Some(&last)) = (descriptions.first(), descriptions.last()) {
            let text = descriptions
                .iter()
                .map(|&index| rows[index][1].trim())
                .collect::<Vec<_>>()
                .join("\n");
            let quote = rows[first..=last]
                .iter()
                .map(|row| format_row_quote(row))
                .filter(|row| !row.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let location = SourceSpan {
                end_line: row_location(last).end_line,
                ..row_location(first)
            };
            requirements.push(requirement(
                text,
                quote,
                location,
                None,
                context,
                Some(table_ref(first)),
            ));
        }
    }
    (requirements, items)
}

fn requirement(
    text: String,
    quote: String,
    location: SourceSpan,
    source_id: Option<String>,
    context: &BlockContext,
    table: Option<TableRef>,
) -> Requirement {
    Requirement {
        id: String::new(),
        source_id,
        text,
        quote,
        location,
        table,
        part: context.part.clone(),
        section: context.section.clone(),
    }
}

fn context_item(
    kind: ContextKind,
    quote: String,
    location: SourceSpan,
    context: &BlockContext,
    table: Option<TableRef>,
) -> ContextItem {
    ContextItem {
        id: String::new(),
        kind,
        quote,
        location,
        table,
        part: context.part.clone(),
        section: context.section.clone(),
        heading: context.heading.clone(),
    }
}

pub(crate) fn segment_blocks(document: &Document) -> Vec<&Block> {
    document
        .blocks
        .iter()
        .zip(index_block_contexts(document))
        .filter(|(block, context)| {
            !matches!(block.content, BlockContent::Table { .. })
                && api_context_kind(block, context).is_none()
        })
        .map(|(block, _)| block)
        .collect()
}

pub(crate) fn merge_requirements(
    document: &Document,
    drafts: Vec<RequirementDraft>,
) -> (Vec<Requirement>, Vec<ContextItem>) {
    let contexts = index_block_contexts(document);
    let (mut requirements, mut items) = extract_tables(document, &contexts);
    let mut seen_spans = HashSet::new();
    let mut heading_ids = HashSet::new();
    for (block, context) in document.blocks.iter().zip(&contexts) {
        if let Some(kind) = api_context_kind(block, context)
            && let Some(quote) = block_plain_text(&block.content)
            && !quote.trim().is_empty()
        {
            items.push(context_item(
                kind,
                quote.trim().to_owned(),
                block.location,
                context,
                None,
            ));
            continue;
        }
        let (BlockContent::Paragraph { text } | BlockContent::ListItem { text }) = &block.content
        else {
            continue;
        };
        let explicit_id = extract_source_id(text);
        let heading_id = context.heading.as_deref().and_then(extract_source_id);
        let full_span = block.location;
        let is_method_rule = matches!(block.content, BlockContent::ListItem { .. })
            && context.heading.as_deref().is_some_and(|heading| {
                matches!(
                    normalize_heading_title(heading).as_str(),
                    "логика работы метода" | "особые правила" | "request body" | "тело запроса"
                )
            });
        // PDF может склеить физические строки, сохранив диапазон всего абзаца.
        let keep_whole_block = heading_id.is_some()
            || text.lines().count() != full_span.end_line - full_span.start_line + 1;
        let spans = if explicit_id.is_some() || is_method_rule {
            vec![full_span]
        } else {
            drafts
                .iter()
                .filter_map(|draft| {
                    (matches!(draft.kind, DraftKind::Requirement)
                        && block.id == draft.block_id
                        && draft.start_line >= full_span.start_line
                        && draft.start_line <= draft.end_line
                        && draft.end_line <= full_span.end_line)
                        .then_some(if keep_whole_block {
                            full_span
                        } else {
                            SourceSpan {
                                start_line: draft.start_line,
                                end_line: draft.end_line,
                                ..full_span
                            }
                        })
                })
                .collect()
        };
        for span in spans {
            if !seen_spans.insert((block.id.0, span.start_line, span.end_line)) {
                continue;
            }
            let quote = if span == full_span {
                text.trim().to_owned()
            } else {
                text.lines()
                    .skip(span.start_line - full_span.start_line)
                    .take(span.end_line - span.start_line + 1)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_owned()
            };
            if quote.is_empty() || quote.starts_with('{') {
                continue;
            }
            let source_id = explicit_id
                .clone()
                .or_else(|| extract_source_id(&quote))
                .or_else(|| {
                    heading_id.clone().filter(|id| {
                        heading_ids.insert((full_span.page, context.heading.clone(), id.clone()))
                    })
                });
            requirements.push(requirement(
                clean_requirement_text(&quote),
                quote,
                span,
                source_id,
                context,
                None,
            ));
        }
    }
    assign_requirement_ids(&mut requirements);
    items.sort_by_key(|item| location_key(item.location));
    for (index, item) in items.iter_mut().enumerate() {
        item.id = format!("C-{:03}", index + 1);
    }
    (requirements, items)
}

fn location_key(span: SourceSpan) -> (Option<usize>, usize, usize) {
    (span.page, span.start_line, span.end_line)
}

fn assign_requirement_ids(requirements: &mut [Requirement]) {
    requirements.sort_by_key(|item| location_key(item.location));
    for (index, item) in requirements.iter_mut().enumerate() {
        item.id = format!("R-{:03}", index + 1);
    }
}

fn clean_requirement_text(quote: &str) -> String {
    let text = strip_list_bullet(quote).unwrap_or(quote).trim_start();
    let text = strip_leading_numbering(text).unwrap_or(text).trim_start();
    let Some(id) = extract_source_id(text) else {
        return text.to_owned();
    };
    for (offset, _) in text.char_indices() {
        if let Some((candidate, rest)) = scan_id_token(&text[offset..])
            && candidate == id
        {
            return rest
                .trim_start_matches(|ch: char| {
                    ch.is_whitespace()
                        || matches!(
                            ch,
                            '.' | ':' | ')' | ']' | '-' | '–' | '—' | '*' | '`' | '_'
                        )
                })
                .to_owned();
        }
    }
    text.to_owned()
}

fn api_context_kind(block: &Block, context: &BlockContext) -> Option<ContextKind> {
    match &block.content {
        BlockContent::Heading { text, .. }
            if starts_with_http_status(text, context.http_status_list) =>
        {
            Some(ContextKind::StatusHeading)
        }
        BlockContent::Code { language, text } => {
            let is_json = language
                .as_deref()
                .is_some_and(|lang| lang.eq_ignore_ascii_case("json"))
                || matches!(text.trim_start().chars().next(), Some('{' | '['));
            Some(if is_json || context.http_status_scenario {
                ContextKind::StatusExample
            } else {
                ContextKind::CodeExample
            })
        }
        BlockContent::Paragraph { text } | BlockContent::ListItem { text }
            if context.http_status_scenario
                || (context.http_status_list && starts_with_http_status(text, true)) =>
        {
            Some(ContextKind::StatusExample)
        }
        _ => None,
    }
}

fn starts_with_http_status(text: &str, in_status_list: bool) -> bool {
    let text = text.trim();
    let prefixed = strip_case_insensitive_prefix(text, "HTTP ");
    let text = prefixed.unwrap_or(text);
    let Some((code, suffix)) = text.get(..3).zip(text.get(3..)) else {
        return false;
    };
    if !code
        .parse::<u16>()
        .is_ok_and(|code| (100..600).contains(&code))
    {
        return false;
    }
    let whitespace_boundary = in_status_list || prefixed.is_some();
    match suffix.chars().next() {
        None | Some('.' | ':' | '–' | '—') => {
            !suffix.chars().nth(1).is_some_and(|ch| ch.is_ascii_digit())
        }
        Some(ch) => whitespace_boundary && ch.is_whitespace(),
    }
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

    let mut http_status_list = false;
    let mut http_status_scenario = false;
    for heading in &section_parts {
        http_status_list |= matches!(
            normalize_heading_title(heading).as_str(),
            "статусы"
                | "коды статусов"
                | "коды ответа"
                | "коды ответов"
                | "http-статусы"
                | "status codes"
                | "http status codes"
        );
        http_status_scenario |= starts_with_http_status(heading, http_status_list);
    }

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
        http_status_list,
        http_status_scenario,
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

fn is_api_passport(headers: &[String], rows: &[Vec<String>]) -> bool {
    is_key_value_headers(headers)
        && rows.iter().filter_map(|row| row.first()).any(|key| {
            matches!(
                normalize_heading_title(key).as_str(),
                "author"
                    | "developer"
                    | "swagger"
                    | "gitlab project"
                    | "framework"
                    | "url type"
                    | "controller"
                    | "rest type"
                    | "request url"
            )
        })
}

fn is_api_parameter_table(context: &BlockContext, headers: &[String]) -> bool {
    let heading = context
        .section
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let parameter_section = [
        "параметры запроса",
        "параметры ответа",
        "query-параметры",
        "path-параметры",
        "request parameters",
        "response parameters",
        "request body",
    ]
    .iter()
    .any(|label| heading.contains(label));
    let parameter_headers = headers.iter().any(|header| {
        matches!(
            header.trim().to_lowercase().as_str(),
            "parameter" | "параметр" | "поле" | "field"
        )
    }) && headers.iter().any(|header| {
        matches!(
            header.trim().to_lowercase().as_str(),
            "format" | "формат" | "тип" | "type"
        )
    });
    parameter_section || parameter_headers
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
            | "parameter"
            | "наименование"
            | "реквизит"
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

fn block_plain_text(content: &BlockContent) -> Option<&str> {
    match content {
        BlockContent::Heading { text, .. }
        | BlockContent::Paragraph { text }
        | BlockContent::ListItem { text }
        | BlockContent::Code { text, .. } => Some(text),
        BlockContent::Table { .. } => None,
    }
}
