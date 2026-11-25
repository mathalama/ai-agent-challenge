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
    Finding, Question, QuestionTopic, RelatedId, RunContext, pipeline::Result, stages::has_cyrillic,
};

#[derive(Deserialize, JsonSchema)]
struct QuestionDraft {
    text: String,
    why: String,
    topic: QuestionTopic,
    options: Vec<String>,
    finding_ids: Vec<String>,
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
    let drafts = generate_typed(
        ctx.provider,
        "clarify",
        prompt,
        serde_json::to_string(&verified)?,
    )
    .await?;
    ctx.questions = build_questions(drafts, &verified)?;
    Ok(())
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
            continue;
        }
        let key = normalized(draft.text.trim_end_matches(['?', '.', '!', '…']));
        if let Some(existing) = unique.get_mut(&key) {
            existing.finding_ids.extend(draft.finding_ids);
            existing.options.extend(draft.options);
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
            .map(str::to_owned)
            .collect();
        let mut evidence: Vec<_> = linked
            .iter()
            .flat_map(|finding| finding.evidence.iter().cloned())
            .collect();
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
                let quotes: BTreeSet<_> = finding.evidence.iter().map(|item| &item.quote).collect();
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
