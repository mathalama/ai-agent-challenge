use std::{path::Path, sync::Mutex};

use doc::ParserRegistry;
use llm::{MockProvider, RetryProvider, TraceLog};
use review::{CheckRegistry, Pipeline, ProgressEvent, ProgressSink, RunContext};

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
