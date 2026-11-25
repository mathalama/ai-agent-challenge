use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use agent::{AgentEvent, Input, Provider, Session, SessionState, build_provider};
use review::{JsonRenderer, ProgressEvent, ReportRenderer};

const SAMPLE: &[u8] = include_bytes!("../../../samples/requirements.md");

#[tokio::test]
async fn chat_without_document_uses_templates() {
    let mut session = Session::new(|_, _| panic!("Провайдер создан до проверки документа"));
    let events = Arc::new(Mutex::new(Vec::new()));
    for text in ["Привет", "/help", "/check", "/export md", "/load sample.md"] {
        let events = Arc::clone(&events);
        session
            .handle(Input::Text(text), move |event| {
                events.lock().unwrap().push(event)
            })
            .await;
    }
    assert_eq!(session.state(), SessionState::Empty);
    {
        let received = events.lock().unwrap();
        assert_eq!(received.len(), 5);
        assert!(received[..4].iter().all(|event| matches!(
            event,
            AgentEvent::Message { text, .. } if text.contains("Загрузите")
        )));
        assert!(matches!(
            received.last(),
            Some(AgentEvent::Message { text, .. }) if text.contains("клиент")
        ));
    }

    session
        .handle(
            Input::Document {
                name: Path::new("sample.md"),
                bytes: SAMPLE,
                answers: Default::default(),
            },
            |_| {},
        )
        .await;
    assert_eq!(session.state(), SessionState::Loaded);
}

#[tokio::test]
async fn mock_review_streams_progress_and_exports_the_shared_report() {
    let mut session = Session::new(|trace, reasoning| {
        assert!(!reasoning);
        build_provider(Provider::Mock, true, trace, reasoning)
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let emit = {
        let events = Arc::clone(&events);
        move |event| events.lock().unwrap().push(event)
    };
    session
        .handle(
            Input::Document {
                name: Path::new("requirements.md"),
                bytes: SAMPLE,
                answers: Default::default(),
            },
            emit.clone(),
        )
        .await;
    session.handle(Input::Text("проверь"), emit.clone()).await;
    assert_eq!(session.state(), SessionState::Reviewed);
    let report = session.report().unwrap();
    assert!(report.summary.finding_count > 0);
    let expected = JsonRenderer.render(report).unwrap();
    session.handle(Input::Text("/export json"), emit).await;

    let events = events.lock().unwrap();
    let states = events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .filter(|event| event["event"] == "state_changed")
        .map(|event| event["state"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(states, ["loaded", "reviewing", "reviewed"]);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Progress {
            progress: ProgressEvent::StageStarted {
                stage: "segment",
                ..
            }
        }
    )));
    assert!(matches!(
        events[events.len() - 2],
        AgentEvent::ReportReady { .. }
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Export { content, .. }) if content == &expected
    ));
    let progress = events
        .iter()
        .find(|event| matches!(event, AgentEvent::Progress { .. }))
        .unwrap();
    let serialized = serde_json::to_value(progress).unwrap();
    assert_eq!(serialized["event"], "progress");
    assert_eq!(serialized["progress"]["event"], "run_started");
}

#[tokio::test]
async fn answers_update_the_report_and_survive_a_second_check() {
    const FIRST_ANSWER: &str = "Проверь с юристами, лимит 500 тыс.";
    let mut session =
        Session::new(|trace, reasoning| build_provider(Provider::Mock, true, trace, reasoning));
    session
        .handle(
            Input::Document {
                name: Path::new("requirements.md"),
                bytes: SAMPLE,
                answers: Default::default(),
            },
            |_| {},
        )
        .await;
    session.handle(Input::Text("/check"), |_| {}).await;
    let questions = &session.report().unwrap().questions;
    let first = questions[0].id.clone();
    let second = questions[1].id.clone();
    session.handle(Input::Text("/questions"), |_| {}).await;
    session
        .handle(Input::Text("Почему FR-3 критично?"), |event| {
            if matches!(event, AgentEvent::AnswersUpdated { .. }) {
                panic!("Свободный вопрос был записан как ответ");
            }
        })
        .await;
    assert_eq!(session.report().unwrap().questions[0].id, first);
    let first_command = format!("/answer {first} {FIRST_ANSWER}");
    session.handle(Input::Text(&first_command), |_| {}).await;
    let command = format!("/answer {second} Второй ответ");
    session
        .handle(Input::Text(&command), move |event| {
            if let AgentEvent::AnswersUpdated { answers } = event {
                assert_eq!(answers[&first], FIRST_ANSWER);
                assert_eq!(answers[&second], "Второй ответ");
            }
        })
        .await;
    let report = session.report().unwrap();
    let expected_answers = serde_json::to_value(&report.finding_answers).unwrap();
    let expected_questions = serde_json::to_value(&report.questions).unwrap();
    assert!(!report.finding_answers.is_empty());
    session.handle(Input::Text("/check"), |_| {}).await;
    let report = session.report().unwrap();
    assert_eq!(
        serde_json::to_value(&report.finding_answers).unwrap(),
        expected_answers
    );
    assert_eq!(
        serde_json::to_value(&report.questions).unwrap(),
        expected_questions
    );
}

#[tokio::test]
async fn reasoning_can_be_enabled_before_loading_the_document() {
    let mut session = Session::new(|trace, reasoning| {
        assert!(reasoning);
        build_provider(Provider::Mock, true, trace, reasoning)
    });
    session
        .handle(Input::Text("/reasoning on"), |event| {
            assert!(matches!(event, AgentEvent::Message { text, .. } if text.contains("включено")));
        })
        .await;
    assert_eq!(session.state(), SessionState::Empty);
    session
        .handle(
            Input::Document {
                name: Path::new("requirements.md"),
                bytes: SAMPLE,
                answers: Default::default(),
            },
            |_| {},
        )
        .await;
    session.handle(Input::Text("/check"), |_| {}).await;
    assert_eq!(session.state(), SessionState::Reviewed);
}

#[tokio::test]
async fn failed_checks_leave_the_document_available_for_retry() {
    let mut session = Session::new(|_, reasoning| {
        assert!(!reasoning);
        Err("Провайдер недоступен.".into())
    });
    session
        .handle(
            Input::Document {
                name: Path::new("requirements.md"),
                bytes: SAMPLE,
                answers: Default::default(),
            },
            |_| {},
        )
        .await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    session
        .handle(Input::Text("/check"), move |event| {
            captured.lock().unwrap().push(event);
        })
        .await;
    assert_eq!(session.state(), SessionState::Loaded);
    assert!(session.report().is_none());
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [
            AgentEvent::StateChanged { state: SessionState::Reviewing },
            AgentEvent::StateChanged { state: SessionState::Loaded },
            AgentEvent::Error { text },
        ] if text == "Провайдер недоступен."
    ));
}
