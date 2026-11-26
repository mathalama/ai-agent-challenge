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

/// Проверяет OpenRouter по статусу ключа и каталогу моделей; у остальных провайдеров запрашивает короткий ответ.
pub async fn test_settings(config: &settings::ResolvedSettings) -> Result<String> {
    use settings::Provider;
    if config.provider == Provider::Mock {
        return Ok("Mock готов к работе".into());
    }
    config.validate()?;
    if config.provider == Provider::Openai && openrouter::is_openrouter(&config.base_url) {
        let key = config
            .api_key
            .as_deref()
            .ok_or(LlmError::Settings("Укажите ключ OpenRouter"))?;
        let (status, catalog) =
            tokio::join!(openrouter::key_status(key), openrouter::models(false));
        let status = status?;
        if let Ok(models) = catalog
            && !models.is_empty()
            && !models.iter().any(|model| model.id == config.model)
        {
            return Err(LlmError::Settings(
                "Модель не найдена в каталоге OpenRouter",
            ));
        }
        return Ok(status.summary());
    }
    let provider: Box<dyn LlmProvider> = match config.provider {
        Provider::Mock => unreachable!(),
        Provider::Gemini => {
            Box::new(GeminiProvider::from_settings(config).map_err(map_test_error)?)
        }
        Provider::Openai => {
            Box::new(OpenAiProvider::from_settings(config).map_err(map_test_error)?)
        }
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
    .map_err(map_test_error)?;
    if !serde_json::from_value::<Connection>(result.json)
        .map_err(|_| {
            LlmError::Settings(
                "Модель ответила в неожиданном формате. Попробуйте другой формат ответа",
            )
        })?
        .ok
    {
        return Err(LlmError::Settings("Провайдер вернул неожиданный ответ"));
    }
    Ok("Соединение установлено".into())
}

fn map_test_error(error: LlmError) -> LlmError {
    match error {
        LlmError::Settings(message) => LlmError::Settings(message),
        LlmError::Config(_) => LlmError::Settings("Укажите API-ключ"),
        LlmError::OpenRouterDailyLimit => LlmError::Settings(
            "Исчерпан дневной лимит бесплатных моделей OpenRouter, сброс в 00:00 UTC",
        ),
        LlmError::Http {
            status: 401 | 403, ..
        } => LlmError::Settings("Неверный API-ключ или нет доступа (HTTP 401/403)"),
        LlmError::Http { status: 404, .. } => {
            LlmError::Settings("Модель или адрес API не найдены (HTTP 404)")
        }
        LlmError::Http { status: 400, .. } => LlmError::Settings(
            "Провайдер отклонил запрос (HTTP 400). Проверьте ID модели и формат ответа",
        ),
        LlmError::Http { status: 429, .. } => {
            LlmError::Settings("Превышен лимит запросов провайдера (HTTP 429)")
        }
        LlmError::Http {
            status: 500..=599, ..
        } => LlmError::Settings("Сервер провайдера временно недоступен (HTTP 5xx)"),
        LlmError::Transport(_) => {
            LlmError::Settings("Не удалось подключиться к адресу API. Проверьте URL и сеть")
        }
        LlmError::ResponseJson { .. }
        | LlmError::SchemaMismatch(_)
        | LlmError::MissingCandidate { .. } => LlmError::Settings(
            "Модель ответила в неожиданном формате. Попробуйте другой формат ответа",
        ),
        _ => LlmError::Settings("Не удалось проверить провайдер. Проверьте адрес, модель и ключ"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_distinguishes_http_and_config_errors() {
        let msg = |err: LlmError| match map_test_error(err) {
            LlmError::Settings(text) => text,
            other => panic!("unexpected error: {other}"),
        };
        assert!(msg(LlmError::Config("OPENAI_API_KEY")).contains("API-ключ"));
        assert!(
            msg(LlmError::Http {
                status: 401,
                message: "Unauthorized".into(),
                retry_after: None,
            })
            .contains("401")
        );
        assert!(
            msg(LlmError::Http {
                status: 404,
                message: "Not Found".into(),
                retry_after: None,
            })
            .contains("404")
        );
        assert!(
            msg(LlmError::Http {
                status: 400,
                message: "Bad Request".into(),
                retry_after: None,
            })
            .contains("400")
        );
        assert!(
            msg(LlmError::Http {
                status: 429,
                message: "Too Many Requests".into(),
                retry_after: None,
            })
            .contains("429")
        );
    }
}
