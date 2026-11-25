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
        if let Ok(bytes) = tokio::fs::read(&path).await
            && let Ok(mut cached) = serde_json::from_slice::<LlmResponse>(&bytes)
        {
            cached.usage = Usage::default();
            return Ok(cached);
        }

        let response = self.inner.generate(request).await?;
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
        for delay in [250, 500, 1000] {
            match self.inner.generate(request).await {
                Err(LlmError::Http {
                    status: 429 | 500..=599,
                    ..
                }) => {
                    sleep(Duration::from_millis(delay)).await;
                }
                result => return result,
            }
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
            Err(env::VarError::NotPresent) => NonZeroU32::new(60).unwrap(),
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
