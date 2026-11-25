use std::collections::HashSet;

use crate::{Detector, Evidence, Finding};

pub fn merge_findings(findings: Vec<Finding>) -> Vec<Finding> {
    let mut merged: Vec<Finding> = Vec::with_capacity(findings.len());
    for candidate in findings {
        if let Some(target) = merged
            .iter_mut()
            .find(|existing| should_merge(existing, &candidate))
        {
            merge_into(target, candidate);
        } else {
            merged.push(candidate);
        }
    }
    merged
}

fn should_merge(left: &Finding, right: &Finding) -> bool {
    if left.scope != right.scope {
        return false;
    }
    if !quotes_overlap(&left.evidence, &right.evidence) {
        return false;
    }
    let left_stages = finding_stages(left);
    let right_stages = finding_stages(right);
    let different_stage = left_stages.is_disjoint(&right_stages);
    if different_stage {
        return true;
    }
    // Внутри одной стадии сливаем только точные дубликаты по одному и тому же критерию.
    let left_criteria = left.all_criteria();
    right
        .all_criteria()
        .into_iter()
        .any(|criterion| left_criteria.contains(&criterion))
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

fn quotes_overlap(left: &[Evidence], right: &[Evidence]) -> bool {
    left.iter().any(|ev_a| {
        right
            .iter()
            .any(|ev_b| spans_overlap(ev_a, ev_b) && texts_overlap(&ev_a.quote, &ev_b.quote))
    })
}

fn spans_overlap(left: &Evidence, right: &Evidence) -> bool {
    left.location.page == right.location.page
        && left.location.start_line <= right.location.end_line
        && right.location.start_line <= left.location.end_line
}

fn normalize_text(text: &str) -> String {
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
    target.severity = target.severity.max(other.severity);

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

    merge_evidence(&mut target.evidence, other.evidence);

    let target_norm = normalize_text(&target.explanation);
    let other_norm = normalize_text(&other.explanation);
    if !other_norm.is_empty() && !target_norm.contains(&other_norm) {
        if !target.explanation.trim_end().ends_with(['.', '!', '?']) {
            target.explanation.push('.');
        }
        target.explanation.push(' ');
        target.explanation.push_str(other.explanation.trim());
    }

    match (&mut target.suggestion, other.suggestion) {
        (None, Some(suggestion)) => target.suggestion = Some(suggestion),
        (Some(existing), Some(addition)) => {
            let existing_norm = normalize_text(existing);
            let addition_norm = normalize_text(&addition);
            if !addition_norm.is_empty() && !existing_norm.contains(&addition_norm) {
                if !existing.trim_end().ends_with(['.', '!', '?']) {
                    existing.push('.');
                }
                existing.push(' ');
                existing.push_str(addition.trim());
            }
        }
        _ => {}
    }

    target.verified = target.verified && other.verified;
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
    use crate::{Criterion, Scope, Severity};

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
            verified: false,
        }
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
}
