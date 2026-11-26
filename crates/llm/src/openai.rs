//! Провайдер для API, совместимых с OpenAI Chat Completions: OpenAI, GitHub Models,
//! OpenRouter, Groq, DeepSeek и другие. Отличаются они только адресом, ключом и моделью.

use std::{env, num::NonZeroU32, time::Duration};

use async_trait::async_trait;
use reqwest::{
    Client,
    header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER},
};
use serde_json::{Map, Value, json};

use crate::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, ReasoningCallback, Result, Usage,
    openrouter::{daily_limit, is_openrouter},
    providers::retry_after_header,
    settings::{Provider, ResolvedSettings, ResponseFormat, Settings},
};

const MAX_TOKENS_VAR: &str = "OPENAI_MAX_TOKENS";
/// Ключ, под которым лежит ответ модели в обёртке схемы.
const RESULT_FIELD: &str = "result";

pub struct OpenAiProvider {
    api_key: HeaderValue,
    model: String,
    endpoint: String,
    openrouter: bool,
    format: ResponseFormat,
    /// `reasoning_effort` для рассуждающих моделей; `none` отключает рассуждения у Ollama.
    reasoning_effort: Option<String>,
    max_tokens: Option<NonZeroU32>,
    client: Client,
}

impl OpenAiProvider {
    pub fn from_env() -> Result<Self> {
        Self::from_settings(&Settings::default().resolve(Some(Provider::Openai))?)
    }

    pub fn from_settings(config: &ResolvedSettings) -> Result<Self> {
        const KEY: &str = "OPENAI_API_KEY";
        const REASONING: &str = "OPENAI_REASONING_EFFORT";
        const TIMEOUT: &str = "OPENAI_TIMEOUT_SECS";

        config.validate()?;
        let key = config.api_key.as_deref().ok_or(LlmError::Config(KEY))?;
        let mut api_key: HeaderValue = format!("Bearer {key}")
            .parse()
            .map_err(|_| LlmError::Config(KEY))?;
        api_key.set_sensitive(true);
        let base_url = config.base_url.trim_end_matches('/');
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
            model: config.model.clone(),
            endpoint: format!("{base_url}/chat/completions"),
            openrouter: is_openrouter(base_url),
            format: config.response_format,
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
            ResponseFormat::JsonObject | ResponseFormat::None => (
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
        });
        if self.format != ResponseFormat::None {
            body["response_format"] = response_format;
        }
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = json!(max_tokens.get());
        }
        body
    }

    async fn complete(
        &self,
        request: &LlmRequest,
        on_delta: Option<&ReasoningCallback<'_>>,
    ) -> Result<LlmResponse> {
        let mut body = self.request_body(request);
        if on_delta.is_some() {
            body["stream"] = json!(true);
            body["stream_options"] = json!({"include_usage": true});
        }
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", self.api_key.clone())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(retry_after_header);
            let body = response.json::<Value>().await.unwrap_or_default();
            if self.openrouter && status.as_u16() == 429 && daily_limit(&body) {
                return Err(LlmError::OpenRouterDailyLimit);
            }
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
        if let Some(on_delta) = on_delta
            && response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return read_stream(response, self.model(), on_delta).await;
        }
        let body = serde_json::from_slice(&response.bytes().await?)?;
        decode_chat_response(&body, self.model(), request.include_reasoning)
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
        self.complete(request, None).await
    }

    async fn generate_with_reasoning(
        &self,
        request: &LlmRequest,
        on_delta: &ReasoningCallback<'_>,
    ) -> Result<LlmResponse> {
        self.complete(request, request.include_reasoning.then_some(on_delta))
            .await
    }
}

async fn read_stream(
    mut response: reqwest::Response,
    model: &str,
    on_delta: &ReasoningCallback<'_>,
) -> Result<LlmResponse> {
    let mut stream = ChatStream::default();
    let mut buffer = Vec::new();
    while !stream.done {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        buffer.extend_from_slice(&chunk);
        let mut consumed = 0;
        while let Some(end) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            let line = &buffer[consumed..consumed + end];
            consumed += end + 1;
            stream.line(line, on_delta)?;
            if stream.done {
                break;
            }
        }
        buffer.drain(..consumed);
    }
    if !stream.done {
        stream.line(&buffer, on_delta)?;
        stream.line(b"", on_delta)?;
    }
    stream.finish(model)
}

#[derive(Default)]
struct ChatStream {
    data: Vec<u8>,
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    usage: Option<Value>,
    done: bool,
}

impl ChatStream {
    fn line(&mut self, line: &[u8], on_delta: &dyn Fn(&str)) -> Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(data) = line.strip_prefix(b"data:") {
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data
                .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
        } else if line.is_empty() && !self.data.is_empty() {
            let data = std::mem::take(&mut self.data);
            if data == b"[DONE]" {
                self.done = true;
                return Ok(());
            }
            let chunk: Value = serde_json::from_slice(&data)?;
            if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
                self.usage = Some(usage.clone());
            }
            let choice = &chunk["choices"][0];
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str() {
                self.content.push_str(text);
            }
            if let Some(text) = reasoning_text(delta) {
                self.reasoning.push_str(text);
                on_delta(text);
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish_reason = Some(reason.into());
            }
        }
        Ok(())
    }

    fn finish(self, model: &str) -> Result<LlmResponse> {
        if !self.done && self.finish_reason.is_none() {
            return Err(LlmError::IncompleteStream);
        }
        decode_chat_response(
            &json!({
                "choices": [{
                    "message": { "content": self.content, "reasoning": self.reasoning },
                    "finish_reason": self.finish_reason,
                }],
                "usage": self.usage,
            }),
            model,
            true,
        )
    }
}

fn reasoning_text(message: &Value) -> Option<&str> {
    ["reasoning_content", "reasoning"]
        .into_iter()
        .filter_map(|field| message[field].as_str())
        .find(|text| !text.is_empty())
}

fn decode_chat_response(body: &Value, model: &str, include_reasoning: bool) -> Result<LlmResponse> {
    let choice = &body["choices"][0];
    let finish_reason = choice["finish_reason"].as_str();
    let usage = body
        .get("usage")
        .filter(|value| !value.is_null())
        .map(|usage| Usage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        });
    let message = &choice["message"];
    let reasoning = include_reasoning
        .then(|| reasoning_text(message).map(str::to_owned))
        .flatten();
    let Some(content) = message["content"]
        .as_str()
        .filter(|_| finish_reason == Some("stop"))
    else {
        return Err(LlmError::MissingCandidate {
            finish_reason: finish_reason.unwrap_or("—").into(),
            block_reason: "—".into(),
            usage,
            reasoning,
        });
    };
    let mut json: Value = serde_json::from_str(strip_code_fence(content)).map_err(|source| {
        LlmError::ResponseJson {
            source,
            usage,
            reasoning: reasoning.clone(),
        }
    })?;
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
        reasoning,
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

    #[tokio::test]
    async fn streaming_emits_reasoning_before_json_is_complete() {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            net::TcpListener,
            sync::mpsc,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut provider = provider(ResponseFormat::JsonObject);
        provider.endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let (notify, notified) = mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut input = BufReader::new(&socket);
            let mut size = 0;
            for line in input.by_ref().lines() {
                let line = line.unwrap();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    size = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; size];
            input.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["stream"], true);
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            let first =
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Сверяю \"}}]}\r\n\r\n";
            let cut = first.find('С').unwrap() + 1;
            let mut chunk = |bytes: &[u8]| {
                write!(socket, "{:x}\r\n", bytes.len()).unwrap();
                socket.write_all(bytes).unwrap();
                socket.write_all(b"\r\n").unwrap();
            };
            chunk(&first.as_bytes()[..cut]);
            chunk(&first.as_bytes()[cut..]);
            assert_eq!(
                notified.recv_timeout(Duration::from_secs(5)).unwrap(),
                "Сверяю "
            );
            chunk(concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning\":\"строки\",\"content\":\"{\\\"result\\\":\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"42}\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9}}\n\n",
                "data: [DONE]\n\n"
            ).as_bytes());
            socket.write_all(b"0\r\n\r\n").unwrap();
        });
        let mut request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        request.include_reasoning = true;
        let response = provider
            .generate_with_reasoning(&request, &|delta| {
                if delta == "Сверяю " {
                    notify.send(delta.to_owned()).unwrap();
                }
            })
            .await
            .unwrap();
        peer.join().unwrap();
        assert_eq!(response.json, 42);
        assert_eq!(response.reasoning.as_deref(), Some("Сверяю строки"));
        assert_eq!(response.usage.input_tokens, 7);
        assert_eq!(response.usage.output_tokens, 9);
    }

    #[tokio::test]
    async fn openrouter_daily_limit_never_consumes_a_retry() {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut provider = provider(ResponseFormat::None);
        provider.endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        provider.openrouter = true;
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&socket);
            let mut size = 0;
            for line in reader.by_ref().lines() {
                let line = line.unwrap();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    size = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(body.get("response_format").is_none());
            let response = include_str!("../tests/fixtures/openrouter-daily-limit.json");
            write!(socket, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 60\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let retry = crate::RetryProvider::new(Box::new(provider));
        let request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), retry.generate(&request))
            .await
            .unwrap();
        assert!(matches!(response, Err(LlmError::OpenRouterDailyLimit)));
        peer.join().unwrap();
    }

    #[test]
    fn chat_decodes_optional_reasoning_fields() {
        for (fields, expected) in [
            (json!({"reasoning_content": "DeepSeek"}), Some("DeepSeek")),
            (json!({"reasoning": "Ollama"}), Some("Ollama")),
            (
                json!({"reasoning_content": null, "reasoning": "OpenRouter"}),
                Some("OpenRouter"),
            ),
            (json!({"reasoning_content": "", "reasoning": null}), None),
            (json!({}), None),
        ] {
            let mut message = fields;
            message["content"] = json!("{\"result\": 42}");
            let body = json!({"choices": [{"finish_reason": "stop", "message": message}]});
            let response = decode_chat_response(&body, "test", true).unwrap();
            assert_eq!(response.json, 42);
            assert_eq!(response.reasoning.as_deref(), expected);
            assert_eq!(
                decode_chat_response(&body, "test", false)
                    .unwrap()
                    .reasoning,
                None
            );
        }
    }

    fn provider(format: ResponseFormat) -> OpenAiProvider {
        OpenAiProvider {
            api_key: HeaderValue::from_static("Bearer test"),
            model: "test-model".into(),
            endpoint: format!("{}/chat/completions", crate::settings::OPENAI_URL),
            openrouter: false,
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
        let response = decode_chat_response(&body, "fallback", false).unwrap();
        assert_eq!(response.json, json!([1, 2]));
        assert_eq!(response.model, "gpt-test");
        assert_eq!(
            (response.usage.input_tokens, response.usage.output_tokens),
            (7, 3)
        );

        let bare =
            json!({"choices": [{"finish_reason": "stop", "message": {"content": "{\"id\": 1}"}}]});
        assert_eq!(
            decode_chat_response(&bare, "fallback", false).unwrap().json,
            json!({"id": 1})
        );
    }

    #[test]
    fn truncated_chat_response_is_an_error_with_usage() {
        let body = json!({
            "choices": [{"finish_reason": "length", "message": {"content": "{\"result\": ["}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 9}
        });
        let Err(error) = decode_chat_response(&body, "test", false) else {
            panic!("Обрезанный ответ принят");
        };
        assert!(error.to_string().contains("finishReason=length"));
        assert_eq!(error.usage().unwrap().output_tokens, 9);
    }
}
