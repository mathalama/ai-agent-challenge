use std::{
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
    inner: Box<dyn LlmProvider>,
    directory: PathBuf,
}

impl CachedProvider {
    pub fn new(inner: Box<dyn LlmProvider>, directory: impl Into<PathBuf>) -> Self {
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
        let path = self.entry_path(request)?;
        let validator = jsonschema::validator_for(&request.response_schema)
            .map_err(|_| LlmError::InvalidSchema)?;
        if let Ok(bytes) = tokio::fs::read(&path).await
            && let Ok(mut cached) = serde_json::from_slice::<LlmResponse>(&bytes)
            && validator.is_valid(&cached.json)
            && request.validate_shape(&cached.json).is_ok()
        {
            cached.usage = Usage::default();
            return Ok(cached);
        }

        let response = self.inner.generate(request).await?;
        validator
            .validate(&response.json)
            .map_err(|error| LlmError::SchemaMismatch(error.instance_path().to_string()))?;
        request.validate_shape(&response.json)?;
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
}

impl RetryProvider {
    pub fn new(inner: Box<dyn LlmProvider>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        for server_error_delay_ms in [250, 500, 1000] {
            let result = self.inner.generate(request).await;
            let Err(LlmError::Http {
                status: status @ (429 | 500..=599),
                retry_after,
                ..
            }) = &result
            else {
                return result;
            };
            let delay = retry_after.unwrap_or_else(|| {
                if *status == 429 {
                    Duration::from_secs(60)
                } else {
                    Duration::from_millis(server_error_delay_ms)
                }
            });
            if delay > Duration::from_secs(120) {
                return result;
            }
            tracing::warn!(status, ?delay, "Повтор запроса LLM");
            sleep(delay).await;
        }
        self.inner.generate(request).await
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
}

#[derive(Clone, Default)]
pub struct TraceLog(Arc<Mutex<Vec<TraceEntry>>>);

impl TraceLog {
    pub fn entries(&self) -> Vec<TraceEntry> {
        self.0.lock().expect("блокировка трассы").clone()
    }
}

pub struct TracingProvider {
    inner: Box<dyn LlmProvider>,
    trace: TraceLog,
}

impl TracingProvider {
    pub fn new(inner: Box<dyn LlmProvider>, trace: TraceLog) -> Self {
        Self { inner, trace }
    }
}

#[async_trait]
impl LlmProvider for TracingProvider {
    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let started = Instant::now();
        let result = self.inner.generate(request).await;
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let usage = result.as_ref().ok().map(|response| response.usage);
        let model = result.as_ref().map_or_else(
            |_| self.model().to_owned(),
            |response| response.model.clone(),
        );
        tracing::debug!(stage = %request.stage, duration_ms, ?usage, %model, "Запрос LLM");
        self.trace
            .0
            .lock()
            .expect("блокировка трассы")
            .push(TraceEntry {
                stage: request.stage.clone(),
                duration_ms,
                usage,
                model,
            });
        result
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
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    #[tokio::test]
    async fn invalid_responses_never_poison_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let provider = CachedProvider::new(
            Box::new(Replies(Mutex::new(
                vec![Ok(reply(json!(null))), Ok(reply(json!(42)))].into_iter(),
            ))),
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
}
