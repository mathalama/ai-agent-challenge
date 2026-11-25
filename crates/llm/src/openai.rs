//! Провайдер для API, совместимых с OpenAI Chat Completions: OpenAI, GitHub Models,
//! OpenRouter, Groq, DeepSeek и другие. Отличаются они только адресом, ключом и моделью.

use std::{env, num::NonZeroU32, time::Duration};

use async_trait::async_trait;
use reqwest::{
    Client,
    header::{HeaderValue, RETRY_AFTER},
};
use serde_json::{Map, Value, json};

use crate::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, Result, Usage, providers::retry_after_header,
};

const MAX_TOKENS_VAR: &str = "OPENAI_MAX_TOKENS";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Ключ, под которым лежит ответ модели в обёртке схемы.
const RESULT_FIELD: &str = "result";

/// Как просить у сервиса JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseFormat {
    /// Строгая схема в `response_format` (OpenAI, GitHub Models, Groq, OpenRouter).
    JsonSchema,
    /// Только `json_object`, схема передаётся в промпте (например, DeepSeek).
    JsonObject,
}

pub struct OpenAiProvider {
    api_key: HeaderValue,
    model: String,
    endpoint: String,
    format: ResponseFormat,
    /// `reasoning_effort` для рассуждающих моделей; `none` отключает рассуждения у Ollama.
    reasoning_effort: Option<String>,
    max_tokens: Option<NonZeroU32>,
    client: Client,
}

impl OpenAiProvider {
    pub fn from_env() -> Result<Self> {
        const KEY: &str = "OPENAI_API_KEY";
        const MODEL: &str = "OPENAI_MODEL";
        const BASE_URL: &str = "OPENAI_BASE_URL";
        const FORMAT: &str = "OPENAI_RESPONSE_FORMAT";
        const REASONING: &str = "OPENAI_REASONING_EFFORT";
        const TIMEOUT: &str = "OPENAI_TIMEOUT_SECS";

        let key = env::var(KEY).map_err(|_| LlmError::Config(KEY))?;
        let mut api_key: HeaderValue = format!("Bearer {}", key.trim())
            .parse()
            .map_err(|_| LlmError::Config(KEY))?;
        if key.trim().is_empty() {
            return Err(LlmError::Config(KEY));
        }
        api_key.set_sensitive(true);

        let model = env::var(MODEL).map_err(|_| LlmError::Config(MODEL))?;
        let model = model.trim();
        if model.is_empty() || model.chars().any(char::is_whitespace) {
            return Err(LlmError::Config(MODEL));
        }

        let base_url = optional_var(BASE_URL)?.unwrap_or_else(|| DEFAULT_BASE_URL.into());
        let base_url = base_url.trim().trim_end_matches('/');
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(LlmError::Config(BASE_URL));
        }

        let format = match optional_var(FORMAT)?.as_deref().map(str::trim) {
            None | Some("" | "json_schema") => ResponseFormat::JsonSchema,
            Some("json_object") => ResponseFormat::JsonObject,
            Some(_) => return Err(LlmError::Config(FORMAT)),
        };
        let reasoning_effort = optional_var(REASONING)?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if reasoning_effort
            .as_deref()
            .is_some_and(|value| !value.bytes().all(|byte| byte.is_ascii_alphabetic()))
        {
            return Err(LlmError::Config(REASONING));
        }

        // Локальным моделям на слабой видеокарте трёх минут на большой запрос не хватает.
        let timeout = match optional_var(TIMEOUT)?.as_deref().map(str::trim) {
            None | Some("") => 180,
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|seconds| (1..=3600).contains(seconds))
                .ok_or(LlmError::Config(TIMEOUT))?,
        };

        Ok(Self {
            api_key,
            model: model.into(),
            endpoint: format!("{base_url}/chat/completions"),
            format,
            reasoning_effort,
            max_tokens: parse_max_tokens(optional_var(MAX_TOKENS_VAR)?.as_deref())?,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(timeout))
                .build()?,
        })
    }

    fn request_body(&self, request: &LlmRequest) -> Value {
        let schema = wrap_schema(&request.response_schema);
        let (system_prompt, response_format) = match self.format {
            ResponseFormat::JsonSchema => (
                request.system_prompt.clone(),
                json!({
                    "type": "json_schema",
                    "json_schema": {"name": "response", "schema": schema, "strict": false}
                }),
            ),
            ResponseFormat::JsonObject => (
                format!(
                    "{}\n\nВерни только JSON-объект, который соответствует JSON Schema:\n{schema}",
                    request.system_prompt
                ),
                json!({"type": "json_object"}),
            ),
        };
        let mut body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": request.user_text},
            ],
            "response_format": response_format,
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = json!(max_tokens.get());
        }
        body
    }
}

fn parse_max_tokens(value: Option<&str>) -> Result<Option<NonZeroU32>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().map_err(|_| LlmError::Config(MAX_TOKENS_VAR)))
        .transpose()
}

fn optional_var(name: &'static str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(LlmError::Config(name)),
    }
}

/// Сервисы принимают только схему-объект, а стадии часто ждут массив.
/// Поэтому ответ кладётся в поле `result`, а определения поднимаются в корень обёртки,
/// чтобы ссылки `#/$defs/...` продолжали работать.
fn wrap_schema(schema: &Value) -> Value {
    let mut inner = schema.clone();
    let mut wrapper = Map::new();
    if let Some(object) = inner.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
        for key in ["$defs", "definitions"] {
            if let Some(definitions) = object.remove(key) {
                wrapper.insert(key.into(), definitions);
            }
        }
    }
    wrapper.insert("type".into(), json!("object"));
    wrapper.insert("properties".into(), json!({ RESULT_FIELD: inner }));
    wrapper.insert("required".into(), json!([RESULT_FIELD]));
    wrapper.insert("additionalProperties".into(), json!(false));
    Value::Object(wrapper)
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", self.api_key.clone())
            .json(&self.request_body(request))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(retry_after_header);
            let body = response.json::<Value>().await.unwrap_or_default();
            let message = body["error"]["message"]
                .as_str()
                .or_else(|| body["message"].as_str())
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("Ошибка провайдера"));
            let secret = self.api_key.to_str().unwrap_or_default();
            let secret = secret.strip_prefix("Bearer ").unwrap_or(secret);
            return Err(LlmError::Http {
                status: status.as_u16(),
                message: message.replace(secret, "[скрыто]"),
                retry_after,
            });
        }
        let body = serde_json::from_slice(&response.bytes().await?)?;
        decode_chat_response(&body, self.model())
    }
}

fn decode_chat_response(body: &Value, model: &str) -> Result<LlmResponse> {
    let choice = &body["choices"][0];
    let finish_reason = choice["finish_reason"].as_str();
    let usage = body.get("usage").map(|usage| Usage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
    });
    let content = choice["message"]["content"]
        .as_str()
        .filter(|_| finish_reason == Some("stop"))
        .ok_or_else(|| LlmError::MissingCandidate {
            finish_reason: finish_reason.unwrap_or("—").into(),
            block_reason: "—".into(),
            usage,
        })?;
    let mut json: Value = serde_json::from_str(strip_code_fence(content))
        .map_err(|source| LlmError::ResponseJson { source, usage })?;
    // Модель иногда отвечает без обёртки; тогда ответ проверит валидатор схемы.
    if let Some(result) = json.as_object_mut().and_then(|object| {
        (object.len() == 1)
            .then(|| object.remove(RESULT_FIELD))
            .flatten()
    }) {
        json = result;
    }
    Ok(LlmResponse {
        json,
        usage: usage.unwrap_or_default(),
        model: body["model"].as_str().unwrap_or(model).into(),
    })
}

/// Часть бесплатных моделей оборачивает JSON в блок кода Markdown.
fn strip_code_fence(content: &str) -> &str {
    let trimmed = content.trim();
    trimmed
        .strip_prefix("```")
        .and_then(|rest| rest.strip_suffix("```"))
        .map(|rest| rest.strip_prefix("json").unwrap_or(rest).trim())
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(format: ResponseFormat) -> OpenAiProvider {
        OpenAiProvider {
            api_key: HeaderValue::from_static("Bearer test"),
            model: "test-model".into(),
            endpoint: format!("{DEFAULT_BASE_URL}/chat/completions"),
            format,
            reasoning_effort: None,
            max_tokens: None,
            client: Client::new(),
        }
    }

    #[test]
    fn array_schema_is_wrapped_into_object_with_root_definitions() {
        #[derive(schemars::JsonSchema, serde::Deserialize)]
        #[allow(dead_code)]
        struct Item {
            kind: Kind,
        }
        #[derive(schemars::JsonSchema, serde::Deserialize)]
        #[allow(dead_code)]
        enum Kind {
            A,
            B,
        }
        let request = LlmRequest::typed::<Vec<Item>>("test", "", String::new()).unwrap();
        let wrapped = wrap_schema(&request.response_schema);

        assert_eq!(wrapped["type"], "object");
        assert_eq!(wrapped["properties"]["result"]["type"], "array");
        assert!(wrapped["properties"]["result"].get("$schema").is_none());
        assert!(wrapped["$defs"]["Kind"].is_object());
        let validator = jsonschema::validator_for(&wrapped).unwrap();
        assert!(validator.is_valid(&json!({"result": [{"kind": "A"}]})));
        assert!(!validator.is_valid(&json!({"result": [{"kind": "C"}]})));
    }

    #[test]
    fn json_object_mode_puts_schema_into_system_prompt() {
        let request = LlmRequest::typed::<u32>("test", "Проверь", String::new()).unwrap();
        let strict = provider(ResponseFormat::JsonSchema).request_body(&request);
        assert_eq!(strict["response_format"]["type"], "json_schema");
        assert_eq!(strict["messages"][0]["content"], "Проверь");

        let loose = provider(ResponseFormat::JsonObject).request_body(&request);
        assert_eq!(loose["response_format"]["type"], "json_object");
        let system = loose["messages"][0]["content"].as_str().unwrap();
        assert!(system.starts_with("Проверь") && system.contains("\"result\""));
        assert!(loose.get("reasoning_effort").is_none());

        let mut quick = provider(ResponseFormat::JsonSchema);
        quick.reasoning_effort = Some("none".into());
        assert_eq!(quick.request_body(&request)["reasoning_effort"], "none");
    }

    #[test]
    fn max_tokens_is_optional_and_must_be_positive() {
        let mut provider = provider(ResponseFormat::JsonObject);
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        assert!(provider.request_body(&request).get("max_tokens").is_none());
        assert_eq!(parse_max_tokens(Some("   ")).unwrap(), None);
        for invalid in ["0", "-1", "bad"] {
            assert!(matches!(
                parse_max_tokens(Some(invalid)),
                Err(LlmError::Config("OPENAI_MAX_TOKENS"))
            ));
        }
        provider.max_tokens = parse_max_tokens(Some(" 4096 ")).unwrap();
        assert_eq!(provider.request_body(&request)["max_tokens"], 4096);
    }

    #[test]
    fn chat_response_is_unwrapped_and_reports_usage() {
        let body = json!({
            "model": "gpt-test",
            "choices": [{"finish_reason": "stop", "message": {"content": "```json\n{\"result\": [1, 2]}\n```"}}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}
        });
        let response = decode_chat_response(&body, "fallback").unwrap();
        assert_eq!(response.json, json!([1, 2]));
        assert_eq!(response.model, "gpt-test");
        assert_eq!(
            (response.usage.input_tokens, response.usage.output_tokens),
            (7, 3)
        );

        let bare =
            json!({"choices": [{"finish_reason": "stop", "message": {"content": "{\"id\": 1}"}}]});
        assert_eq!(
            decode_chat_response(&bare, "fallback").unwrap().json,
            json!({"id": 1})
        );
    }

    #[test]
    fn truncated_chat_response_is_an_error_with_usage() {
        let body = json!({
            "choices": [{"finish_reason": "length", "message": {"content": "{\"result\": ["}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 9}
        });
        let Err(error) = decode_chat_response(&body, "test") else {
            panic!("Обрезанный ответ принят");
        };
        assert!(error.to_string().contains("finishReason=length"));
        assert_eq!(error.usage().unwrap().output_tokens, 9);
    }
}
