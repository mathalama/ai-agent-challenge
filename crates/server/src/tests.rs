use super::*;
use llm::settings::Provider;

#[tokio::test]
async fn busy_reports_return_conflict_without_waiting_for_the_lock() {
    let app = mock_app();
    let (_, Json(created)) = create_session(State(app.clone())).await.unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    let chat = app.chat(&id).unwrap();
    let _running = chat.begin().unwrap();
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        report(
            State(app),
            RoutePath(id),
            Query(ReportQuery {
                format: Format::Json,
            }),
        ),
    )
    .await;
    assert!(matches!(result, Ok(Err(ApiError(StatusCode::CONFLICT, _)))));
}

#[tokio::test]
async fn expiring_a_session_closes_an_open_sse_stream() {
    let app = mock_app();
    let (_, Json(created)) = create_session(State(app.clone())).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let events = Arc::clone(&app.chat(id).unwrap().events);
    let stream = events.subscribe(0);
    app.sessions
        .lock()
        .unwrap()
        .get_mut(&Uuid::parse_str(id).unwrap())
        .unwrap()
        .last_used = Instant::now() - SESSION_IDLE;
    let (status, _) = create_session(State(app.clone())).await.unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert!(matches!(
        app.chat(id),
        Err(ApiError(StatusCode::NOT_FOUND, _))
    ));
    let payload = tokio::time::timeout(
        Duration::from_secs(1),
        axum::body::to_bytes(stream.into_body(), 1024),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        std::str::from_utf8(&payload)
            .unwrap()
            .contains("event: expired")
    );
}

fn mock_app() -> App {
    App {
        sessions: Arc::default(),
        provider: Arc::new(ProviderFactory::new(Provider::Mock, true)),
        config_path: None,
    }
}

#[tokio::test]
async fn http_session_replays_events_and_serves_the_mock_report() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    let app = router(
        ProviderFactory::new(Provider::Mock, true),
        None,
        Path::new("web/dist"),
        true,
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let foreign = client
        .post(format!("{base}/sessions"))
        .header("host", "example.test")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    let created = client
        .post(format!("{base}/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    assert_eq!(created["greeting"]["event"], "message");
    assert_eq!(created["provider"], "Mock");
    let session = format!("{base}/sessions/{}", created["id"].as_str().unwrap());

    let response = client
        .get(format!("{session}/report"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = format!(
        "--sample\r\nContent-Disposition: form-data; name=\"file\"; filename=\"requirements.md\"\r\n\r\n{}\r\n--sample--\r\n",
        include_str!("../../../samples/requirements.md"),
    );
    let upload = client
        .post(format!("{session}/document"))
        .header("content-type", "multipart/form-data; boundary=sample")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::ACCEPTED);
    let (load_events, cursor) = read_operation(&client, &session, "0").await;
    assert!(load_events.contains("\"state\":\"loaded\""));

    send_message(&client, &session, "/check").await;
    let (check_events, cursor) = read_operation(&client, &session, &cursor).await;
    assert!(check_events.contains("\"event\":\"run_started\""));
    assert!(check_events.contains("\"event\":\"report_ready\""));
    assert!(!check_events.contains("\"state\":\"loaded\""));

    let report = client
        .get(format!("{session}/report"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let report: Value = report.json().await.unwrap();
    assert_eq!(report["passport"]["model"], "mock");
    assert_eq!(report["readiness"], "blocked");
    let markdown = client
        .get(format!("{session}/report?format=md"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(markdown.text().await.unwrap().contains("**Не готово.**"));

    send_message(&client, &session, "Что найдено?").await;
    let (chat_events, _) = read_operation(&client, &session, &cursor).await;
    assert!(chat_events.contains("\"citations\":[{"));
    server.abort();
}

async fn send_message(client: &reqwest::Client, session: &str, text: &str) {
    let response = client
        .post(format!("{session}/messages"))
        .json(&json!({ "text": text }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

async fn read_operation(client: &reqwest::Client, session: &str, cursor: &str) -> (String, String) {
    let mut stream = client
        .get(format!("{session}/events"))
        .header("last-event-id", cursor)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(stream.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut received = Vec::new();
    while let Some(chunk) = stream.chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
        let Ok(text) = std::str::from_utf8(&received) else {
            continue;
        };
        if let Some(done) = text
            .split_terminator("\n\n")
            .find(|frame| frame.contains("event: done") && frame.contains("data: {}"))
        {
            let id = done
                .lines()
                .find_map(|line| line.strip_prefix("id: "))
                .unwrap();
            return (text.to_owned(), id.into());
        }
    }
    panic!("Поток завершился без события done");
}

#[tokio::test]
async fn localhost_settings_mask_keys_and_change_the_active_factory() {
    use llm::settings::Settings;

    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let factory = ProviderFactory::with_settings(None, true, Settings::default());
    let router = router(factory, Some(config.clone()), directory.path(), true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let client = reqwest::Client::new();
    let response = client
        .put(format!("{base}/settings"))
        .header("origin", "https://example.test")
        .json(&json!({"provider": "mock"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!config.exists());

    let response = client
        .put(format!("{base}/settings"))
        .json(&json!({
            "provider": "openai", "model": "test/model", "api_key": "fake-settings-secret-1a2b"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!response.contains("fake-settings-secret"));
    let settings: Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["openai"]["api_key_mask"], "задан ••••1a2b");
    assert_eq!(settings["effective_provider"], "openai");
    let first: Value = client
        .post(format!("{base}/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["provider"], "OpenAI-совместимый");

    client
        .put(format!("{base}/settings"))
        .json(&json!({"provider": "mock"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let second: Value = client
        .post(format!("{base}/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["provider"], "Mock");
    let tested: Value = client
        .post(format!("{base}/settings/test"))
        .json(&json!({"provider": "mock"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tested["message"], "Mock готов к работе");
    assert_eq!(Settings::load(&config).unwrap().provider, Provider::Mock);
    task.abort();
}
