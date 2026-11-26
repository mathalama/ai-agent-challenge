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
    settings::{ResolvedSettings, Settings, SettingsPatch, SettingsView},
};
use review::{
    AGENT_VERSION, CheckRegistry, Pipeline, ProgressEvent, ProgressSink, Report, RunContext,
    RunPassport, StageResult, StageTrace,
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub type Answers = BTreeMap<String, String>;

pub use llm::settings::Provider;

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
    no_cache: bool,
    state: Mutex<FactoryState>,
}

struct FactoryState {
    settings: Settings,
    provider: Option<Provider>,
    model: Option<String>,
    base: Option<Arc<dyn LlmProvider>>,
}

impl FactoryState {
    fn resolve(&self) -> Result<ResolvedSettings> {
        let mut resolved = self.settings.resolve(self.provider)?;
        if let Some(model) = &self.model
            && resolved.provider != Provider::Mock
        {
            resolved.model.clone_from(model);
        }
        Ok(resolved)
    }
}

impl ProviderFactory {
    pub fn new(provider: Provider, no_cache: bool) -> Self {
        Self::with_settings(Some(provider), no_cache, Settings::default())
    }

    pub fn with_settings(provider: Option<Provider>, no_cache: bool, settings: Settings) -> Self {
        Self {
            no_cache,
            state: Mutex::new(FactoryState {
                settings,
                provider,
                model: None,
                base: None,
            }),
        }
    }

    pub fn resolved(&self) -> Result<ResolvedSettings> {
        self.state.lock().expect("блокировка провайдера").resolve()
    }

    pub fn view(&self) -> Result<SettingsView> {
        let state = self.state.lock().expect("блокировка провайдера");
        Ok(state.settings.view(state.provider)?)
    }

    pub fn preview(&self, patch: SettingsPatch) -> Result<ResolvedSettings> {
        let state = self.state.lock().expect("блокировка провайдера");
        let provider = match patch.provider {
            Some(provider) => provider,
            None => state.resolve()?.provider,
        };
        Ok(state.settings.preview(patch, provider)?)
    }

    pub fn update(&self, path: &Path, patch: SettingsPatch) -> Result<SettingsView> {
        let mut state = self.state.lock().expect("блокировка провайдера");
        let mut settings = Settings::load(path)?;
        settings.apply(patch)?;
        let view = settings.view(state.provider)?;
        settings.save(path)?;
        state.settings = settings;
        state.base = None;
        Ok(view)
    }

    pub fn reload(&self, settings: Settings) {
        let mut state = self.state.lock().expect("блокировка провайдера");
        state.provider = Some(settings.provider);
        state.settings = settings;
        state.model = None;
        state.base = None;
    }

    pub fn switch_provider(&self, provider: Provider) -> Result<()> {
        let mut state = self.state.lock().expect("блокировка провайдера");
        state.settings.resolve(Some(provider))?;
        state.provider = Some(provider);
        state.model = None;
        state.base = None;
        Ok(())
    }

    pub fn switch_model(&self, model: &str) -> Result<()> {
        let mut state = self.state.lock().expect("блокировка провайдера");
        let mut config = state.resolve()?;
        if config.provider == Provider::Mock {
            return Err("У Mock фиксированная модель".into());
        }
        config.model = model.to_owned();
        config.validate()?;
        state.model = Some(model.to_owned());
        state.base = None;
        Ok(())
    }

    pub fn build(&self, trace: TraceLog, reasoning: bool) -> Result<Box<dyn LlmProvider>> {
        let mut state = self.state.lock().expect("блокировка провайдера");
        let config = state.resolve()?;
        let base = match &state.base {
            Some(base) => Arc::clone(base),
            None => {
                let base: Arc<dyn LlmProvider> = match config.provider {
                    Provider::Mock => Arc::new(MockProvider),
                    Provider::Gemini => Arc::new(RateLimitedProvider::from_env(Box::new(
                        GeminiProvider::from_settings(&config)?,
                    ))?),
                    Provider::Openai => Arc::new(RateLimitedProvider::from_env(Box::new(
                        OpenAiProvider::from_settings(&config)?,
                    ))?),
                };
                state.base = Some(Arc::clone(&base));
                base
            }
        };
        let retry = RetryProvider::from_shared(base)
            .with_trace(trace)
            .with_reasoning(reasoning);
        Ok(if self.no_cache {
            Box::new(retry)
        } else {
            let scope = format!(
                "{:?}|{}|{:?}",
                config.provider, config.base_url, config.response_format
            );
            Box::new(CachedProvider::new(retry, ".cache/llm").with_scope(scope))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_updates_provider_even_when_overridden_by_flag_or_command() {
        let factory = ProviderFactory::new(Provider::Mock, true);
        assert_eq!(factory.resolved().unwrap().provider, Provider::Mock);

        let mut updated = Settings::default();
        updated
            .apply(SettingsPatch {
                provider: Some(Provider::Openai),
                model: Some("test/model".into()),
                ..Default::default()
            })
            .unwrap();
        factory.reload(updated);
        let resolved = factory.resolved().unwrap();
        assert_eq!(resolved.provider, Provider::Openai);
        assert_eq!(resolved.model, "test/model");
    }
}
