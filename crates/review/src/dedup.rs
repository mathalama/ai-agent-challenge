use std::collections::HashSet;

use crate::{Criterion, Detector, Evidence, Finding, Requirement, Scope};

pub fn merge_findings(findings: Vec<Finding>) -> Vec<Finding> {
    merge_findings_in_context(findings, &[])
}

pub(crate) fn merge_findings_in_context(
    findings: Vec<Finding>,
    requirements: &[Requirement],
) -> Vec<Finding> {
    let mut merged: Vec<Finding> = Vec::with_capacity(findings.len());
    for mut candidate in findings {
        if let Some(target) = merged
            .iter_mut()
            .find(|existing| should_merge(existing, &candidate, requirements))
        {
            if candidate.verified && !target.verified {
                std::mem::swap(target, &mut candidate);
            }
            merge_into(target, candidate);
        } else {
            merged.push(candidate);
        }
    }
    merged
}

pub(crate) fn narrow_set_scope(finding: &mut Finding, requirements: &[Requirement]) {
    if finding.scope == Scope::Set
        && !finding.criterion.is_set_level()
        && let Some(owner) = evidence_owner(finding, requirements)
    {
        finding.scope = Scope::Requirement(owner.id.clone());
    }
}

fn evidence_owner<'a>(
    finding: &Finding,
    requirements: &'a [Requirement],
) -> Option<&'a Requirement> {
    if finding.fallback_evidence || finding.evidence.is_empty() {
        return None;
    }
    let mut owners = requirements.iter().filter(|requirement| {
        finding.evidence.iter().all(|evidence| {
            let span = evidence.location;
            let owner = requirement.location;
            owner.page == span.page
                && owner.start_line <= span.start_line
                && span.start_line <= span.end_line
                && span.end_line <= owner.end_line
        })
    });
    let owner = owners.next()?;
    owners.next().is_none().then_some(owner)
}

fn same_scope(left: &Finding, right: &Finding, requirements: &[Requirement]) -> bool {
    if left.scope == right.scope {
        return true;
    }
    let (set, single) = if left.scope == Scope::Set {
        (left, right)
    } else {
        (right, left)
    };
    let Scope::Requirement(id) = &single.scope else {
        return false;
    };
    // Одно упущение может быть одновременно дефектом полноты набора и требования.
    set.scope == Scope::Set
        && set.all_criteria().contains(&Criterion::SetComplete)
        && single.criterion == Criterion::Complete
        && set.verified
        && single.verified
        && evidence_owner(set, requirements).is_some_and(|owner| owner.id == *id)
}

fn should_merge(left: &Finding, right: &Finding, requirements: &[Requirement]) -> bool {
    if !same_scope(left, right, requirements) {
        return false;
    }
    let mixed_verification = left.verified != right.verified;
    let paired_conflict = mixed_verification && is_paired_conflict(left, right);
    if mixed_verification && !can_absorb_unverified(left, right, paired_conflict) {
        return false;
    }
    let quotes_match = |a: &Evidence, b: &Evidence| {
        spans_overlap(a, b)
            && if mixed_verification {
                numeric_values(&a.quote).eq(numeric_values(&b.quote))
                    && (similar_sentences(&a.quote, &b.quote)
                        || (paired_conflict && texts_overlap(&a.quote, &b.quote)))
            } else if left.scope != right.scope {
                normalize_text(&a.quote) == normalize_text(&b.quote)
            } else {
                texts_overlap(&a.quote, &b.quote)
            }
    };
    if !quotes_overlap(&left.evidence, &right.evidence, quotes_match) {
        return false;
    }
    if (mixed_verification || left.scope == Scope::Set || right.scope == Scope::Set)
        && (!covers_evidence(&left.evidence, &right.evidence, quotes_match)
            || !covers_evidence(&right.evidence, &left.evidence, quotes_match))
    {
        return false;
    }

    let left_stages = finding_stages(left);
    let right_stages = finding_stages(right);
    let same_criteria = shares_criterion(left, right);

    // Разные детерминированные правила проверяют независимые инварианты документа —
    // их нельзя сливать друг с другом даже при совпадении цитаты (например, заголовка H1).
    if is_pure_rule(left) && is_pure_rule(right) && left_stages.is_disjoint(&right_stages) {
        return false;
    }

    // На уровне документа находки с запасной якорной цитатой (например, заголовок H1)
    // сливаем только при совпадении критерия.
    if matches!(left.scope, Scope::Set)
        && (left.fallback_evidence || right.fallback_evidence)
        && !same_criteria
    {
        return false;
    }

    if left_stages.is_disjoint(&right_stages) {
        return true;
    }
    // Внутри одной стадии сливаем только точные дубликаты по одному и тому же критерию.
    same_criteria
}

fn is_paired_conflict(left: &Finding, right: &Finding) -> bool {
    let separate_spans = |finding: &Finding| {
        finding
            .evidence
            .split_first()
            .is_some_and(|(first, rest)| rest.iter().any(|other| !spans_overlap(first, other)))
    };
    left.scope == Scope::Set
        && matches!(
            (left.criterion, right.criterion),
            (
                Criterion::Consistent,
                Criterion::Consistent | Criterion::Unambiguous
            ) | (Criterion::Unambiguous, Criterion::Consistent)
        )
        && separate_spans(left)
        && separate_spans(right)
}

fn can_absorb_unverified(left: &Finding, right: &Finding, paired_conflict: bool) -> bool {
    let (verified, pending) = if left.verified {
        (left, right)
    } else {
        (right, left)
    };
    pending.severity <= verified.severity
        && (paired_conflict
            || (shares_criterion(left, right)
                && similar_sentences(&verified.explanation, &pending.explanation)))
}

fn shares_criterion(left: &Finding, right: &Finding) -> bool {
    let left_criteria = left.all_criteria();
    right
        .all_criteria()
        .into_iter()
        .any(|criterion| left_criteria.contains(&criterion))
}

fn is_pure_rule(finding: &Finding) -> bool {
    finding
        .all_detectors()
        .into_iter()
        .all(|detector| matches!(detector, Detector::Rule(_)))
}

fn finding_stages(finding: &Finding) -> HashSet<String> {
    let mut stages = HashSet::new();
    for detector in finding.all_detectors() {
        stages.insert(detector_stage_key(detector, &finding.id));
    }
    stages
}

fn detector_stage_key(detector: &Detector, finding_id: &str) -> String {
    match detector {
        Detector::Rule(id) => format!("rule:{id}"),
        Detector::Llm(source) => {
            if let Some((stage, _)) = source.split_once(':') {
                format!("llm:{stage}")
            } else if let Some((prefix, _)) = finding_id.rsplit_once('-') {
                format!("llm:{prefix}")
            } else {
                format!("llm:{source}")
            }
        }
        Detector::Solver => "solver".into(),
    }
}

fn quotes_overlap(
    left: &[Evidence],
    right: &[Evidence],
    matches: impl Fn(&Evidence, &Evidence) -> bool,
) -> bool {
    left.iter().any(|a| right.iter().any(|b| matches(a, b)))
}

fn covers_evidence(
    left: &[Evidence],
    right: &[Evidence],
    matches: impl Fn(&Evidence, &Evidence) -> bool,
) -> bool {
    left.iter().all(|a| right.iter().any(|b| matches(a, b)))
}

fn spans_overlap(left: &Evidence, right: &Evidence) -> bool {
    left.location.page == right.location.page
        && left.location.start_line <= right.location.end_line
        && right.location.start_line <= left.location.end_line
}

pub(crate) fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['«', '»', '“', '”', '„', '‟', '‹', '›'], "\"")
        .replace(['‘', '’', '‚', '‛'], "'")
        .to_lowercase()
        .replace('ё', "е")
}

fn texts_overlap(left: &str, right: &str) -> bool {
    let a = normalize_text(left);
    let b = normalize_text(right);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let shorter_alnum = a
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .count()
        .min(b.chars().filter(|ch| ch.is_alphanumeric()).count());
    if shorter_alnum >= 4 && (a.contains(&b) || b.contains(&a)) {
        return true;
    }

    let words_a = content_words(&a);
    let words_b = content_words(&b);
    if words_a.is_empty() || words_b.is_empty() {
        return false;
    }
    // Совпадающая последовательность из двух значимых слов подряд.
    if words_a.len() >= 2 && words_b.len() >= 2 {
        let bigrams_a: HashSet<(&str, &str)> = words_a
            .windows(2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
            .collect();
        if words_b
            .windows(2)
            .any(|pair| bigrams_a.contains(&(pair[0].as_str(), pair[1].as_str())))
        {
            return true;
        }
    }
    let set_a: HashSet<&str> = words_a.iter().map(String::as_str).collect();
    let set_b: HashSet<&str> = words_b.iter().map(String::as_str).collect();
    let shared = set_a.intersection(&set_b).count();
    let min_len = set_a.len().min(set_b.len());
    (shared >= 2 && shared * 2 >= min_len)
        || (min_len == 1
            && shared == 1
            && set_a.intersection(&set_b).any(|w| w.chars().count() >= 5))
}

fn content_words(normalized: &str) -> Vec<String> {
    normalized
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_string)
        .collect()
}

fn merge_into(target: &mut Finding, other: Finding) {
    for criterion in other.all_criteria() {
        if criterion != target.criterion && !target.additional_criteria.contains(&criterion) {
            target.additional_criteria.push(criterion);
        }
    }

    for detector in other.all_detectors() {
        if *detector != target.detector && !target.additional_detectors.contains(detector) {
            target.additional_detectors.push(detector.clone());
        }
    }

    for id in other.all_ids() {
        if id != target.id && !target.merged_ids.iter().any(|existing| existing == id) {
            target.merged_ids.push(id.to_string());
        }
    }

    if target.verified != other.verified {
        return;
    }
    if other.scope == Scope::Set {
        target.scope = Scope::Set;
    }
    target.severity = target.severity.max(other.severity);
    merge_evidence(&mut target.evidence, other.evidence);

    append_distinct_sentences(&mut target.explanation, &other.explanation);
    match (&mut target.suggestion, other.suggestion) {
        (None, Some(suggestion)) => target.suggestion = Some(suggestion),
        (Some(existing), Some(addition)) => append_distinct_sentences(existing, &addition),
        _ => {}
    }

    target.fallback_evidence = target.fallback_evidence && other.fallback_evidence;
}

fn append_distinct_sentences(target: &mut String, addition: &str) {
    for sentence in sentences(addition) {
        if sentences(target).any(|existing| similar_sentences(existing, sentence)) {
            continue;
        }
        if !target.trim().is_empty() {
            if !target.trim_end().ends_with(['.', '!', '?']) {
                target.push('.');
            }
            target.push(' ');
        }
        target.push_str(sentence);
    }
}

fn sentences(text: &str) -> impl Iterator<Item = &str> {
    let mut start = 0;
    text.match_indices(['.', '!', '?'])
        .map(|(index, punctuation)| (index + 1, punctuation))
        .chain(std::iter::once((text.len(), "")))
        .filter_map(move |(end, punctuation)| {
            let rest = &text[end..];
            if !rest.is_empty() {
                if !rest.starts_with(char::is_whitespace) {
                    return None;
                }
                let sentence = text[start..end].trim_start();
                if punctuation == "." && !ends_sentence(sentence, rest.trim_start()) {
                    return None;
                }
            }
            let sentence = text[start..end].trim();
            start = end;
            (!sentence.is_empty()).then_some(sentence)
        })
}

fn ends_sentence(sentence: &str, rest: &str) -> bool {
    let next = rest
        .trim_start_matches(['«', '“', '"', '(', '—'])
        .trim_start();
    if next
        .chars()
        .next()
        .is_some_and(|ch| ch.is_lowercase() || ch.is_numeric())
    {
        return false;
    }
    let without_dot = sentence.trim_end_matches('.');
    let bare_id = without_dot.split_once('-').is_some_and(|(prefix, suffix)| {
        !prefix.is_empty()
            && prefix.chars().all(char::is_uppercase)
            && !suffix.is_empty()
            && suffix.chars().all(|ch| ch.is_ascii_digit())
    });
    !bare_id
        && !without_dot
            .split_whitespace()
            .next_back()
            .is_some_and(|word| {
                let word = word.to_lowercase();
                matches!(
                    word.as_str(),
                    "тыс" | "млн" | "млрд" | "руб" | "см" | "рис" | "табл"
                ) || introduces_example_or_address(&word)
            })
}

fn introduces_example_or_address(word: &str) -> bool {
    matches!(word, "г" | "ул" | "напр")
}

fn abbreviation_values(text: &str) -> impl Iterator<Item = &str> {
    let words = text.split_whitespace();
    words
        .clone()
        .zip(words.skip(1))
        .filter_map(|(marker, value)| {
            marker
                .strip_suffix('.')
                .filter(|word| introduces_example_or_address(word))
                .map(|_| value.trim_matches(|ch: char| !ch.is_alphanumeric()))
        })
}

fn numeric_values(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !ch.is_numeric())
        .filter(|value| !value.is_empty())
}

fn similar_sentences(left: &str, right: &str) -> bool {
    let left = normalize_text(left);
    let right = normalize_text(right);
    if left.trim_end_matches(['.', '!', '?']) == right.trim_end_matches(['.', '!', '?']) {
        return true;
    }
    let negated = |text: &str| {
        text.split(|ch: char| !ch.is_alphanumeric())
            .any(|word| matches!(word, "не" | "без" | "нельзя" | "запрещено"))
    };
    if negated(&left) != negated(&right)
        || numeric_values(&left).ne(numeric_values(&right))
        || abbreviation_values(&left).ne(abbreviation_values(&right))
    {
        return false;
    }
    let left: HashSet<_> = content_words(&left).into_iter().collect();
    let right: HashSet<_> = content_words(&right).into_iter().collect();
    let shared = left.intersection(&right).count();
    shared >= 3 && shared * 4 >= left.len().max(right.len()) * 3
}

fn merge_evidence(target: &mut Vec<Evidence>, incoming: Vec<Evidence>) {
    for candidate in incoming {
        let cand_norm = normalize_text(&candidate.quote);
        if cand_norm.is_empty() {
            continue;
        }
        if let Some(existing) = target.iter_mut().find(|existing| {
            existing.location == candidate.location && {
                let ex_norm = normalize_text(&existing.quote);
                ex_norm == cand_norm || ex_norm.contains(&cand_norm) || cand_norm.contains(&ex_norm)
            }
        }) {
            let ex_norm = normalize_text(&existing.quote);
            if cand_norm.len() > ex_norm.len() && cand_norm.contains(&ex_norm) {
                existing.quote = candidate.quote;
            }
        } else {
            target.push(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use doc::SourceSpan;

    use super::*;
    use crate::{Scope, Severity};

    fn make_finding(
        id: &str,
        scope: Scope,
        criterion: Criterion,
        severity: Severity,
        quote: &str,
        line: usize,
        detector: Detector,
    ) -> Finding {
        Finding {
            id: id.into(),
            scope,
            criterion,
            additional_criteria: Vec::new(),
            severity,
            evidence: vec![Evidence {
                quote: quote.into(),
                location: SourceSpan {
                    start_line: line,
                    end_line: line,
                    page: None,
                },
            }],
            explanation: format!("Пояснение {id}."),
            suggestion: Some(format!("Рекомендация {id}.")),
            detector,
            additional_detectors: Vec::new(),
            merged_ids: Vec::new(),
            fallback_evidence: false,
            verified: false,
        }
    }

    #[test]
    fn keeps_examples_and_addresses_intact_when_merging() {
        for (first, second) in [
            (
                "Отчёт доступен для г. Алматы.",
                "Отчёт доступен для г. Астана.",
            ),
            (
                "Проверить адрес для ул. Абая.",
                "Проверить адрес для ул. Сатпаева.",
            ),
            (
                "Проверить пример, напр. Алматы.",
                "Проверить пример, напр. Астана.",
            ),
        ] {
            let mut text = first.to_owned();
            append_distinct_sentences(&mut text, second);
            assert_eq!(text, format!("{first} {second}"));
        }
    }

    #[test]
    fn preserves_abbreviations_and_requirement_ids_when_joining_sentences() {
        let mut text = "Проверить ER-3. Уточнить п. 4.".to_owned();
        append_distinct_sentences(
            &mut text,
            "ER-3. Лимит — 10 тыс. тенге и т. п. Уточнить п. 3.",
        );
        assert_eq!(
            text,
            "Проверить ER-3. Уточнить п. 4. ER-3. Лимит — 10 тыс. тенге и т. п. Уточнить п. 3."
        );
    }

    #[test]
    fn preserves_conflicting_numeric_values() {
        let left = "Динамический код оплаты действует в течение 15 минут после создания.";
        let right = "Динамический код оплаты действует в течение 10 минут после создания.";
        assert!(!similar_sentences(left, right));
    }

    #[test]
    fn keeps_distinct_explanations_even_when_a_single_quote_matches() {
        let mut verified = make_finding(
            "requirement_checks-001",
            Scope::Requirement("R-001".into()),
            Criterion::Verifiable,
            Severity::Medium,
            "Проверить журнал запросов.",
            3,
            Detector::Llm("requirement_checks:test".into()),
        );
        verified.verified = true;
        verified.explanation = "Не указан срок хранения журнала.".into();
        let mut pending = verified.clone();
        pending.id = "ambiguity-001".into();
        pending.detector = Detector::Llm("ambiguity:test".into());
        pending.verified = false;
        pending.explanation = "Не определено, какие запросы записываются в журнал.".into();
        assert_eq!(merge_findings(vec![verified, pending]).len(), 2);
    }

    #[test]
    fn doesnt_absorb_an_unverified_critical_issue_into_a_wording_warning() {
        let mut rule = make_finding(
            "vague-term-009",
            Scope::Requirement("R-025".into()),
            Criterion::Unambiguous,
            Severity::Medium,
            "Номера карт записываются в журнал приложения в открытом виде.",
            2,
            Detector::Rule("vague-terms".into()),
        );
        rule.verified = true;
        let critical = make_finding(
            "set_checks-007",
            rule.scope.clone(),
            Criterion::Appropriate,
            Severity::Critical,
            "Полные номера карт записываются в журнал приложения.",
            2,
            Detector::Llm("set_checks:test".into()),
        );
        for input in [
            vec![rule.clone(), critical.clone()],
            vec![critical.clone(), rule],
        ] {
            let findings = merge_findings(input);
            assert_eq!(findings.len(), 2);
            assert!(findings.iter().any(|f| f.id == critical.id
                && f.severity == Severity::Critical
                && f.explanation == critical.explanation));
        }
    }

    #[test]
    fn keeps_new_sentences_when_merging_repeated_recommendations() {
        let mut first = make_finding(
            "set_checks-001",
            Scope::Set,
            Criterion::Consistent,
            Severity::High,
            "Динамический QR-код действует 15 минут.",
            16,
            Detector::Llm("set_checks:test".into()),
        );
        first.explanation =
            "Срок действия динамического QR-кода различается в разделах спецификации.".into();
        let suggestion = "Привести значение срока действия динамического QR-кода к единому значению во всех разделах спецификации (например, 15 минут).";
        first.suggestion = Some(suggestion.into());
        let mut duplicate = first.clone();
        duplicate.id = "ambiguity-001".into();
        duplicate.detector = Detector::Llm("ambiguity:test".into());
        duplicate.explanation = "Срок действия динамического QR-кода различается в разделах документации. В тестах нужен сценарий просрочки.".into();
        duplicate.suggestion = Some("Привести значение срока действия динамического QR-кода к единому значению (например, 15 минут) во всех разделах документации. Добавить приёмочный тест.".into());

        let merged = merge_findings(vec![first, duplicate]);
        assert_eq!(
            merged[0].explanation,
            "Срок действия динамического QR-кода различается в разделах спецификации. В тестах нужен сценарий просрочки."
        );
        assert_eq!(
            merged[0].suggestion,
            Some(format!("{suggestion} Добавить приёмочный тест."))
        );
    }

    #[test]
    fn merges_same_requirement_overlapping_quotes_across_stages() {
        let first = make_finding(
            "requirement_checks-002",
            Scope::Requirement("R-009".into()),
            Criterion::Complete,
            Severity::Medium,
            "Повторная доставка одного события сохраняет исходный итог.",
            52,
            Detector::Llm("requirement_checks:gemini-2.5-flash".into()),
        );
        let second = make_finding(
            "ambiguity-003",
            Scope::Requirement("R-009".into()),
            Criterion::Unambiguous,
            Severity::High,
            "сохраняет исходный итог",
            52,
            Detector::Llm("ambiguity:gemini-2.5-flash".into()),
        );

        let merged = merge_findings(vec![first, second]);
        assert_eq!(merged.len(), 1);
        let item = &merged[0];
        assert_eq!(item.id, "requirement_checks-002");
        assert_eq!(item.severity, Severity::High);
        assert_eq!(
            item.all_criteria(),
            vec![Criterion::Complete, Criterion::Unambiguous]
        );
        assert_eq!(
            item.all_detectors(),
            vec![
                &Detector::Llm("requirement_checks:gemini-2.5-flash".into()),
                &Detector::Llm("ambiguity:gemini-2.5-flash".into()),
            ]
        );
        assert_eq!(
            item.all_ids(),
            vec!["requirement_checks-002", "ambiguity-003"]
        );
        assert_eq!(item.evidence.len(), 1);
        assert!(
            item.explanation
                .contains("Пояснение requirement_checks-002")
        );
        assert!(item.explanation.contains("Пояснение ambiguity-003"));
    }

    #[test]
    fn merges_rule_and_llm_findings_for_same_requirement_and_quote() {
        let rule = make_finding(
            "vague-term-001",
            Scope::Requirement("R-003".into()),
            Criterion::Unambiguous,
            Severity::Medium,
            "Сервис отвечает быстро на запросы пользователя.",
            18,
            Detector::Rule("vague-terms".into()),
        );
        let llm = make_finding(
            "ambiguity-001",
            Scope::Requirement("R-003".into()),
            Criterion::Verifiable,
            Severity::High,
            "отвечает быстро",
            18,
            Detector::Llm("ambiguity:gemini-2.5-flash".into()),
        );

        let merged = merge_findings(vec![rule, llm]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].severity, Severity::High);
        assert_eq!(merged[0].all_detectors().len(), 2);
        assert_eq!(merged[0].all_criteria().len(), 2);
    }

    #[test]
    fn does_not_merge_different_requirements() {
        let first = make_finding(
            "requirement_checks-001",
            Scope::Requirement("R-001".into()),
            Criterion::Complete,
            Severity::Medium,
            "Сумма задаётся в тенге.",
            10,
            Detector::Llm("requirement_checks:test".into()),
        );
        let second = make_finding(
            "ambiguity-001",
            Scope::Requirement("R-002".into()),
            Criterion::Unambiguous,
            Severity::High,
            "Сумма задаётся в тенге.",
            10,
            Detector::Llm("ambiguity:test".into()),
        );

        let merged = merge_findings(vec![first, second]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn does_not_merge_non_overlapping_quotes_in_same_requirement() {
        let first = make_finding(
            "requirement_checks-001",
            Scope::Requirement("R-002".into()),
            Criterion::Complete,
            Severity::Medium,
            "Месяц определяется по дате совершения операции.",
            38,
            Detector::Llm("requirement_checks:test".into()),
        );
        let second = make_finding(
            "ambiguity-001",
            Scope::Requirement("R-002".into()),
            Criterion::Unambiguous,
            Severity::Medium,
            "Переводы и снятие наличных исключаются.",
            38,
            Detector::Llm("ambiguity:test".into()),
        );

        let merged = merge_findings(vec![first, second]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn does_not_merge_distinct_criteria_from_same_stage() {
        let first = make_finding(
            "requirement_checks-001",
            Scope::Requirement("R-001".into()),
            Criterion::Appropriate,
            Severity::Medium,
            "Передавать параметры через GET.",
            12,
            Detector::Llm("requirement_checks:test".into()),
        );
        let second = make_finding(
            "requirement_checks-002",
            Scope::Requirement("R-001".into()),
            Criterion::Complete,
            Severity::Medium,
            "Передавать параметры через GET.",
            12,
            Detector::Llm("requirement_checks:test".into()),
        );

        let merged = merge_findings(vec![first, second]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merges_verified_finding_with_unverified_paraphrase_in_either_order() {
        let mut verified_rule = make_finding(
            "vague-term-001",
            Scope::Requirement("R-003".into()),
            Criterion::Unambiguous,
            Severity::Medium,
            "Сервис отвечает быстро на запросы пользователя.",
            18,
            Detector::Rule("vague-terms".into()),
        );
        verified_rule.verified = true;

        let mut unverified_llm = make_finding(
            "ambiguity-001",
            Scope::Requirement("R-003".into()),
            Criterion::Unambiguous,
            Severity::Medium,
            "Сервис отвечает быстро на запросы клиентов",
            18,
            Detector::Llm("ambiguity:test".into()),
        );
        unverified_llm.explanation = verified_rule.explanation.clone();

        for input in [
            vec![verified_rule.clone(), unverified_llm.clone()],
            vec![unverified_llm, verified_rule.clone()],
        ] {
            let merged = merge_findings(input);
            assert_eq!(merged.len(), 1);
            assert!(merged[0].verified);
            assert_eq!(merged[0].id, verified_rule.id);
            assert_eq!(merged[0].evidence.len(), 1);
            assert_eq!(merged[0].evidence[0].quote, verified_rule.evidence[0].quote);
            assert_eq!(merged[0].merged_ids, ["ambiguity-001"]);
        }
    }

    #[test]
    fn does_not_merge_unrelated_document_level_rules_sharing_fallback_heading() {
        let mut testing_missing = make_finding(
            "testing-section-missing",
            Scope::Set,
            Criterion::SetComplete,
            Severity::Medium,
            "Спецификация платёжного шлюза",
            1,
            Detector::Rule("testing-section".into()),
        );
        testing_missing.fallback_evidence = true;
        testing_missing.verified = true;

        let mut version_missing = make_finding(
            "document-version-date-missing",
            Scope::Set,
            Criterion::Conforming,
            Severity::Medium,
            "Спецификация платёжного шлюза",
            1,
            Detector::Rule("document-version-date".into()),
        );
        version_missing.fallback_evidence = true;
        version_missing.verified = true;

        let mut unrelated_set = make_finding(
            "set_checks-001",
            Scope::Set,
            Criterion::Ranked,
            Severity::Low,
            "Спецификация платёжного шлюза",
            1,
            Detector::Llm("set_checks:test".into()),
        );
        unrelated_set.verified = true;

        let merged = merge_findings(vec![testing_missing, version_missing, unrelated_set]);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn keeps_conflicts_with_different_evidence_separate() {
        let mut left = make_finding(
            "set_checks-001",
            Scope::Set,
            Criterion::Consistent,
            Severity::High,
            "FR-3. Код действует 15 минут.",
            3,
            Detector::Llm("set_checks:test".into()),
        );
        left.evidence.push(Evidence {
            quote: "ER-2. Код устаревает через 10 минут.".into(),
            location: SourceSpan {
                start_line: 5,
                end_line: 5,
                page: None,
            },
        });
        let mut duplicate = left.clone();
        duplicate.id = "ambiguity-001".into();
        duplicate.criterion = Criterion::Unambiguous;
        duplicate.detector = Detector::Llm("ambiguity:test".into());
        duplicate.evidence[1].quote = "ER-2. Код устаревает через 10 мин.".into();
        left.verified = true;
        let mut other = duplicate.clone();
        other.id = "ambiguity-002".into();
        other.evidence[1].location.start_line = 7;
        other.evidence[1].location.end_line = 7;

        let merged = merge_findings(vec![left, duplicate, other]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].merged_ids, ["ambiguity-001"]);
        assert_eq!(merged[0].evidence.len(), 2);
        assert!(matches!(merged[0].scope, Scope::Set));
        assert!(matches!(merged[1].scope, Scope::Set));
    }

    #[test]
    fn merges_set_level_llm_findings_sharing_real_quote() {
        let mut set_check = make_finding(
            "set_checks-001",
            Scope::Set,
            Criterion::Consistent,
            Severity::Medium,
            "Месяц определяется по дате совершения покупки в Алматы.",
            38,
            Detector::Llm("set_checks:test".into()),
        );
        set_check.verified = true;

        let mut ambiguity = make_finding(
            "ambiguity-001",
            Scope::Set,
            Criterion::Unambiguous,
            Severity::High,
            "в Алматы",
            38,
            Detector::Llm("ambiguity:test".into()),
        );
        ambiguity.verified = true;

        let merged = merge_findings(vec![set_check, ambiguity]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].verified);
        assert_eq!(merged[0].severity, Severity::High);
        assert_eq!(merged[0].all_detectors().len(), 2);
    }
}
