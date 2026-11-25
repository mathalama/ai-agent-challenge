use std::{
    borrow::Cow,
    env, fs,
    io::{self, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    sync::Mutex as AsyncMutex,
    time::{Instant, sleep, sleep_until},
};

use crate::{LlmError, LlmProvider, LlmRequest, LlmResponse, Result, Usage};

pub struct CachedProvider {
    inner: RetryProvider,
    directory: PathBuf,
}

impl CachedProvider {
    pub fn new(inner: RetryProvider, directory: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            directory: directory.into(),
        }
    }

    fn entry_path(&self, request: &LlmRequest) -> Result<PathBuf> {
        let serialized = serde_json::to_vec(&(self.model(), request))?;
        let digest = Sha256::digest(serialized);
        let mut key = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            write!(key, "{byte:02x}").expect("запись в строку");
        }
        Ok(self.directory.join(format!("{key}.json")))
    }
}

#[async_trait]
impl LlmProvider for CachedProvider {
    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let request = &self.inner.configure_request(request);
        let started = Instant::now();
        let path = self.entry_path(request)?;
        let validator = response_validator(request)?;
        if let Ok(bytes) = tokio::fs::read(&path).await
            && let Ok(mut cached) = serde_json::from_slice::<LlmResponse>(&bytes)
            && validate_response(&validator, request, &cached).is_ok()
        {
            cached.usage = Usage::default();
            self.inner
                .trace
                .record(request, started, Ok(&cached), &cached.model, true, true);
            return Ok(cached);
        }

        let response = self.inner.generate_validated(request, &validator).await?;
        let bytes = serde_json::to_vec(&response)?;
        let write_result =
            tokio::task::spawn_blocking(move || write_cache_entry(&path, &bytes)).await;
        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "Не удалось записать кэш LLM"),
            Err(error) => tracing::warn!(%error, "Задача записи кэша LLM завершилась с ошибкой"),
        }
        Ok(response)
    }
}

fn response_validator(request: &LlmRequest) -> Result<jsonschema::Validator> {
    jsonschema::validator_for(&request.response_schema).map_err(|_| LlmError::InvalidSchema)
}

fn validate_response(
    validator: &jsonschema::Validator,
    request: &LlmRequest,
    response: &LlmResponse,
) -> Result<()> {
    validator
        .validate(&response.json)
        .map_err(|error| LlmError::SchemaMismatch(error.instance_path().to_string()))?;
    Ok(request.validate_shape(&response.json)?)
}

fn write_cache_entry(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.persist(path)?;
    Ok(())
}

pub struct RetryProvider {
    inner: Box<dyn LlmProvider>,
    trace: TraceLog,
    include_reasoning: bool,
}

impl RetryProvider {
    pub fn new(inner: Box<dyn LlmProvider>) -> Self {
        Self {
            inner,
            trace: TraceLog::default(),
            include_reasoning: false,
        }
    }
    pub fn with_trace(mut self, trace: TraceLog) -> Self {
        self.trace = trace;
        self
    }

    pub fn with_reasoning(mut self, include_reasoning: bool) -> Self {
        self.include_reasoning = include_reasoning;
        self
    }

    fn configure_request<'a>(&self, request: &'a LlmRequest) -> Cow<'a, LlmRequest> {
        if self.include_reasoning && !request.include_reasoning {
            let mut request = request.clone();
            request.include_reasoning = true;
            Cow::Owned(request)
        } else {
            Cow::Borrowed(request)
        }
    }

    async fn generate_validated(
        &self,
        request: &LlmRequest,
        validator: &jsonschema::Validator,
    ) -> Result<LlmResponse> {
        // Перегрузка провайдера (503) обычно длится десятки секунд, поэтому паузы растут до минуты.
        let mut transient_delays = [5, 15, 30, 60].into_iter().map(Duration::from_secs);
        let mut retry_invalid_response = true;
        loop {
            let started = Instant::now();
            let error = match self.inner.generate(request).await {
                Ok(response) => {
                    let validation = validate_response(validator, request, &response);
                    self.trace.record(
                        request,
                        started,
                        Ok(&response),
                        &response.model,
                        validation.is_ok(),
                        false,
                    );
                    match validation {
                        Ok(()) => return Ok(response),
                        Err(error) => error,
                    }
                }
                Err(error) => {
                    self.trace
                        .record(request, started, Err(&error), self.model(), false, false);
                    error
                }
            };
            let (reason, status, delay) = match &error {
                LlmError::Http {
                    status: status @ (429 | 500..=599),
                    retry_after,
                    ..
                } => {
                    let delay = transient_delays.next().map(|default| {
                        retry_after.unwrap_or(if *status == 429 {
                            Duration::from_secs(60)
                        } else {
                            default
                        })
                    });
                    ("http", Some(*status), delay)
                }
                // Обрыв соединения при чтении тела reqwest сообщает как ошибку декодирования.
                LlmError::Transport(error)
                    if error.is_timeout()
                        || error.is_connect()
                        || error.is_request()
                        || error.is_body()
                        || error.is_decode() =>
                {
                    ("transport", None, transient_delays.next())
                }
                LlmError::SchemaMismatch(_) | LlmError::Json(_) | LlmError::ResponseJson { .. }
                    if retry_invalid_response =>
                {
                    retry_invalid_response = false;
                    ("invalid_response", None, Some(Duration::from_millis(250)))
                }
                _ => return Err(error),
            };
            let Some(delay) = delay.filter(|delay| *delay <= Duration::from_secs(120)) else {
                return Err(error);
            };
            tracing::info!(reason, ?status, ?delay, "Повтор запроса LLM");
            self.trace.retry(request, reason, status, delay);
            sleep(delay).await;
        }
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let request = &self.configure_request(request);
        let validator = response_validator(request)?;
        self.generate_validated(request, &validator).await
    }
}

pub struct RateLimitedProvider {
    inner: Box<dyn LlmProvider>,
    interval: Duration,
    next_request: AsyncMutex<Instant>,
}

impl RateLimitedProvider {
    pub fn from_env(inner: Box<dyn LlmProvider>) -> Result<Self> {
        const VARIABLE: &str = "LLM_REQUESTS_PER_MINUTE";
        let rate = match env::var(VARIABLE) {
            Ok(value) => value
                .parse::<NonZeroU32>()
                .map_err(|_| LlmError::Config(VARIABLE))?,
            Err(env::VarError::NotPresent) => NonZeroU32::new(5).unwrap(),
            Err(env::VarError::NotUnicode(_)) => return Err(LlmError::Config(VARIABLE)),
        };
        Ok(Self::new(inner, rate))
    }

    pub fn new(inner: Box<dyn LlmProvider>, requests_per_minute: NonZeroU32) -> Self {
        let nanos = 60_000_000_000_u64.div_ceil(u64::from(requests_per_minute.get()));
        Self {
            inner,
            interval: Duration::from_nanos(nanos),
            next_request: AsyncMutex::new(Instant::now()),
        }
    }
}

#[async_trait]
impl LlmProvider for RateLimitedProvider {
    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        {
            let mut next_request = self.next_request.lock().await;
            sleep_until(*next_request).await;
            *next_request = Instant::now() + self.interval;
        }
        self.inner.generate(request).await
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceEntry {
    stage: String,
    duration_ms: u64,
    usage: Option<Usage>,
    model: String,
    succeeded: bool,
    cached: bool,
    // В выключенном режиме поля нет, во включённом отсутствие текста обозначается null.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Option<String>>,
}

/// Повтор запроса к LLM: причина и пауза перед следующей попыткой.
#[derive(Debug, Clone, Serialize)]
pub struct RetryNotice {
    pub stage: String,
    pub reason: &'static str,
    pub status: Option<u16>,
    pub delay_ms: u64,
}

type RetryListener = Arc<dyn Fn(&RetryNotice) + Send + Sync>;

#[derive(Clone, Default)]
pub struct TraceLog {
    entries: Arc<Mutex<Vec<TraceEntry>>>,
    on_retry: Option<RetryListener>,
}

impl TraceLog {
    /// Вызывает `listener` перед каждой паузой повтора, чтобы показать её пользователю.
    pub fn on_retry(mut self, listener: impl Fn(&RetryNotice) + Send + Sync + 'static) -> Self {
        self.on_retry = Some(Arc::new(listener));
        self
    }

    pub fn entries(&self) -> Vec<TraceEntry> {
        self.entries.lock().expect("блокировка трассы").clone()
    }

    /// Число записанных попыток; используется как отметка начала стадии.
    pub fn entry_count(&self) -> usize {
        self.entries.lock().expect("блокировка трассы").len()
    }

    /// Суммарный расход токенов, число попыток и попаданий в кэш после отметки `start`.
    pub fn usage_since(&self, start: usize) -> (Usage, usize, usize) {
        let entries = self.entries.lock().expect("блокировка трассы");
        let recent = entries.get(start..).unwrap_or_default();
        let usage =
            recent
                .iter()
                .filter_map(|entry| entry.usage)
                .fold(Usage::default(), |total, usage| Usage {
                    input_tokens: total.input_tokens.saturating_add(usage.input_tokens),
                    output_tokens: total.output_tokens.saturating_add(usage.output_tokens),
                });
        let cached = recent.iter().filter(|entry| entry.cached).count();
        (usage, recent.len(), cached)
    }

    fn retry(
        &self,
        request: &LlmRequest,
        reason: &'static str,
        status: Option<u16>,
        delay: Duration,
    ) {
        if let Some(listener) = &self.on_retry {
            listener(&RetryNotice {
                stage: request.stage.clone(),
                reason,
                status,
                delay_ms: delay.as_millis().try_into().unwrap_or(u64::MAX),
            });
        }
    }

    fn record(
        &self,
        request: &LlmRequest,
        started: Instant,
        outcome: std::result::Result<&LlmResponse, &LlmError>,
        model: &str,
        succeeded: bool,
        cached: bool,
    ) {
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let (usage, reasoning) = match outcome {
            Ok(response) => (Some(response.usage), response.reasoning.as_deref()),
            Err(error) => (error.usage(), error.reasoning()),
        };
        tracing::debug!(stage = %request.stage, duration_ms, ?usage, %model, succeeded, cached, "Запрос LLM");
        self.entries
            .lock()
            .expect("блокировка трассы")
            .push(TraceEntry {
                stage: request.stage.clone(),
                duration_ms,
                usage,
                model: model.into(),
                succeeded,
                cached,
                reasoning: request
                    .include_reasoning
                    .then(|| reasoning.map(str::to_owned)),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Replies(Mutex<std::vec::IntoIter<Result<LlmResponse>>>);

    #[async_trait]
    impl LlmProvider for Replies {
        fn model(&self) -> &str {
            "test"
        }

        async fn generate(&self, _: &LlmRequest) -> Result<LlmResponse> {
            self.0.lock().unwrap().next().expect("unexpected request")
        }
    }

    fn reply(json: serde_json::Value) -> LlmResponse {
        LlmResponse {
            json,
            model: "test".into(),
            reasoning: None,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    #[tokio::test]
    async fn reasoning_survives_cache_hits_without_reusing_disabled_entries() {
        let directory = tempfile::tempdir().unwrap();
        let trace = TraceLog::default();
        let mut response = reply(json!(42));
        response.reasoning = Some("Объяснение".into());
        let provider = CachedProvider::new(
            RetryProvider::new(Box::new(Replies(Mutex::new(
                vec![Ok(response)].into_iter(),
            ))))
            .with_trace(trace.clone()),
            directory.path(),
        );
        let mut request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        let path = provider.entry_path(&request).unwrap();
        fs::write(
            path,
            br#"{"json":21,"usage":{"input_tokens":1,"output_tokens":1},"model":"test"}"#,
        )
        .unwrap();
        let legacy = provider.generate(&request).await.unwrap();
        assert_eq!(legacy.json, 21);
        assert_eq!(legacy.reasoning, None);

        request.include_reasoning = true;
        for expected_cached in [false, true] {
            let response = provider.generate(&request).await.unwrap();
            assert_eq!(response.json, 42);
            assert_eq!(response.reasoning.as_deref(), Some("Объяснение"));
            assert_eq!(trace.entries().last().unwrap().cached, expected_cached);
        }
        let entries = serde_json::to_value(trace.entries()).unwrap();
        assert!(entries[0].get("reasoning").is_none());
        assert_eq!(entries[1]["reasoning"], "Объяснение");
        assert_eq!(entries[2]["reasoning"], "Объяснение");
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_responses_never_poison_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let provider = CachedProvider::new(
            RetryProvider::new(Box::new(Replies(Mutex::new(
                vec![
                    Ok(reply(json!(null))),
                    Ok(reply(json!(null))),
                    Ok(reply(json!(42))),
                ]
                .into_iter(),
            )))),
            directory.path(),
        );
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        let path = provider.entry_path(&request).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&reply(json!("old invalid response"))).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            provider.generate(&request).await,
            Err(LlmError::SchemaMismatch(_))
        ));
        let old: LlmResponse = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(old.json, "old invalid response");
        assert_eq!(provider.generate(&request).await.unwrap().json, 42);
        assert_eq!(
            provider
                .generate(&request)
                .await
                .unwrap()
                .usage
                .input_tokens,
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn trace_records_usage_for_failed_attempts_and_zero_for_cache_hits() {
        let directory = tempfile::tempdir().unwrap();
        let trace = TraceLog::default();
        let retry = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![Ok(reply(json!(null))), Ok(reply(json!(42)))].into_iter(),
        ))))
        .with_trace(trace.clone());
        let provider = CachedProvider::new(retry, directory.path());
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();

        assert_eq!(provider.generate(&request).await.unwrap().json, 42);
        assert_eq!(
            provider
                .generate(&request)
                .await
                .unwrap()
                .usage
                .input_tokens,
            0
        );
        let entries = trace.entries();
        assert_eq!(entries.len(), 3);
        assert!(!entries[0].succeeded && !entries[0].cached);
        assert_eq!(entries[0].usage.unwrap().input_tokens, 1);
        assert!(entries[1].succeeded && !entries[1].cached);
        assert_eq!(entries[1].usage.unwrap().input_tokens, 1);
        assert!(entries[2].succeeded && entries[2].cached);
        assert_eq!(entries[2].usage.unwrap().input_tokens, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_json_and_schema_violations_share_one_retry() {
        let bad_json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let trace = TraceLog::default();
        let retry = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![
                Err(LlmError::ResponseJson {
                    reasoning: None,
                    source: bad_json,
                    usage: Some(Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                    }),
                }),
                Ok(reply(json!(null))),
            ]
            .into_iter(),
        ))))
        .with_trace(trace.clone());
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();

        assert!(matches!(
            retry.generate(&request).await,
            Err(LlmError::SchemaMismatch(_))
        ));
        let entries = trace.entries();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| !entry.succeeded));
        assert_eq!(entries[0].usage.unwrap().output_tokens, 2);
    }

    #[tokio::test]
    async fn transport_timeout_is_retried() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let error = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_timeout());
        let retry = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![Err(LlmError::Transport(error)), Ok(reply(json!(42)))].into_iter(),
        ))));
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        assert_eq!(retry.generate(&request).await.unwrap().json, 42);
    }

    #[tokio::test]
    async fn response_body_cut_off_mid_transfer_is_retried() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 1024]).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"choi")
                .unwrap();
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(error.is_decode(), "{error:?}");
        let retry = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![Err(LlmError::Transport(error)), Ok(reply(json!(42)))].into_iter(),
        ))));
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        assert_eq!(retry.generate(&request).await.unwrap().json, 42);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_retries_respect_server_delay_and_wait_a_minute_without_one() {
        let limited = |retry_after| {
            Err(LlmError::Http {
                status: 429,
                message: "quota".into(),
                retry_after,
            })
        };
        let provider = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![
                limited(Some(Duration::from_secs(24))),
                limited(None),
                Ok(reply(json!(42))),
            ]
            .into_iter(),
        ))));
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        let start = Instant::now();
        provider.generate(&request).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(84));
    }

    #[tokio::test(start_paused = true)]
    async fn overloaded_provider_is_retried_with_growing_delays() {
        let overloaded = || {
            Err(LlmError::Http {
                status: 503,
                message: "high demand".into(),
                retry_after: None,
            })
        };
        let provider = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![
                overloaded(),
                overloaded(),
                overloaded(),
                overloaded(),
                Ok(reply(json!(42))),
            ]
            .into_iter(),
        ))));
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        let start = Instant::now();
        assert_eq!(provider.generate(&request).await.unwrap().json, 42);
        assert_eq!(start.elapsed(), Duration::from_secs(5 + 15 + 30 + 60));

        let exhausted = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![
                overloaded(),
                overloaded(),
                overloaded(),
                overloaded(),
                overloaded(),
            ]
            .into_iter(),
        ))));
        assert!(matches!(
            exhausted.generate(&request).await,
            Err(LlmError::Http { status: 503, .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_are_reported_and_usage_is_summed_per_stage() {
        let notices = Arc::new(Mutex::new(Vec::new()));
        let seen = notices.clone();
        let trace = TraceLog::default().on_retry(move |notice| {
            seen.lock().unwrap().push(notice.clone());
        });
        let provider = RetryProvider::new(Box::new(Replies(Mutex::new(
            vec![
                Err(LlmError::Http {
                    status: 503,
                    message: "busy".into(),
                    retry_after: None,
                }),
                Ok(reply(json!(42))),
            ]
            .into_iter(),
        ))))
        .with_trace(trace.clone());
        let request = LlmRequest::typed::<u32>("segment", "", String::new()).unwrap();
        provider.generate(&request).await.unwrap();

        let notices = notices.lock().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            (
                notices[0].stage.as_str(),
                notices[0].reason,
                notices[0].status
            ),
            ("segment", "http", Some(503))
        );
        let (usage, calls, cached) = trace.usage_since(0);
        assert_eq!((usage.input_tokens, calls, cached), (1, 2, 0));
        assert_eq!(trace.usage_since(trace.entry_count()).1, 0);
    }
}
