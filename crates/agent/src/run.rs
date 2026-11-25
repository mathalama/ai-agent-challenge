use std::{collections::BTreeMap, error::Error, fs, io, path::Path, sync::Arc};

use chrono::Local;
use doc::Document;
use llm::{
    CachedProvider, GeminiProvider, LlmProvider, MockProvider, OpenAiProvider, RateLimitedProvider,
    RetryProvider, TraceLog,
};
use review::{
    AGENT_VERSION, CheckRegistry, Pipeline, ProgressSink, Report, RunContext, RunPassport,
    StageResult, StageTrace,
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
    let base: Box<dyn LlmProvider> = match provider {
        Provider::Mock => Box::new(MockProvider),
        Provider::Gemini => Box::new(RateLimitedProvider::from_env(Box::new(
            GeminiProvider::from_env()?,
        ))?),
        Provider::Openai => Box::new(RateLimitedProvider::from_env(Box::new(
            OpenAiProvider::from_env()?,
        ))?),
    };
    let retry = RetryProvider::new(base)
        .with_trace(trace_log)
        .with_reasoning(reasoning);
    Ok(if no_cache {
        Box::new(retry)
    } else {
        Box::new(CachedProvider::new(retry, ".cache/llm"))
    })
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
    TraceLog::default().on_retry(move |notice| sink.emit(&notice.into()))
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
