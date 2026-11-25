use std::env;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{LlmError, LlmProvider, LlmRequest, LlmResponse, Result, Usage};

pub struct GeminiProvider {
    _api_key: String,
    model: String,
}

impl GeminiProvider {
    pub fn from_env() -> Result<Self> {
        let read = |name| {
            env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or(LlmError::Config(name))
        };
        Ok(Self {
            _api_key: read("GEMINI_API_KEY")?,
            model: read("GEMINI_MODEL")?,
        })
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn generate(&self, _request: &LlmRequest) -> Result<LlmResponse> {
        // TODO(issue): вызвать Gemini API со схемой ответа, разобрать JSON и usage.
        todo!("HTTP-клиент Gemini")
    }
}

pub struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model(&self) -> &str {
        "mock"
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let json = match request.stage.as_str() {
            "segment" => {
                let candidate: Option<Value> = serde_json::from_str(&request.user_text)?;
                json!(candidate.into_iter().collect::<Vec<_>>())
            }
            "facts" | "rules" | "requirement_checks" | "set_checks" | "consistency"
            | "ambiguity" | "verify" | "clarify" => json!([]),
            _ => return Err(LlmError::UnknownStage(request.stage.clone())),
        };
        let count = |text: &str| text.split_whitespace().count() as u64;
        Ok(LlmResponse {
            usage: Usage {
                input_tokens: count(&request.system_prompt) + count(&request.user_text),
                output_tokens: count(&json.to_string()),
            },
            json,
            model: self.model().into(),
        })
    }
}
