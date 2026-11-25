use std::{
    collections::BTreeMap,
    error::Error,
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::Local;
use doc::Document;
use llm::{
    CachedProvider, GeminiProvider, LlmProvider, MockProvider, OpenAiProvider, RateLimitedProvider,
    RetryProvider, TraceLog,
};
use review::{
    AGENT_VERSION, CheckRegistry, Pipeline, ProgressEvent, ProgressSink, Report, RunContext,
    RunPassport, StageResult, StageTrace,
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub type Answers = BTreeMap<String, String>;

#[derive(Clone, Copy)]
pub enum Provider {
    Gemini,
    Openai,
    Mock,
}

pub fn build_provider(
    provider: Provider,
    no_cache: bool,
    trace_log: TraceLog,
    reasoning: bool,
) -> Result<Box<dyn LlmProvider>> {
    ProviderFactory::new(provider, no_cache).build(trace_log, reasoning)
}

/// Создаёт базовый провайдер при первом запросе и делит его лимит между операциями.
pub struct ProviderFactory {
    provider: Provider,
    no_cache: bool,
    base: Mutex<Option<Arc<dyn LlmProvider>>>,
}

impl ProviderFactory {
    pub fn new(provider: Provider, no_cache: bool) -> Self {
        Self {
            provider,
            no_cache,
            base: Mutex::new(None),
        }
    }

    pub fn build(&self, trace: TraceLog, reasoning: bool) -> Result<Box<dyn LlmProvider>> {
        let retry = RetryProvider::from_shared(self.base()?)
            .with_trace(trace)
            .with_reasoning(reasoning);
        Ok(if self.no_cache {
            Box::new(retry)
        } else {
            Box::new(CachedProvider::new(retry, ".cache/llm"))
        })
    }

    fn base(&self) -> Result<Arc<dyn LlmProvider>> {
        let mut stored = self.base.lock().expect("блокировка провайдера");
        if let Some(provider) = stored.as_ref() {
            return Ok(Arc::clone(provider));
        }
        let base: Arc<dyn LlmProvider> = match self.provider {
            Provider::Mock => Arc::new(MockProvider),
            Provider::Gemini => Arc::new(RateLimitedProvider::from_env(Box::new(
                GeminiProvider::from_env()?,
            ))?),
            Provider::Openai => Arc::new(RateLimitedProvider::from_env(Box::new(
                OpenAiProvider::from_env()?,
            ))?),
        };
        *stored = Some(Arc::clone(&base));
        Ok(base)
    }
}

/// Сохраняем трассу стадий даже при ошибке конвейера.
pub struct ReviewRun {
    pub report: StageResult<Report>,
    pub stages: Vec<StageTrace>,
}

pub async fn review(
    document: Document,
    provider: &dyn LlmProvider,
    sink: &dyn ProgressSink,
    trace: TraceLog,
    answers: &Answers,
) -> ReviewRun {
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, provider, &checks).with_progress(sink, trace);
    let result = Pipeline::default().run(&mut ctx).await;
    let stages = std::mem::take(&mut ctx.trace);
    let report = result.map(|()| {
        let passport = RunPassport {
            document_title: ctx.document.title.clone(),
            date: Local::now().date_naive().to_string(),
            model: provider.model().into(),
            agent_version: AGENT_VERSION.into(),
        };
        Report::with_answers(ctx, passport, answers)
    });
    ReviewRun { report, stages }
}

pub fn progress_trace(sink: Arc<dyn ProgressSink>) -> TraceLog {
    let retry_sink = Arc::clone(&sink);
    TraceLog::default()
        .on_retry(move |notice| retry_sink.emit(&notice.into()))
        .on_reasoning(move |stage, text, delta| {
            sink.emit(&if delta {
                ProgressEvent::LlmReasoningDelta {
                    stage: stage.into(),
                    delta: text.into(),
                }
            } else {
                ProgressEvent::LlmReasoning {
                    stage: stage.into(),
                    text: text.into(),
                }
            });
        })
}

/// Читает сохранённые ответы для файловых клиентов.
pub fn read_answers(path: &Path) -> Result<Answers> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            format!("Некорректный файл ответов «{}»: {error}", path.display()).into()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Answers::new()),
        Err(error) => Err(format!("Не удалось прочитать «{}»: {error}", path.display()).into()),
    }
}
