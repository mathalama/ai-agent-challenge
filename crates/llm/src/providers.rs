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

fn gemini_request_body(request: &LlmRequest) -> Value {
    let mut schema = request.response_schema.clone();
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
    }
    let mut body = json!({
        "systemInstruction": {"parts": [{"text": request.system_prompt}]},
        "contents": [{"role": "user", "parts": [{"text": request.user_text}]}],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseJsonSchema": schema
        },
    });
    if request.include_reasoning {
        body["generationConfig"]["thinkingConfig"] = json!({"includeThoughts": true});
    }
    body
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn generate(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-goog-api-key", self.api_key.clone())
            .json(&gemini_request_body(request))
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
        decode_gemini_response(&body, self.model(), request.include_reasoning)
    }
}

fn decode_gemini_response(
    body: &Value,
    model: &str,
    include_reasoning: bool,
) -> Result<LlmResponse> {
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
    let parts = candidate["content"]["parts"].as_array();
    let reasoning = include_reasoning
        .then(|| {
            parts
                .into_iter()
                .flatten()
                .filter(|part| part["thought"] == true)
                .filter_map(|part| part["text"].as_str())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty());
    let Some(parts) = parts.filter(|_| finish_reason == Some("STOP")) else {
        return Err(LlmError::MissingCandidate {
            finish_reason: finish_reason.unwrap_or("—").into(),
            block_reason: block_reason.unwrap_or("—").into(),
            usage,
            reasoning,
        });
    };
    let text: String = parts
        .iter()
        .filter(|part| part["thought"] != true)
        .filter_map(|part| part["text"].as_str())
        .collect();
    Ok(LlmResponse {
        json: serde_json::from_str(&text).map_err(|source| LlmError::ResponseJson {
            source,
            usage,
            reasoning: reasoning.clone(),
        })?,
        usage: usage.unwrap_or_default(),
        model: body["modelVersion"].as_str().unwrap_or(model).into(),
        reasoning,
    })
}

pub(crate) fn retry_after_header(value: &HeaderValue) -> Option<Duration> {
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
                    #[serde(default)]
                    title: String,
                    blocks: Vec<Block>,
                }
                let input: SegmentInput = serde_json::from_str(&request.user_text)?;
                if input.title == crate::mock_review::DOCUMENT_TITLE {
                    json!(
                        input
                            .blocks
                            .iter()
                            .filter(|block| numbered_item(block) || sample_requirement_block(block))
                            .map(mock_candidate)
                            .collect::<Vec<_>>()
                    )
                } else {
                    json!(
                        first_mock_candidate(&input.blocks)
                            .into_iter()
                            .collect::<Vec<_>>()
                    )
                }
            }
            "set_checks" => crate::mock_review::findings(&request.user_text)?,
            "clarify" => crate::mock_review::questions(&request.user_text)?,
            "chat" => {
                let input: Value = serde_json::from_str(&request.user_text)?;
                let report = &input["report"];
                let evidence = report["findings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|finding| finding["verified"] == true)
                    .find_map(|finding| finding["evidence"].as_array()?.first());
                json!({
                    "text": format!(
                        "В отчёте по документу «{}» найдено замечаний: {}.",
                        report["passport"]["document_title"].as_str().unwrap_or("Документ"),
                        report["summary"]["finding_count"].as_u64().unwrap_or(0),
                    ),
                    "citations": evidence.into_iter().collect::<Vec<_>>(),
                })
            }
            "facts" | "rules" | "requirement_checks" | "consistency" | "ambiguity" | "verify" => {
                json!([])
            }
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
            reasoning: None,
        })
    }
}

fn numbered_item(block: &Block) -> bool {
    let BlockContent::ListItem { text } = &block.content else {
        return false;
    };
    text.split_once(". ")
        .and_then(|(id, _)| id.split_once('-'))
        .is_some_and(|(prefix, number)| {
            !prefix.is_empty()
                && prefix.chars().all(char::is_alphabetic)
                && !number.is_empty()
                && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn sample_requirement_block(block: &Block) -> bool {
    let (BlockContent::Paragraph { text } | BlockContent::ListItem { text }) = &block.content
    else {
        return false;
    };
    [
        "Сервис передаёт push на отправку",
        "Хранить идентификатор карты",
        "Приложение читает и сохраняет настройки",
        "Проверить пользовательскую сессию",
        "Проверить принадлежность карты",
        "Проверить сумму и переключатель",
        "Заменить настройку и вернуть",
        "При отклонении суммы вернуть",
        "Целое число тенге",
        "Повторное сохранение заменяет",
        "Изменение порога сохраняет",
        "Тело не используется",
        "Если результат 200 или 201",
    ]
    .iter()
    .any(|fragment| text.contains(fragment))
}

fn first_mock_candidate(blocks: &[Block]) -> Option<Value> {
    blocks
        .iter()
        .find(|block| numbered_item(block))
        .or_else(|| {
            blocks.iter().find(|block| matches!(
            &block.content, BlockContent::Paragraph { text } | BlockContent::ListItem { text }
                if !text.trim().is_empty()
        ))
        })
        .map(mock_candidate)
}

fn mock_candidate(block: &Block) -> Value {
    json!({
        "kind": "requirement",
        "block_id": block.id,
        "start_line": block.location.start_line,
        "end_line": block.location.end_line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_requests_thoughts_only_when_enabled() {
        let mut request = LlmRequest::typed::<u32>("test", "", String::new()).unwrap();
        assert!(
            gemini_request_body(&request)["generationConfig"]
                .get("thinkingConfig")
                .is_none()
        );
        request.include_reasoning = true;
        assert_eq!(
            gemini_request_body(&request)["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn gemini_keeps_thought_parts_separate_from_the_result() {
        let body = json!({"candidates": [{"finishReason": "STOP", "content": {"parts": [
            {"text": "Первый шаг", "thought": true},
            {"text": "[1,"},
            {"thought": true, "thoughtSignature": "signature"},
            {"text": "Второй шаг", "thought": true},
            {"text": "2]", "thought": false}
        ]}}]});
        let response = decode_gemini_response(&body, "test", true).unwrap();
        assert_eq!(response.json, json!([1, 2]));
        assert_eq!(
            response.reasoning.as_deref(),
            Some("Первый шаг\nВторой шаг")
        );
        assert_eq!(
            decode_gemini_response(&body, "test", false)
                .unwrap()
                .reasoning,
            None
        );
    }

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
            let Err(error) = decode_gemini_response(&body, "test", false) else {
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
        let Err(error) = decode_gemini_response(&body, "test", false) else {
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
