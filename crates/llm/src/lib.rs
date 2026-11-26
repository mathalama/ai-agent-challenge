mod decorators;
mod mock_review;
mod openai;
pub mod openrouter;
mod providers;
pub mod settings;

pub use decorators::{
    CachedProvider, RateLimitedProvider, RetryNotice, RetryProvider, TraceEntry, TraceLog,
};
pub use openai::OpenAiProvider;
pub use providers::{GeminiProvider, MockProvider};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct LlmRequest {
    pub stage: String,
    pub system_prompt: String,
    pub user_text: String,
    pub response_schema: Value,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub include_reasoning: bool,
    #[serde(skip)]
    validate_shape: fn(&Value) -> serde_json::Result<()>,
}

impl LlmRequest {
    pub fn typed<T: JsonSchema + DeserializeOwned>(
        stage: &str,
        system_prompt: &str,
        user_text: String,
    ) -> Result<Self> {
        Ok(Self {
            stage: stage.into(),
            system_prompt: system_prompt.into(),
            user_text,
            response_schema: serde_json::to_value(schemars::schema_for!(T))?,
            include_reasoning: false,
            validate_shape: |value| T::deserialize(value).map(drop),
        })
    }

    pub(crate) fn validate_shape(&self, value: &Value) -> serde_json::Result<()> {
        (self.validate_shape)(value)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LlmResponse {
    pub json: Value,
    pub usage: Usage,
    pub model: String,
    pub reasoning: Option<String>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("Не задана или некорректна переменная {0}")]
    Config(&'static str),
    #[error("Провайдер вернул HTTP {status}: {message}")]
    Http {
        status: u16,
        message: String,
        retry_after: Option<std::time::Duration>,
    },
    #[error("{0}")]
    Settings(&'static str),
    #[error("Исчерпан дневной лимит бесплатных моделей OpenRouter, сброс в 00:00 UTC")]
    OpenRouterDailyLimit,
    #[error("Ошибка запроса к LLM: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Поток ответа LLM прервался до результата")]
    IncompleteStream,
    #[error(
        "Ответ LLM не содержит полного результата (finishReason={finish_reason}, promptFeedback.blockReason={block_reason})"
    )]
    MissingCandidate {
        finish_reason: String,
        block_reason: String,
        usage: Option<Usage>,
        reasoning: Option<String>,
    },
    #[error("Ошибка обработки JSON ответа LLM: {source}")]
    ResponseJson {
        #[source]
        source: serde_json::Error,
        usage: Option<Usage>,
        reasoning: Option<String>,
    },
    #[error("Некорректная схема ответа LLM")]
    InvalidSchema,
    #[error("Ответ LLM не соответствует схеме (поле «{0}»)")]
    SchemaMismatch(String),
    #[error("Ошибка обработки JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Mock не поддерживает стадию «{0}»")]
    UnknownStage(String),
}

impl LlmError {
    pub fn is_output_limit(&self) -> bool {
        matches!(self, Self::MissingCandidate { finish_reason, .. }
            if matches!(finish_reason.as_str(), "length" | "MAX_TOKENS"))
    }

    pub(crate) fn usage(&self) -> Option<Usage> {
        match self {
            Self::MissingCandidate { usage, .. } | Self::ResponseJson { usage, .. } => *usage,
            _ => None,
        }
    }

    pub(crate) fn reasoning(&self) -> Option<&str> {
        match self {
            Self::MissingCandidate { reasoning, .. } | Self::ResponseJson { reasoning, .. } => {
                reasoning.as_deref()
            }
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;
pub type ReasoningCallback<'a> = dyn Fn(&str) + Send + Sync + 'a;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn model(&self) -> &str;
    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse>;

    /// Провайдеры со стримингом могут передавать рассуждение до готовности ответа.
    async fn generate_with_reasoning(
        &self,
        request: &LlmRequest,
        _on_delta: &ReasoningCallback<'_>,
    ) -> Result<LlmResponse> {
        self.generate(request).await
    }
}

pub async fn generate_typed<T: JsonSchema + DeserializeOwned>(
    provider: &dyn LlmProvider,
    stage: &str,
    system_prompt: &str,
    user_text: String,
) -> Result<T> {
    let request = LlmRequest::typed::<T>(stage, system_prompt, user_text)?;
    let response = provider.generate(&request).await?;
    Ok(serde_json::from_value(response.json)?)
}

/// Проверяет OpenRouter по статусу ключа; у остальных провайдеров запрашивает короткий ответ.
pub async fn test_settings(config: &settings::ResolvedSettings) -> Result<String> {
    use settings::Provider;
    if config.provider == Provider::Openai && openrouter::is_openrouter(&config.base_url) {
        let key = config
            .api_key
            .as_deref()
            .ok_or(LlmError::Settings("Укажите ключ OpenRouter"))?;
        return Ok(openrouter::key_status(key).await?.summary());
    }
    let provider: Box<dyn LlmProvider> = match config.provider {
        Provider::Mock => return Ok("Mock готов к работе".into()),
        Provider::Gemini => Box::new(GeminiProvider::from_settings(config)?),
        Provider::Openai => Box::new(OpenAiProvider::from_settings(config)?),
    };
    #[derive(Deserialize, JsonSchema)]
    struct Connection {
        ok: bool,
    }
    let request = LlmRequest::typed::<Connection>(
        "settings_test",
        "Верни объект JSON с полем ok = true.",
        "Проверка соединения.".into(),
    )?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        provider.generate(&request),
    )
    .await
    .map_err(|_| LlmError::Settings("Провайдер не ответил за 20 секунд"))?
    .map_err(|_| {
        LlmError::Settings("Не удалось проверить провайдер. Проверьте адрес, модель и ключ")
    })?;
    if !serde_json::from_value::<Connection>(result.json)?.ok {
        return Err(LlmError::Settings("Провайдер вернул неожиданный ответ"));
    }
    Ok("Соединение установлено".into())
}
