use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

use llm::generate_typed;
use schemars::JsonSchema;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    Evidence, Finding, ProgressEvent, Question, QuestionTopic, RelatedId, RunContext,
    dedup::normalize_text, pipeline::Result, stages::has_cyrillic,
};

#[derive(Deserialize, JsonSchema)]
struct QuestionDraft {
    text: String,
    why: String,
    topic: QuestionTopic,
    options: Vec<String>,
    finding_ids: Vec<String>,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

pub(crate) async fn ask_questions(ctx: &mut RunContext<'_>, prompt: &str) -> Result<()> {
    ctx.questions.clear();
    let verified: Vec<_> = ctx
        .findings
        .iter()
        .filter(|finding| finding.verified)
        .collect();
    if verified.is_empty() {
        return Ok(());
    }
    let mut pending = vec![verified.as_slice()];
    let mut drafts = Vec::new();
    let mut skipped_finding = false;
    while let Some(batch) = pending.pop() {
        let input = serde_json::to_string(batch)?;
        match generate_typed::<Vec<QuestionDraft>>(ctx.provider, "clarify", prompt, input).await {
            Ok(response) => drafts.extend(response.into_iter().map(|mut draft| {
                resolve_links(&mut draft, batch);
                draft
            })),
            Err(error) if error.is_output_limit() => {
                if batch.len() > 1 {
                    let (left, right) = batch.split_at(batch.len() / 2);
                    ctx.emit(|| ProgressEvent::ClarifyBatchSplit {
                        finding_ids: batch.iter().map(|finding| finding.id.clone()).collect(),
                    });
                    pending.push(right);
                    pending.push(left);
                } else {
                    skipped_finding = true;
                    ctx.emit(|| ProgressEvent::ClarifyFindingSkipped {
                        finding_id: batch[0].id.clone(),
                    });
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    ctx.questions = build_questions(drafts, &verified)?;
    if skipped_finding {
        ctx.mark_incomplete("clarify");
    }
    Ok(())
}

fn resolve_links(draft: &mut QuestionDraft, batch: &[&Finding]) {
    let declared: BTreeSet<_> = std::mem::take(&mut draft.finding_ids).into_iter().collect();
    let cited: Vec<_> = std::mem::take(&mut draft.evidence)
        .into_iter()
        .map(|item| (item.location, normalize_text(&item.quote)))
        .collect();
    for finding in batch {
        let declared_link = declared.contains(&finding.id);
        let selected: Vec<_> = finding
            .evidence
            .iter()
            .filter(|known| {
                let quote = normalize_text(&known.quote);
                cited.iter().any(|(location, text)| {
                    !quote.is_empty()
                        && quote == *text
                        && (known.location == *location
                            || declared_link
                                && known.location.page == location.page
                                && known.location.start_line.abs_diff(location.start_line) <= 1
                                && known.location.end_line.abs_diff(location.end_line) <= 1)
                })
            })
            .cloned()
            .collect();
        if !selected.is_empty() {
            draft.finding_ids.push(finding.id.clone());
            draft.evidence.extend(selected);
        }
    }

    // Без сопоставимых цитат восстанавливаем явные ссылки из текущей пачки.
    if draft.finding_ids.is_empty() {
        for finding in batch
            .iter()
            .filter(|finding| declared.contains(&finding.id))
        {
            draft.finding_ids.push(finding.id.clone());
            draft.evidence.extend_from_slice(&finding.evidence);
        }
    }
}

fn build_questions(drafts: Vec<QuestionDraft>, verified: &[&Finding]) -> Result<Vec<Question>> {
    let findings: BTreeMap<_, _> = verified
        .iter()
        .map(|finding| (finding.id.as_str(), *finding))
        .collect();
    let mut unique = BTreeMap::<String, QuestionDraft>::new();
    for mut draft in drafts {
        draft.text = draft.text.trim().to_owned();
        draft.why = draft.why.trim().to_owned();
        if !has_cyrillic(&draft.text) || !has_cyrillic(&draft.why) {
            warn!("Отброшен уточняющий вопрос без кириллицы");
            continue;
        }
        draft
            .finding_ids
            .retain(|id| findings.contains_key(id.as_str()));
        if draft.finding_ids.is_empty() {
            warn!("Отброшен уточняющий вопрос без ссылок на подтверждённые находки");
            continue;
        }
        let key = normalized(draft.text.trim_end_matches(['?', '.', '!', '…']));
        if let Some(existing) = unique.get_mut(&key) {
            existing.finding_ids.extend(draft.finding_ids);
            existing.options.extend(draft.options);
            existing.evidence.extend(draft.evidence);
        } else {
            unique.insert(key, draft);
        }
    }

    let mut questions = Vec::new();
    for (key, draft) in unique {
        let related: BTreeSet<_> = draft.finding_ids.into_iter().collect();
        let linked: Vec<_> = related.iter().map(|id| findings[id.as_str()]).collect();
        let Some(priority) = linked.iter().map(|finding| finding.severity).max() else {
            continue;
        };
        let mut seen_options = BTreeSet::new();
        let options: Vec<_> = draft
            .options
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty() && seen_options.insert(normalized(value)))
            .take(3)
            .map(str::to_owned)
            .collect();
        let mut evidence = draft.evidence;
        evidence.sort_by(|left, right| {
            let key = |span: doc::SourceSpan| (span.page, span.start_line, span.end_line);
            key(left.location)
                .cmp(&key(right.location))
                .then_with(|| left.quote.cmp(&right.quote))
        });
        evidence
            .dedup_by(|left, right| left.location == right.location && left.quote == right.quote);

        // Use question content and cited findings instead of their position in a model response.
        let finding_keys: BTreeSet<_> = linked
            .iter()
            .map(|finding| {
                let quotes: BTreeSet<_> = finding
                    .evidence
                    .iter()
                    .filter(|item| {
                        evidence.iter().any(|selected| {
                            selected.location == item.location && selected.quote == item.quote
                        })
                    })
                    .map(|item| &item.quote)
                    .collect();
                serde_json::to_string(&(finding.criterion, quotes))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect();
        let identity = serde_json::to_vec(&(key, seen_options, finding_keys))?;
        let mut id = String::from("Q-");
        for byte in &Sha256::digest(identity)[..12] {
            write!(id, "{byte:02x}").expect("запись в строку");
        }
        questions.push(Question {
            id,
            text: draft.text,
            why: draft.why,
            topic: draft.topic,
            options,
            evidence,
            priority,
            related: related.into_iter().map(RelatedId::Finding).collect(),
        });
    }
    questions.sort_by_key(|question| {
        (
            Reverse(question.priority),
            normalized(&question.text),
            question.id.clone(),
        )
    });
    Ok(questions)
}

fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use doc::SourceSpan;

    use super::*;
    use crate::{Criterion, Detector, Scope, Severity};

    fn evidence(quote: &str, line: usize) -> Evidence {
        Evidence {
            quote: quote.into(),
            location: SourceSpan {
                start_line: line,
                end_line: line,
                page: Some(1),
            },
        }
    }

    fn finding(id: &str, evidence: Vec<Evidence>) -> Finding {
        Finding {
            id: id.into(),
            scope: Scope::Set,
            criterion: Criterion::Consistent,
            additional_criteria: Vec::new(),
            severity: Severity::High,
            evidence,
            explanation: "В требованиях указаны разные значения.".into(),
            suggestion: None,
            detector: Detector::Llm("set_checks:test".into()),
            additional_detectors: Vec::new(),
            merged_ids: Vec::new(),
            fallback_evidence: false,
            verified: true,
        }
    }

    #[test]
    fn question_keeps_multiple_declared_findings_without_evidence() {
        let first_quote = evidence("Код истекает через 15 минут.", 8);
        let second_quote = evidence("Код истекает через 10 минут.", 15);
        let first = finding("set_checks-001", vec![first_quote.clone()]);
        let second = finding("set_checks-002", vec![second_quote.clone()]);
        let response = serde_json::json!({
            "text": "Когда истекает код?",
            "why": "От срока зависит проверка оплаты.",
            "topic": "other",
            "options": [],
            "finding_ids": [first.id, second.id, "set_checks-999"],
        });
        let batch = [&first, &second];
        let mut draft = serde_json::from_value(response).unwrap();
        resolve_links(&mut draft, &batch);
        let questions = build_questions(vec![draft], &batch).unwrap();

        assert_eq!(questions.len(), 1);
        assert!(matches!(
            questions[0].related.as_slice(),
            [RelatedId::Finding(left), RelatedId::Finding(right)]
                if *left == first.id && *right == second.id
        ));
        assert_eq!(
            questions[0]
                .evidence
                .iter()
                .map(|item| (&item.quote, item.location))
                .collect::<Vec<_>>(),
            [
                (&first_quote.quote, first_quote.location),
                (&second_quote.quote, second_quote.location)
            ]
        );
    }

    #[test]
    fn keeps_declared_links_when_model_misses_the_exact_quote() {
        let canonical = evidence("Код устаревает после статуса «Ожидание».", 15);
        let conflict = finding("set_checks-001", vec![canonical.clone()]);
        let unrelated = finding("set_checks-002", vec![evidence("Лимит — 500 тенге.", 12)]);
        let batch = [&conflict, &unrelated];
        for (index, draft_quote) in [
            evidence("Код устаревает после статуса “Ожидание”.", 16),
            evidence("Код переходит в ожидание.", 20),
        ]
        .into_iter()
        .enumerate()
        {
            let mut draft = QuestionDraft {
                text: "Когда код устаревает?".into(),
                why: "От срока зависит проверка.".into(),
                topic: QuestionTopic::Other,
                options: vec![],
                finding_ids: vec![conflict.id.clone()],
                evidence: vec![draft_quote],
            };
            if index == 0 {
                draft.finding_ids.push(unrelated.id.clone());
            }
            resolve_links(&mut draft, &batch);
            assert_eq!(
                draft.finding_ids.as_slice(),
                std::slice::from_ref(&conflict.id)
            );
            assert_eq!(draft.evidence.len(), 1);
            assert_eq!(draft.evidence[0].quote, canonical.quote);
            assert_eq!(draft.evidence[0].location, canonical.location);
        }
    }

    #[test]
    fn question_keeps_only_findings_and_quotes_supported_by_its_response() {
        let fr3 = evidence("FR-3. Код действует 15 минут.", 16);
        let er2 = evidence("ER-2. Код устаревает через 10 минут.", 30);
        let conflict = finding(
            "set_checks-001",
            vec![
                fr3.clone(),
                er2.clone(),
                evidence("Срок указан в протоколе.", 35),
            ],
        );
        let unrelated = finding(
            "set_checks-002",
            vec![evidence(
                "FR-6. Максимальная сумма составляет 1000 тенге.",
                18,
            )],
        );
        let same_text_elsewhere = finding("set_checks-003", vec![evidence(&er2.quote, 40)]);
        let response = serde_json::json!({
            "text": "Каков срок действия QR-кода: 15 минут или 10 минут?",
            "why": "От ответа зависит обработка просроченных платежей.",
            "topic": "other",
            "options": ["15 минут", "10 минут"],
            "finding_ids": ["set_checks-001", "set_checks-002", "set_checks-003"],
            "evidence": [fr3, er2, evidence("Подставная цитата", 18)],
        });
        let mut draft = serde_json::from_value(response).unwrap();
        let batch = [&conflict, &unrelated, &same_text_elsewhere];
        resolve_links(&mut draft, &batch);
        let questions = build_questions(vec![draft], &batch).unwrap();
        assert_eq!(questions.len(), 1);
        assert!(
            matches!(questions[0].related.as_slice(), [RelatedId::Finding(id)] if id == "set_checks-001")
        );
        assert_eq!(
            questions[0]
                .evidence
                .iter()
                .map(|item| (&item.quote, item.location))
                .collect::<Vec<_>>(),
            [(&fr3.quote, fr3.location), (&er2.quote, er2.location)]
        );
    }
}
