use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{LlmError, Result, settings::ResponseFormat};

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
pub const NO_JSON_WARNING: &str = "Модель не поддерживает JSON-режим, отчёт может не собраться";

pub fn is_openrouter(address: &str) -> bool {
    Url::parse(address).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("openrouter.ai")
            && url.port().is_none()
            && url.path().trim_end_matches('/') == "/api/v1"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

#[derive(Serialize)]
pub struct RouterModel {
    pub id: String,
    pub context_length: u64,
    pub response_format: ResponseFormat,
}

#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    context_length: Option<u64>,
    pricing: Pricing,
    #[serde(default)]
    supported_parameters: Vec<String>,
    architecture: Architecture,
}

#[derive(Deserialize)]
struct Pricing {
    prompt: String,
    completion: String,
}

#[derive(Deserialize)]
struct Architecture {
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

pub async fn models(free_only: bool) -> Result<Vec<RouterModel>> {
    let response = client()?
        .get(format!("{OPENROUTER_URL}/models"))
        .send()
        .await
        .map_err(|_| LlmError::Settings("Каталог OpenRouter недоступен"))?;
    let body = response
        .error_for_status()
        .map_err(|_| LlmError::Settings("Каталог OpenRouter недоступен"))?
        .bytes()
        .await
        .map_err(|_| LlmError::Settings("Не удалось загрузить каталог OpenRouter"))?;
    parse_models(&body, free_only)
        .map_err(|_| LlmError::Settings("Не удалось прочитать каталог OpenRouter"))
}

fn parse_models(body: &[u8], free_only: bool) -> Result<Vec<RouterModel>> {
    let response: Envelope<Vec<CatalogModel>> = serde_json::from_slice(body)?;
    let mut models: Vec<_> = response
        .data
        .into_iter()
        .filter_map(|model| {
            let text = model
                .architecture
                .output_modalities
                .iter()
                .any(|item| item == "text");
            let free = model.id.ends_with(":free")
                || (model.pricing.prompt == "0" && model.pricing.completion == "0");
            if !text || (free_only && !free) {
                return None;
            }
            let has = |parameter| {
                model
                    .supported_parameters
                    .iter()
                    .any(|item| item == parameter)
            };
            Some(RouterModel {
                id: model.id,
                context_length: model.context_length.unwrap_or(0),
                response_format: if has("structured_outputs") {
                    ResponseFormat::JsonSchema
                } else if has("response_format") {
                    ResponseFormat::JsonObject
                } else {
                    ResponseFormat::None
                },
            })
        })
        .collect();
    models.sort_unstable_by(|a, b| {
        b.context_length
            .cmp(&a.context_length)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(models)
}

#[derive(Deserialize)]
pub struct KeyStatus {
    pub is_free_tier: bool,
    pub free_model_daily_requests: Option<DailyRequests>,
}

#[derive(Deserialize)]
pub struct DailyRequests {
    pub used: u64,
    pub limit: u64,
    pub remaining: u64,
}

impl KeyStatus {
    pub fn summary(&self) -> String {
        let mut message = String::from("Ключ действителен");
        if self.is_free_tier {
            message.push_str(" · бесплатный тариф");
        }
        if let Some(quota) = &self.free_model_daily_requests {
            message.push_str(&format!(
                " · осталось {} из {} бесплатных запросов сегодня",
                quota.remaining, quota.limit
            ));
        }
        message
    }
}

pub async fn key_status(key: &str) -> Result<KeyStatus> {
    fetch_key(&client()?, &format!("{OPENROUTER_URL}/key"), key).await
}

async fn fetch_key(client: &Client, url: &str, key: &str) -> Result<KeyStatus> {
    let response = client
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| LlmError::Settings("Не удалось проверить ключ OpenRouter"))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(LlmError::Settings("Ключ OpenRouter недействителен"));
    }
    response
        .error_for_status()
        .map_err(|_| LlmError::Settings("Не удалось проверить ключ OpenRouter"))?
        .json::<Envelope<KeyStatus>>()
        .await
        .map(|result| result.data)
        .map_err(|_| LlmError::Settings("OpenRouter вернул некорректный статус ключа"))
}

pub(crate) fn daily_limit(body: &Value) -> bool {
    if body["error"]["metadata"]["limit_source"] == "openrouter_free_tier_daily" {
        return true;
    }
    let message = body["error"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    (message.contains("daily")
        || message.contains("per-day")
        || message.contains("per day")
        || message.contains("free-models-per-day"))
        && (message.contains("free") || message.contains(":free"))
        && (message.contains("limit") || message.contains("quota"))
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_text_and_prices_and_chooses_json_format() {
        let body = include_bytes!("../tests/fixtures/openrouter-models.json");
        let free = parse_models(body, true).unwrap();
        let choices: Vec<_> = free
            .iter()
            .map(|model| (model.id.as_str(), model.response_format))
            .collect();
        assert_eq!(
            choices,
            [
                ("example/schema:free", ResponseFormat::JsonSchema),
                ("example/object", ResponseFormat::JsonObject),
                ("example/plain:free", ResponseFormat::None),
            ]
        );
        assert_eq!(parse_models(body, false).unwrap()[0].id, "example/paid");
    }

    #[test]
    fn reads_daily_quota_from_key_status() {
        let payload: Envelope<KeyStatus> =
            serde_json::from_str(include_str!("../tests/fixtures/openrouter-key.json")).unwrap();
        assert_eq!(
            payload
                .data
                .free_model_daily_requests
                .as_ref()
                .unwrap()
                .used,
            13
        );
        assert_eq!(
            payload.data.summary(),
            "Ключ действителен · бесплатный тариф · осталось 37 из 50 бесплатных запросов сегодня"
        );
    }

    #[tokio::test]
    async fn checks_the_key_endpoint_without_a_completion_and_maps_401() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
        };

        for (status, body) in [
            (
                "200 OK",
                include_str!("../tests/fixtures/openrouter-key.json"),
            ),
            (
                "401 Unauthorized",
                r#"{"error":{"message":"User not found","code":401}}"#,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/api/v1/key", listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let headers: Vec<_> = BufReader::new(&socket)
                    .lines()
                    .map(|line| line.unwrap())
                    .take_while(|line| !line.is_empty())
                    .collect();
                assert_eq!(headers[0], "GET /api/v1/key HTTP/1.1");
                assert!(
                    headers
                        .iter()
                        .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fake"))
                );
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let result = fetch_key(&client().unwrap(), &endpoint, "fake").await;
            peer.join().unwrap();
            if status.starts_with("200") {
                assert_eq!(
                    result.unwrap().free_model_daily_requests.unwrap().remaining,
                    37
                );
            } else {
                assert!(matches!(
                    result,
                    Err(LlmError::Settings("Ключ OpenRouter недействителен"))
                ));
            }
        }
    }

    #[test]
    fn distinguishes_daily_limit_from_retryable_minute_limit() {
        let payload: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/openrouter-daily-limit.json"
        ))
        .unwrap();
        assert!(daily_limit(&payload));
        let payload: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/openrouter-minute-limit.json"
        ))
        .unwrap();
        assert!(!daily_limit(&payload));
        assert!(daily_limit(
            &serde_json::json!({"error":{"message":"Rate limit exceeded: free-models-per-day"}})
        ));
    }
}
