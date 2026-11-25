mod decorators;
mod mock_review;
mod openai;
mod providers;

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

#[derive(Debug, Serialize)]
pub struct LlmRequest {
    pub stage: String,
    pub system_prompt: String,
    pub user_text: String,
    pub response_schema: Value,
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
    #[error("Ошибка запроса к LLM: {0}")]
    Transport(#[from] reqwest::Error),
    #[error(
        "Ответ LLM не содержит полного результата (finishReason={finish_reason}, promptFeedback.blockReason={block_reason})"
    )]
    MissingCandidate {
        finish_reason: String,
        block_reason: String,
        usage: Option<Usage>,
    },
    #[error("Ошибка обработки JSON ответа LLM: {source}")]
    ResponseJson {
        #[source]
        source: serde_json::Error,
        usage: Option<Usage>,
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
    pub(crate) fn usage(&self) -> Option<Usage> {
        match self {
            Self::MissingCandidate { usage, .. } | Self::ResponseJson { usage, .. } => *usage,
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn model(&self) -> &str;
    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse>;
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
