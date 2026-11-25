use std::{path::Path, sync::Mutex};

use async_trait::async_trait;
use doc::ParserRegistry;
use llm::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, MockProvider, RetryProvider, TraceLog, Usage,
};
use review::{
    CheckRegistry, MarkdownRenderer, Pipeline, ProgressEvent, ProgressSink, Report, ReportRenderer,
    RunContext, RunPassport,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Recorder(Mutex<Vec<ProgressEvent>>);

impl ProgressSink for Recorder {
    fn emit(&self, event: &ProgressEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[tokio::test]
async fn pipeline_reports_every_stage_with_llm_usage() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let input = workspace.join("samples/requirements.md");
    let document = ParserRegistry::default()
        .parse(&input, &std::fs::read(&input).unwrap())
        .unwrap();
    let trace = TraceLog::default();
    let provider = RetryProvider::new(Box::new(MockProvider)).with_trace(trace.clone());
    let checks = CheckRegistry::with_builtin_rules();
    let recorder = Recorder::default();
    let mut ctx = RunContext::new(document, &provider, &checks).with_progress(&recorder, trace);

    Pipeline::default().run(&mut ctx).await.unwrap();

    let events = recorder.0.into_inner().unwrap();
    let ProgressEvent::RunStarted { model, stages, .. } = &events[0] else {
        panic!("первым должно идти run_started: {:?}", events[0]);
    };
    assert_eq!(model, "mock");
    assert_eq!(stages.first(), Some(&"segment"));
    assert_eq!(stages.last(), Some(&"clarify"));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::RunFinished {
            succeeded: true,
            ..
        })
    ));

    let finished: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ProgressEvent::StageFinished {
                stage,
                index,
                total,
                succeeded,
                llm_calls,
                input_tokens,
                requirements,
                ..
            } => Some((
                *stage,
                *index,
                *total,
                *succeeded,
                *llm_calls,
                *input_tokens,
                *requirements,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), stages.len());
    for (position, (stage, index, total, succeeded, ..)) in finished.iter().enumerate() {
        assert_eq!(
            (*stage, *index, *total),
            (stages[position], position + 1, stages.len())
        );
        assert!(succeeded);
    }
    let segment = finished[0];
    assert!(
        segment.4 >= 1 && segment.5 > 0,
        "segment без вызова LLM: {segment:?}"
    );
    assert!(segment.6 > 0, "после segment должны быть требования");

    let json = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(json["event"], "stage_started");
    assert_eq!(json["stage"], "segment");
}

struct LimitedFacts {
    requests: Mutex<Vec<Vec<String>>>,
    finish_reason: &'static str,
}

#[async_trait]
impl LlmProvider for LimitedFacts {
    fn model(&self) -> &str {
        "test-limit"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage != "facts" {
            return MockProvider.generate(request).await;
        }
        let input: Value = serde_json::from_str(&request.user_text).unwrap();
        let rows: Vec<_> = input["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .chain(input["context"].as_array().unwrap())
            .collect();
        let ids: Vec<_> = rows
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect();
        self.requests.lock().unwrap().push(ids.clone());
        if ids.len() > 1 || ids[0] == "R-002" {
            return Err(LlmError::MissingCandidate {
                finish_reason: self.finish_reason.into(),
                block_reason: "—".into(),
                usage: Some(Usage {
                    input_tokens: 2,
                    output_tokens: 10,
                }),
            });
        }
        Ok(LlmResponse {
            json: json!([{
                "entity": format!("источник {}", ids[0]),
                "requirement_id": ids[0],
                "quote": if ids[0] == "R-003" { Some("цитаты нет в документе") } else { None },
                "data": {"kind": "frequency", "maximum": 1, "period": "week"}
            }]),
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

fn facts_input() -> doc::Document {
    let text = "* Ф-1. Первое правило.\n* Ф-2. Второе правило.\n* Ф-3. Третье правило.\n\n## API\n\n| Поле | Значение |\n| --- | --- |\n| Controller | TestController |\n| REST type | POST |\n";
    ParserRegistry::default()
        .parse(Path::new("limits.md"), text.as_bytes())
        .unwrap()
}

#[tokio::test]
async fn length_splits_batches_and_warns_when_a_single_source_is_skipped() {
    let recorder = Recorder::default();
    let provider = LimitedFacts {
        requests: Mutex::default(),
        finish_reason: "length",
    };
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(facts_input(), &provider, &checks)
        .with_progress(&recorder, TraceLog::default());

    Pipeline::default().run(&mut ctx).await.unwrap();

    assert_eq!(
        *provider.requests.lock().unwrap(),
        [
            vec!["R-001", "R-002", "R-003"],
            vec!["R-001"],
            vec!["R-002", "R-003"],
            vec!["R-002"],
            vec!["R-003"],
            vec!["C-001", "C-002"],
            vec!["C-001"],
            vec!["C-002"],
        ]
    );
    assert_eq!(
        ctx.facts
            .iter()
            .map(|fact| fact.requirement_id.as_str())
            .collect::<Vec<_>>(),
        ["R-001", "C-001", "C-002"]
    );
    let events = recorder.0.into_inner().unwrap();
    let events: Vec<_> = events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "fact_batch_split"
                && event["source_ids"] == json!(["C-001", "C-002"]))
    );
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "fact_source_skipped"
                && event["source_id"] == "R-002"
                && event["location"]["start_line"] == 2)
    );
    assert_eq!(events.last().unwrap()["succeeded"], true);
}

#[tokio::test]
async fn unrelated_provider_error_still_fails_the_facts_stage() {
    let provider = LimitedFacts {
        requests: Mutex::default(),
        finish_reason: "content_filter",
    };
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(facts_input(), &provider, &checks);

    let error = Pipeline::default().run(&mut ctx).await.unwrap_err();

    assert!(error.to_string().contains("finishReason=content_filter"));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

struct OverflowReview {
    stage: &'static str,
}

fn output_limit() -> LlmError {
    LlmError::MissingCandidate {
        finish_reason: "length".into(),
        block_reason: "—".into(),
        usage: None,
    }
}

#[async_trait]
impl LlmProvider for OverflowReview {
    fn model(&self) -> &str {
        "test-limit"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        if request.stage == self.stage {
            return Err(output_limit());
        }
        let json = match request.stage.as_str() {
            "requirement_checks" => {
                let input: Vec<Value> = serde_json::from_str(&request.user_text).unwrap();
                json!(
                    input
                        .iter()
                        .map(|req| json!({
                            "requirement_id": req["id"],
                            "criterion": "complete",
                            "severity": "medium",
                            "evidence": [{"quote": req["quote"], "location": req["location"]}],
                            "explanation": "Нужны условия архивации.",
                            "suggestion": null
                        }))
                        .collect::<Vec<_>>()
                )
            }
            "clarify" => {
                let input: Vec<Value> = serde_json::from_str(&request.user_text).unwrap();
                if input.len() > 2 {
                    return Err(output_limit());
                }
                if input
                    .iter()
                    .any(|finding| finding["id"] == "requirement_checks-003")
                {
                    return Err(output_limit());
                }
                json!([{
                    "text": "Когда нужно архивировать запись?",
                    "why": "От срока зависит проверка архивации.",
                    "topic": "other",
                    "options": [],
                    "finding_ids": input.iter().map(|finding| &finding["id"]).collect::<Vec<_>>(),
                }])
            }
            _ => return MockProvider.generate(request).await,
        };
        Ok(LlmResponse {
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn output_limits_preserve_partial_questions_and_report_incomplete_stages() {
    let source = "* Ф-1. Архивировать договор.\n* Ф-2. Архивировать заявку.\n* Ф-3. Архивировать решение.\n* Ф-4. Архивировать справку.\n";
    let document = ParserRegistry::default()
        .parse(Path::new("archive.md"), source.as_bytes())
        .unwrap();
    let recorder = Recorder::default();
    let provider = OverflowReview {
        stage: "set_checks",
    };
    let checks = CheckRegistry::default();
    let mut ctx =
        RunContext::new(document, &provider, &checks).with_progress(&recorder, TraceLog::default());

    Pipeline::default().run(&mut ctx).await.unwrap();

    let report = Report::from_run(
        ctx,
        RunPassport {
            document_title: "Архив".into(),
            date: "2026-11-25".into(),
            model: provider.model().into(),
            agent_version: "test".into(),
        },
    );
    assert_eq!(report.findings.iter().filter(|f| f.verified).count(), 4);
    assert_eq!(report.questions.len(), 1);
    assert_eq!(report.questions[0].evidence.len(), 3);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["readiness"], "blocked");
    assert_eq!(json["incomplete_stages"], json!(["set_checks", "clarify"]));
    assert!(
        MarkdownRenderer
            .render(&report)
            .unwrap()
            .contains("Проверка неполная")
    );
    let events = recorder.0.into_inner().unwrap();
    let events: Vec<_> = events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "stage_output_limit" && event["stage"] == "set_checks")
    );
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "clarify_batch_split"
                && event["finding_ids"].as_array().unwrap().len() == 4)
    );
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "clarify_finding_skipped"
                && event["finding_id"] == "requirement_checks-003")
    );
    assert_eq!(events.last().unwrap()["succeeded"], true);
}

#[tokio::test]
async fn segmentation_overflow_still_keeps_deterministic_requirements() {
    let provider = OverflowReview { stage: "segment" };
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(facts_input(), &provider, &checks);
    Pipeline::default().run(&mut ctx).await.unwrap();
    assert_eq!(ctx.requirements.len(), 3);
    assert_eq!(ctx.context_items.len(), 2);
    assert!(ctx.findings.iter().all(|finding| finding.verified));
}
