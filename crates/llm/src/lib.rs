mod decorators;
mod providers;

pub use decorators::{
    CachedProvider, RateLimitedProvider, RetryProvider, TraceEntry, TraceLog, TracingProvider,
};
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
    Http { status: u16, message: String },
    #[error("Ошибка обработки JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Mock не поддерживает стадию «{0}»")]
    UnknownStage(String),
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
    let request = LlmRequest {
        stage: stage.into(),
        system_prompt: system_prompt.into(),
        user_text,
        response_schema: serde_json::to_value(schemars::schema_for!(T))?,
    };
    let response = provider.generate(&request).await?;
    Ok(serde_json::from_value(response.json)?)
}
