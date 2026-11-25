use std::{
    env,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use doc::{Block, BlockContent};
use reqwest::{
    Client,
    header::{HeaderValue, RETRY_AFTER},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{LlmError, LlmProvider, LlmRequest, LlmResponse, Result, Usage};

pub struct GeminiProvider {
    api_key: HeaderValue,
    model: String,
    client: Client,
    endpoint: String,
}

impl GeminiProvider {
    pub fn from_env() -> Result<Self> {
        const KEY: &str = "GEMINI_API_KEY";
        const MODEL: &str = "GEMINI_MODEL";
        let key = env::var(KEY).map_err(|_| LlmError::Config(KEY))?;
        let mut api_key: HeaderValue = key.trim().parse().map_err(|_| LlmError::Config(KEY))?;
        if api_key.is_empty() {
            return Err(LlmError::Config(KEY));
        }
        api_key.set_sensitive(true);
        let configured = match env::var(MODEL) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => String::new(),
            Err(_) => return Err(LlmError::Config(MODEL)),
        };
        let model = match configured.trim() {
            "" => "gemini-3.1-flash-lite",
            name => name.strip_prefix("models/").unwrap_or(name),
        };
        if model.is_empty()
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        {
            return Err(LlmError::Config(MODEL));
        }
        Ok(Self {
            api_key,
            model: model.into(),
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .build()?,
            endpoint: format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
            ),
        })
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let mut schema = request.response_schema.clone();
        if let Some(object) = schema.as_object_mut() {
            object.remove("$schema");
        }
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-goog-api-key", self.api_key.clone())
            .json(&json!({
                "systemInstruction": {"parts": [{"text": request.system_prompt}]},
                "contents": [{"role": "user", "parts": [{"text": request.user_text}]}],
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseJsonSchema": schema
                },
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let header_delay = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(retry_after_header);
            let body = response.json::<Value>().await.unwrap_or_default();
            let retry_after = header_delay.into_iter().chain(retry_info(&body)).max();
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("Ошибка Gemini"));
            return Err(LlmError::Http {
                status: status.as_u16(),
                message: message.replace(self.api_key.to_str().unwrap_or_default(), "[скрыто]"),
                retry_after,
            });
        }
        let body = serde_json::from_slice(&response.bytes().await?)?;
        decode_gemini_response(&body, self.model())
    }
}

fn decode_gemini_response(body: &Value, model: &str) -> Result<LlmResponse> {
    let candidate = &body["candidates"][0];
    let finish_reason = candidate["finishReason"].as_str();
    let block_reason = body["promptFeedback"]["blockReason"].as_str();
    let usage = body.get("usageMetadata").map(|metadata| Usage {
        input_tokens: metadata["promptTokenCount"].as_u64().unwrap_or(0),
        output_tokens: metadata["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(metadata["thoughtsTokenCount"].as_u64().unwrap_or(0)),
    });
    let missing_candidate = || LlmError::MissingCandidate {
        finish_reason: finish_reason.unwrap_or("—").into(),
        block_reason: block_reason.unwrap_or("—").into(),
        usage,
    };
    let parts = candidate["content"]["parts"]
        .as_array()
        .filter(|_| finish_reason == Some("STOP"))
        .ok_or_else(missing_candidate)?;
    let text: String = parts
        .iter()
        .filter(|part| part["thought"] != true)
        .filter_map(|part| part["text"].as_str())
        .collect();
    Ok(LlmResponse {
        json: serde_json::from_str(&text)
            .map_err(|source| LlmError::ResponseJson { source, usage })?,
        usage: usage.unwrap_or_default(),
        model: body["modelVersion"].as_str().unwrap_or(model).into(),
    })
}

fn retry_after_header(value: &HeaderValue) -> Option<Duration> {
    let value = value.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(date.duration_since(SystemTime::now()).unwrap_or_default())
}

fn retry_info(body: &Value) -> Option<Duration> {
    body["error"]["details"]
        .as_array()?
        .iter()
        .filter(|detail| detail["@type"] == "type.googleapis.com/google.rpc.RetryInfo")
        .filter_map(|detail| {
            let seconds = detail["retryDelay"]
                .as_str()?
                .strip_suffix('s')?
                .parse()
                .ok()?;
            Duration::try_from_secs_f64(seconds).ok()
        })
        .max()
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
                #[derive(Deserialize)]
                struct SegmentInput {
                    blocks: Vec<Block>,
                }
                let input: SegmentInput = serde_json::from_str(&request.user_text)?;
                json!(
                    mock_candidate(&input.blocks)
                        .into_iter()
                        .collect::<Vec<_>>()
                )
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

fn mock_candidate(blocks: &[Block]) -> Option<Value> {
    let (block, source_id, body, quote) = blocks
        .iter()
        .find_map(|block| {
            let BlockContent::ListItem { text } = &block.content else {
                return None;
            };
            let (id, body) = text.split_once(". ")?;
            let (prefix, number) = id.split_once('-')?;
            if prefix.is_empty()
                || !prefix.chars().all(char::is_alphabetic)
                || number.is_empty()
                || !number.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some((block, Some(id), body, text.as_str()))
        })
        .or_else(|| {
            blocks.iter().find_map(|block| match &block.content {
                BlockContent::Paragraph { text } | BlockContent::ListItem { text }
                    if !text.trim().is_empty() =>
                {
                    Some((block, None, text.as_str(), text.as_str()))
                }
                _ => None,
            })
        })?;
    Some(json!({
        "id": "R-001",
        "source_id": source_id,
        "text": body,
        "quote": quote,
        "location": block.location,
        "part": null,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_gemini_response_reports_finish_and_block_reasons() {
        for (body, expected) in [
            (
                json!({"candidates": [{"finishReason": "MAX_TOKENS"}]}),
                "finishReason=MAX_TOKENS, promptFeedback.blockReason=—",
            ),
            (
                json!({"promptFeedback": {"blockReason": "SAFETY"}}),
                "finishReason=—, promptFeedback.blockReason=SAFETY",
            ),
        ] {
            let Err(error) = decode_gemini_response(&body, "test") else {
                panic!("Неполный ответ Gemini принят");
            };
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn broken_candidate_json_retains_gemini_usage() {
        let body = json!({
            "candidates": [{"finishReason": "STOP", "content": {"parts": [{"text": "{"}]}}],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 2, "thoughtsTokenCount": 3}
        });
        let Err(error) = decode_gemini_response(&body, "test") else {
            panic!("Некорректный JSON Gemini принят");
        };
        let usage = error.usage().unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (5, 5));
        assert!(matches!(error, LlmError::ResponseJson { .. }));
    }

    #[test]
    fn gemini_retry_delays_support_http_headers_and_rpc_details() {
        assert_eq!(
            retry_after_header(&HeaderValue::from_static("24")),
            Some(Duration::from_secs(24))
        );
        let body = json!({"error": {"details": [
            {"@type": "type.googleapis.com/google.rpc.QuotaFailure"},
            {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "37.5s"}
        ]}});
        assert_eq!(retry_info(&body), Some(Duration::from_millis(37_500)));
    }
}
