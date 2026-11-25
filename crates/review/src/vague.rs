use std::ops::Range;

use async_trait::async_trait;
use doc::{BlockContent, SourceSpan};

use crate::{
    Check, CheckPhase, Criterion, Detector, Evidence, Finding, RunContext, Scope, Severity,
    StageResult, rules::is_review_checklist_table,
};

const BUILTIN_DICTIONARY: &str = include_str!("../../../dictionaries/ru-vague-terms.txt");
const MAX_STEM_SUFFIX_CHARS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VagueTermEntry {
    pub term: String,
    pub category: String,
    pub forms: Vec<String>,
    pub why_bad: String,
    pub rewrite: String,
}

#[derive(Debug, Clone)]
enum PatternToken {
    Word(String),
    Stem(String),
    Punct(char),
}

#[derive(Debug, Clone)]
struct CompiledPattern {
    entry_index: usize,
    tokens: Vec<PatternToken>,
}

#[derive(Debug, Clone)]
pub struct VagueDictionary {
    entries: Vec<VagueTermEntry>,
    patterns: Vec<CompiledPattern>,
    exceptions: Vec<Vec<PatternToken>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VagueMatch<'a> {
    pub entry: &'a VagueTermEntry,
    pub matched_text: String,
    pub byte_range: Range<usize>,
}

impl VagueDictionary {
    pub fn builtin() -> Self {
        Self::parse(BUILTIN_DICTIONARY).expect("встроенный словарь размытых формулировок корректен")
    }

    pub fn parse(source: &str) -> Result<Self, String> {
        let mut entries = Vec::new();
        let mut patterns = Vec::new();
        let mut exceptions = Vec::new();

        for (line_idx, raw_line) in source.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("!исключение") {
                let phrase = rest
                    .trim_start()
                    .strip_prefix('|')
                    .ok_or_else(|| {
                        format!(
                            "Строка {}: ожидается формат «!исключение | фраза»",
                            line_idx + 1
                        )
                    })?
                    .trim();
                let tokens = compile_pattern(phrase);
                if tokens.is_empty() {
                    return Err(format!("Строка {}: пустая фраза-исключение", line_idx + 1));
                }
                exceptions.push(tokens);
                continue;
            }

            let parts: Vec<&str> = line.split('|').map(str::trim).collect();
            if parts.len() != 5 {
                return Err(format!(
                    "Строка {}: ожидается 5 столбцов через «|», получено {}",
                    line_idx + 1,
                    parts.len()
                ));
            }
            let term = parts[0].to_string();
            let category = parts[1].to_string();
            let forms: Vec<String> = if parts[2].is_empty() {
                vec![term.clone()]
            } else {
                parts[2]
                    .split(',')
                    .map(str::trim)
                    .filter(|form| !form.is_empty())
                    .map(str::to_string)
                    .collect()
            };
            let why_bad = parts[3].to_string();
            let rewrite = parts[4].to_string();

            if term.is_empty() || category.is_empty() || why_bad.is_empty() || rewrite.is_empty() {
                return Err(format!(
                    "Строка {}: обязательные поля словаря не должны быть пустыми",
                    line_idx + 1
                ));
            }

            let entry_index = entries.len();
            for form in &forms {
                let tokens = compile_pattern(form);
                if !tokens.is_empty() {
                    patterns.push(CompiledPattern {
                        entry_index,
                        tokens,
                    });
                }
            }
            entries.push(VagueTermEntry {
                term,
                category,
                forms,
                why_bad,
                rewrite,
            });
        }

        // Более длинные фразы проверяем первыми, чтобы «в течение разумного времени»
        // не перекрывалось более короткими фрагментами.
        patterns.sort_by_key(|pattern| std::cmp::Reverse(pattern.tokens.len()));

        Ok(Self {
            entries,
            patterns,
            exceptions,
        })
    }

    pub fn entries(&self) -> &[VagueTermEntry] {
        &self.entries
    }

    pub fn find_matches<'a>(&'a self, text: &str) -> Vec<VagueMatch<'a>> {
        let tokens = tokenize_text(text);
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut excluded = quoted_and_code_ranges(text);
        for exception in &self.exceptions {
            for range in find_pattern_ranges(text, &tokens, exception) {
                excluded.push(range);
            }
        }

        let mut occupied: Vec<Range<usize>> = Vec::new();
        let mut matches = Vec::new();

        for pattern in &self.patterns {
            for range in find_pattern_ranges(text, &tokens, &pattern.tokens) {
                if overlaps_any(&range, &excluded) || overlaps_any(&range, &occupied) {
                    continue;
                }
                occupied.push(range.clone());
                matches.push(VagueMatch {
                    entry: &self.entries[pattern.entry_index],
                    matched_text: text[range.clone()].to_string(),
                    byte_range: range,
                });
            }
        }

        matches.sort_by_key(|item| item.byte_range.start);
        matches
    }
}

#[derive(Debug)]
struct TextToken {
    start: usize,
    end: usize,
    normalized: String,
    is_word: bool,
}

fn normalize_word(raw: &str) -> String {
    raw.to_lowercase().replace('ё', "е")
}

fn tokenize_text(text: &str) -> Vec<TextToken> {
    let mut tokens = Vec::new();
    let mut iter = text.char_indices().peekable();
    while let Some((start, ch)) = iter.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_alphanumeric() {
            let mut end = start + ch.len_utf8();
            while let Some(&(next_idx, next_ch)) = iter.peek() {
                if next_ch.is_alphanumeric() {
                    end = next_idx + next_ch.len_utf8();
                    iter.next();
                } else {
                    break;
                }
            }
            tokens.push(TextToken {
                start,
                end,
                normalized: normalize_word(&text[start..end]),
                is_word: true,
            });
        } else {
            tokens.push(TextToken {
                start,
                end: start + ch.len_utf8(),
                normalized: ch.to_string(),
                is_word: false,
            });
        }
    }
    tokens
}

fn compile_pattern(pattern: &str) -> Vec<PatternToken> {
    let mut tokens = Vec::new();
    let mut iter = pattern.char_indices().peekable();
    while let Some((start, ch)) = iter.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_alphanumeric() {
            let mut end = start + ch.len_utf8();
            while let Some(&(next_idx, next_ch)) = iter.peek() {
                if next_ch.is_alphanumeric() {
                    end = next_idx + next_ch.len_utf8();
                    iter.next();
                } else {
                    break;
                }
            }
            let normalized = normalize_word(&pattern[start..end]);
            if matches!(iter.peek(), Some(&(_, '*'))) {
                iter.next();
                tokens.push(PatternToken::Stem(normalized));
            } else {
                tokens.push(PatternToken::Word(normalized));
            }
        } else if ch != '*' {
            tokens.push(PatternToken::Punct(ch));
        }
    }
    tokens
}

fn token_matches(token: &TextToken, pattern: &PatternToken) -> bool {
    match pattern {
        PatternToken::Word(expected) => token.is_word && token.normalized == *expected,
        PatternToken::Stem(stem) => {
            if !token.is_word || !token.normalized.starts_with(stem) {
                return false;
            }
            let suffix = &token.normalized[stem.len()..];
            suffix.chars().count() <= MAX_STEM_SUFFIX_CHARS
                && suffix
                    .chars()
                    .all(|ch| ('\u{0400}'..='\u{04FF}').contains(&ch))
        }
        PatternToken::Punct(expected) => !token.is_word && token.normalized == expected.to_string(),
    }
}

fn find_pattern_ranges(
    source: &str,
    tokens: &[TextToken],
    pattern: &[PatternToken],
) -> Vec<Range<usize>> {
    if pattern.is_empty() || tokens.len() < pattern.len() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    for start_idx in 0..=tokens.len() - pattern.len() {
        let matched = pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| token_matches(&tokens[start_idx + offset], pat));
        if !matched {
            continue;
        }
        let start_byte = tokens[start_idx].start;
        let end_byte = tokens[start_idx + pattern.len() - 1].end;

        // Граница справа для аббревиатур с точкой (например, «и т.д.»): за точкой не должно идти слово без пробела.
        if matches!(pattern.last(), Some(PatternToken::Punct('.')))
            && source[end_byte..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        // Граница слева для символов пунктуации/подчёркивания: исключаем идентификаторы вида foo_bar.
        if source[..start_byte]
            .chars()
            .next_back()
            .is_some_and(|ch| ch == '_')
            || source[end_byte..]
                .chars()
                .next()
                .is_some_and(|ch| ch == '_')
        {
            continue;
        }
        ranges.push(start_byte..end_byte);
    }
    ranges
}

fn quoted_and_code_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    // Парные кавычки с разными открывающим и закрывающим символами.
    for (open, close) in [('«', '»'), ('“', '”'), ('„', '”'), ('„', '“')] {
        let mut open_pos = None;
        for (idx, ch) in text.char_indices() {
            if ch == open && open_pos.is_none() {
                open_pos = Some(idx);
            } else if ch == close
                && let Some(start) = open_pos.take()
            {
                ranges.push(start..idx + ch.len_utf8());
            }
        }
    }
    // Симметричные разделители: двойные кавычки и обратные кавычки inline-кода.
    for delim in ['"', '`'] {
        let mut open_pos = None;
        for (idx, ch) in text.char_indices() {
            if ch == delim {
                if let Some(start) = open_pos.take() {
                    ranges.push(start..idx + ch.len_utf8());
                } else {
                    open_pos = Some(idx);
                }
            }
        }
    }
    ranges
}

fn overlaps_any(range: &Range<usize>, excluded: &[Range<usize>]) -> bool {
    excluded
        .iter()
        .any(|other| range.start < other.end && other.start < range.end)
}

fn extract_quote_and_span(
    text: &str,
    match_range: &Range<usize>,
    block_location: SourceSpan,
) -> (String, SourceSpan) {
    let lines: Vec<&str> = text.lines().collect();
    let exact_line_mapping =
        !lines.is_empty() && lines.len() == block_location.end_line - block_location.start_line + 1;

    // Выбираем строку, содержащую совпадение.
    let mut offset = 0;
    let mut line_index = 0;
    let mut line_slice = text;
    let mut line_start_offset = 0;
    for (idx, raw_line) in text.split('\n').enumerate() {
        let line_len = raw_line.len();
        let line_end = offset + line_len;
        if match_range.start >= offset && match_range.start <= line_end {
            line_index = idx;
            line_slice = raw_line.trim_end_matches('\r');
            line_start_offset = offset;
            break;
        }
        offset = line_end + 1;
    }

    let rel_start = match_range.start.saturating_sub(line_start_offset);
    let rel_end = match_range
        .end
        .saturating_sub(line_start_offset)
        .min(line_slice.len());

    // Пытаемся сузить цитату до предложения внутри строки, сохраняя дословный фрагмент.
    let quote = extract_sentence(line_slice, rel_start, rel_end);
    let span = if exact_line_mapping {
        let line = block_location.start_line + line_index;
        SourceSpan {
            start_line: line,
            end_line: line,
            page: block_location.page,
        }
    } else {
        block_location
    };
    (quote.trim().to_string(), span)
}

fn extract_sentence(line: &str, rel_start: usize, rel_end: usize) -> &str {
    let mut sent_start = 0;
    let bytes = line.as_bytes();
    for (idx, ch) in line.char_indices() {
        if idx >= rel_start {
            break;
        }
        if matches!(ch, '.' | '!' | '?') {
            let next_idx = idx + ch.len_utf8();
            if bytes.get(next_idx).is_some_and(|b| b.is_ascii_whitespace()) {
                // Не разрываем после маркера вида «Ф-1.» или аббревиатуры «т.д.»
                let before = line[..idx].trim_end();
                let is_abbrev =
                    before.ends_with(" т") || before.ends_with(" др") || before.len() <= 4;
                if !is_abbrev {
                    sent_start = next_idx;
                }
            }
        }
    }
    let mut sent_end = line.len();
    for (idx, ch) in line.char_indices() {
        if idx < rel_end {
            continue;
        }
        if matches!(ch, '.' | '!' | '?') {
            let next_idx = idx + ch.len_utf8();
            if next_idx == line.len()
                || bytes.get(next_idx).is_some_and(|b| b.is_ascii_whitespace())
            {
                sent_end = next_idx;
                break;
            }
        }
    }
    &line[sent_start..sent_end]
}

fn scope_for_span(ctx: &RunContext<'_>, span: SourceSpan) -> Scope {
    ctx.requirements
        .iter()
        .find(|req| {
            req.location.page == span.page
                && req.location.start_line <= span.end_line
                && span.start_line <= req.location.end_line
        })
        .map_or(Scope::Set, |req| Scope::Requirement(req.id.clone()))
}

pub(crate) struct VagueTermsCheck {
    dictionary: VagueDictionary,
}

impl Default for VagueTermsCheck {
    fn default() -> Self {
        Self {
            dictionary: VagueDictionary::builtin(),
        }
    }
}

#[async_trait]
impl Check for VagueTermsCheck {
    fn phase(&self) -> CheckPhase {
        CheckPhase::Rules
    }

    async fn run(&self, ctx: &RunContext<'_>) -> StageResult<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut counter = 0usize;

        for block in &ctx.document.blocks {
            match &block.content {
                // Заголовки и блоки кода исключаются по контексту.
                BlockContent::Heading { .. } | BlockContent::Code { .. } => {}
                BlockContent::Paragraph { text } | BlockContent::ListItem { text } => {
                    for found in self.dictionary.find_matches(text) {
                        let (quote, location) =
                            extract_quote_and_span(text, &found.byte_range, block.location);
                        if quote.is_empty() {
                            continue;
                        }
                        counter += 1;
                        findings.push(vague_finding(
                            format!("vague-term-{counter:03}"),
                            scope_for_span(ctx, location),
                            Evidence { quote, location },
                            &found,
                        ));
                    }
                }
                BlockContent::Table { rows, row_lines } => {
                    if is_review_checklist_table(rows) {
                        continue;
                    }
                    // Пропускаем строку заголовков таблицы (row 0).
                    for (row_idx, row) in rows.iter().enumerate().skip(1) {
                        let location =
                            row_lines
                                .get(row_idx)
                                .map_or(block.location, |&line| SourceSpan {
                                    start_line: line,
                                    end_line: line,
                                    page: block.location.page,
                                });
                        for cell in row {
                            for found in self.dictionary.find_matches(cell) {
                                let quote = cell.trim().to_string();
                                if quote.is_empty() {
                                    continue;
                                }
                                counter += 1;
                                findings.push(vague_finding(
                                    format!("vague-term-{counter:03}"),
                                    scope_for_span(ctx, location),
                                    Evidence { quote, location },
                                    &found,
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(findings)
    }
}

fn vague_finding(id: String, scope: Scope, evidence: Evidence, found: &VagueMatch<'_>) -> Finding {
    let category = found.entry.category.replace('_', " ");
    let mut rewrite = found.entry.rewrite.clone();
    if !rewrite.trim_end().ends_with(['.', '!', '?']) {
        rewrite.push('.');
    }
    Finding {
        id,
        scope,
        criterion: Criterion::Unambiguous,
        additional_criteria: Vec::new(),
        severity: Severity::Medium,
        evidence: vec![evidence],
        explanation: format!(
            "Обнаружена размытая формулировка «{}» (словарь: «{}», категория «{}»): {}.",
            found.matched_text, found.entry.term, category, found.entry.why_bad
        ),
        suggestion: Some(rewrite),
        detector: Detector::Rule("vague-terms".into()),
        additional_detectors: Vec::new(),
        merged_ids: Vec::new(),
        fallback_evidence: false,
        verified: false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use doc::ParserRegistry;
    use llm::MockProvider;

    use super::*;
    use crate::CheckRegistry;

    #[test]
    fn builtin_dictionary_loads_all_core_examples() {
        let dict = VagueDictionary::builtin();
        assert!(dict.entries().len() >= 25);
        for required in [
            "быстро",
            "удобно",
            "своевременно",
            "при необходимости",
            "по возможности",
            "и т.д.",
            "и т.п.",
            "как правило",
            "максимально",
            "оптимально",
            "достаточно",
            "в течение разумного времени",
            "и/или",
        ] {
            assert!(
                dict.entries().iter().any(|entry| entry.term == required),
                "Отсутствует базовый термин «{required}»"
            );
        }
    }

    #[test]
    fn matches_word_forms_and_multiword_phrases() {
        let dict = VagueDictionary::builtin();
        let text = concat!(
            "Система должна обрабатывать заявки быстрее конкурентов и предоставлять удобный интерфейс. ",
            "Уведомления отправляются своевременным способом в течение разумного времени. ",
            "Поддерживаются карты, счета и т.п., а также переводы и/или платежи."
        );
        let found: Vec<&str> = dict
            .find_matches(text)
            .into_iter()
            .map(|m| m.entry.term.as_str())
            .collect();
        assert!(found.contains(&"быстро"), "{found:?}");
        assert!(found.contains(&"удобно"), "{found:?}");
        assert!(found.contains(&"своевременно"), "{found:?}");
        assert!(found.contains(&"в течение разумного времени"), "{found:?}");
        assert!(found.contains(&"и т.п."), "{found:?}");
        assert!(found.contains(&"и/или"), "{found:?}");
    }

    #[test]
    fn respects_word_boundaries() {
        let dict = VagueDictionary::builtin();
        // «недостаточно» не должно срабатывать на «достаточно»,
        // «быстродействие» не должно срабатывать на «быстро»,
        // «максимальная сумма» не должна срабатывать на «максимально».
        let text = concat!(
            "Если на счёте недостаточно средств, операция отклоняется. ",
            "Требования к быстродействию заданы в разделе SLA: p95 <= 200 мс. ",
            "Максимальная сумма перевода составляет 500 000 ₸."
        );
        let found = dict.find_matches(text);
        assert!(found.is_empty(), "Ложные срабатывания: {found:?}");
    }

    #[test]
    fn respects_context_exceptions_quotes_code_and_explicit_phrases() {
        let dict = VagueDictionary::builtin();
        let text = concat!(
            "Кнопка «Быстрый платёж» открывает экран перевода через Систему быстрых платежей. ",
            "Сообщение \"Удобный вход включён\" отображается один раз. ",
            "Поле `quick_mode` передаётся в JSON. ",
            "Если на счёте достаточно средств или достаточный остаток, операция проводится. ",
            "При списании средств на счёте может быть не достаточно баланса."
        );
        let found = dict.find_matches(text);
        assert!(
            found.is_empty(),
            "Ожидались исключения по контексту: {found:?}"
        );

        let vague = dict.find_matches("Сервис должен работать достаточно стабильно.");
        assert_eq!(vague.len(), 1);
        assert_eq!(vague[0].entry.term, "достаточно");
    }

    #[test]
    fn convenience_rationale_is_exempt_but_ux_requirements_still_match() {
        let dict = VagueDictionary::builtin();
        let rationale = "NFR-4. Для удобства отладки полные номера карт и счетов клиентов записываются в журнал приложения в открытом виде.";
        assert!(dict.find_matches(rationale).is_empty());
        let text = format!("{rationale} Интерфейс должен быть удобным.");
        let matches = dict.find_matches(&text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "удобным");
    }

    #[tokio::test]
    async fn vague_terms_rule_emits_verified_finding_with_term_quote_and_rewrite() {
        let source = concat!(
            "# Требования к модулю отчётов\n\n",
            "## Быстрый старт\n\n",
            "```json\n{\"hint\": \"быстро и удобно\"}\n```\n\n",
            "* **FR-1.** Отчёт формируется быстро и при необходимости выгружается в CSV и т.д.\n"
        );
        let document = ParserRegistry::default()
            .parse(Path::new("spec.md"), source.as_bytes())
            .unwrap();
        let mut registry = CheckRegistry::default();
        registry.register(VagueTermsCheck::default());
        let ctx = RunContext::new(document, &MockProvider, &registry);
        let findings = registry.run(CheckPhase::Rules, &ctx).await.unwrap();

        assert_eq!(findings.len(), 3, "{findings:#?}");
        for finding in &findings {
            assert_eq!(finding.criterion, Criterion::Unambiguous);
            assert!(matches!(finding.detector, Detector::Rule(ref id) if id == "vague-terms"));
            assert!(finding.suggestion.is_some());
            assert!(
                finding
                    .evidence
                    .iter()
                    .all(|ev| { ctx.document.contains_quote(ev.location, &ev.quote) })
            );
        }
        assert!(findings.iter().any(|f| f.explanation.contains("быстро")));
        assert!(
            findings
                .iter()
                .any(|f| f.explanation.contains("при необходимости"))
        );
        assert!(findings.iter().any(|f| f.explanation.contains("и т.д.")));
    }
}
