use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{LlmError, Result, openrouter::is_openrouter};

pub const OPENAI_URL: &str = "https://api.openai.com/v1";
pub const GEMINI_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_MODEL: &str = "gemini-3.1-flash-lite";

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Gemini,
    Openai,
    Mock,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::Openai => "OpenAI-совместимый",
            Self::Mock => "Mock",
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = LlmError;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gemini" => Ok(Self::Gemini),
            "openai" => Ok(Self::Openai),
            "mock" => Ok(Self::Mock),
            _ => Err(LlmError::Settings(
                "Выберите провайдер gemini, openai или mock",
            )),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    JsonSchema,
    JsonObject,
    None,
}

impl std::str::FromStr for ResponseFormat {
    type Err = LlmError;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "json_schema" => Ok(Self::JsonSchema),
            "json_object" => Ok(Self::JsonObject),
            "none" => Ok(Self::None),
            _ => Err(LlmError::Settings(
                "Выберите формат json_schema, json_object или none",
            )),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Profile {
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    response_format: ResponseFormat,
    free_only: Option<bool>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    gemini: Profile,
    openai: Profile,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub provider: Option<Provider>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub response_format: Option<ResponseFormat>,
    pub free_only: Option<bool>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedSettings {
    pub provider: Provider,
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: String,
    pub response_format: ResponseFormat,
    pub free_only: bool,
}

#[derive(Serialize)]
pub struct SettingsView {
    pub provider: Provider,
    pub effective_provider: Provider,
    pub gemini: ProfileView,
    pub openai: ProfileView,
}

#[derive(Serialize)]
pub struct ProfileView {
    pub api_key_mask: Option<String>,
    pub model: String,
    pub base_url: String,
    pub response_format: ResponseFormat,
    pub free_only: bool,
}

impl ResolvedSettings {
    pub fn name(&self) -> &'static str {
        if self.provider == Provider::Openai && is_openrouter(&self.base_url) {
            "OpenRouter"
        } else {
            self.provider.name()
        }
    }

    pub fn masked(&self) -> ProfileView {
        ProfileView {
            api_key_mask: self.api_key.as_ref().map(|key| {
                let len = key.chars().count();
                if len <= 4 {
                    return "задан ••••".into();
                }
                let tail: String = key.chars().skip(len - 4).collect();
                format!("задан ••••{tail}")
            }),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            response_format: self.response_format,
            free_only: self.free_only,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.provider != Provider::Mock {
            validate_url(&self.base_url)?;
            validate_model(self.provider, &self.model)?;
        }
        Ok(())
    }
}

impl Settings {
    pub fn resolve(&self, provider: Option<Provider>) -> Result<ResolvedSettings> {
        self.resolve_with(provider, optional_env)
    }

    /// Проверяет явно введённые значения без изменения сохранённой конфигурации.
    pub fn preview(
        &self,
        mut patch: SettingsPatch,
        provider: Provider,
    ) -> Result<ResolvedSettings> {
        let explicit_key = patch.api_key.is_some();
        let explicit_url = patch.base_url.is_some();
        let explicit_model = patch.model.is_some();
        let explicit_format = patch.response_format.is_some();
        patch.provider = Some(provider);
        let mut candidate = self.clone();
        candidate.apply(patch)?;
        candidate.resolve_with(Some(provider), |name| {
            let explicit = match name {
                "OPENAI_API_KEY" | "OPENROUTER_API_KEY" | "GEMINI_API_KEY" => explicit_key,
                "OPENAI_BASE_URL" | "GEMINI_BASE_URL" => explicit_url,
                "OPENAI_MODEL" | "GEMINI_MODEL" => explicit_model,
                "OPENAI_RESPONSE_FORMAT" => explicit_format,
                _ => false,
            };
            if explicit {
                Ok(None)
            } else {
                optional_env(name)
            }
        })
    }

    fn resolve_with(
        &self,
        provider: Option<Provider>,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<ResolvedSettings> {
        let provider = match provider {
            Some(provider) => provider,
            None => match get("RS_PROVIDER")? {
                Some(value) => value.parse()?,
                None => match get("PROVIDER")? {
                    Some(value) => value.parse()?,
                    None => self.provider,
                },
            },
        };
        let gemini = &self.gemini;
        let openai = &self.openai;
        let (api_key, model, base_url, response_format) = match provider {
            Provider::Mock => (None, "mock".into(), String::new(), ResponseFormat::None),
            Provider::Gemini => (
                get("GEMINI_API_KEY")?.or_else(|| gemini.api_key.clone()),
                get("GEMINI_MODEL")?
                    .or_else(|| gemini.model.clone())
                    .unwrap_or_else(|| GEMINI_MODEL.into()),
                get("GEMINI_BASE_URL")?
                    .or_else(|| gemini.base_url.clone())
                    .unwrap_or_else(|| GEMINI_URL.into()),
                ResponseFormat::JsonSchema,
            ),
            Provider::Openai => {
                let base_url = get("OPENAI_BASE_URL")?
                    .or_else(|| openai.base_url.clone())
                    .unwrap_or_else(|| OPENAI_URL.into());
                let fallback = if is_openrouter(&base_url) {
                    get("OPENROUTER_API_KEY")?
                } else {
                    None
                };
                (
                    get("OPENAI_API_KEY")?
                        .or(fallback)
                        .or_else(|| openai.api_key.clone()),
                    get("OPENAI_MODEL")?
                        .or_else(|| openai.model.clone())
                        .unwrap_or_default(),
                    base_url,
                    get("OPENAI_RESPONSE_FORMAT")?
                        .map(|value| value.parse())
                        .transpose()?
                        .unwrap_or(openai.response_format),
                )
            }
        };
        Ok(ResolvedSettings {
            provider,
            api_key,
            model,
            base_url,
            response_format,
            free_only: openai.free_only.unwrap_or(true),
        })
    }

    pub fn view(&self, provider: Option<Provider>) -> Result<SettingsView> {
        Ok(SettingsView {
            provider: self.provider,
            effective_provider: self.resolve(provider)?.provider,
            gemini: self.resolve(Some(Provider::Gemini))?.masked(),
            openai: self.resolve(Some(Provider::Openai))?.masked(),
        })
    }

    pub fn apply(&mut self, patch: SettingsPatch) -> Result<()> {
        let provider = patch.provider.unwrap_or(self.provider);
        let profile = match provider {
            Provider::Gemini => Some(&mut self.gemini),
            Provider::Openai => Some(&mut self.openai),
            Provider::Mock => None,
        };
        if let Some(profile) = profile {
            if let Some(key) = patch.api_key {
                let key = key.trim();
                if !key.is_empty() && reqwest::header::HeaderValue::from_str(key).is_err() {
                    return Err(LlmError::Settings("Некорректный API-ключ"));
                }
                profile.api_key = (!key.is_empty()).then(|| key.to_owned());
            }
            if let Some(model) = patch.model {
                let model = model.trim();
                if !model.is_empty() {
                    validate_model(provider, model)?;
                }
                profile.model = (!model.is_empty()).then(|| model.to_owned());
            }
            if let Some(url) = patch.base_url {
                let url = url.trim().trim_end_matches('/');
                if !url.is_empty() {
                    validate_url(url)?;
                }
                profile.base_url = (!url.is_empty()).then(|| url.to_owned());
            }
            if let Some(format) = patch.response_format {
                profile.response_format = format;
            }
            if let Some(free_only) = patch.free_only {
                profile.free_only = Some(free_only);
            }
        }
        self.provider = provider;
        Ok(())
    }

    pub fn load(path: &Path) -> std::result::Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "Некорректный файл настроек rs")
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> std::result::Result<(), io::Error> {
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temporary, self)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }
}

pub fn config_path() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
    }
    .map(|dir| dir.join("rs").join("config.json"))
}

pub(crate) fn optional_env(name: &'static str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok((!value.trim().is_empty()).then(|| value.trim().to_owned())),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(LlmError::Config(name)),
    }
}

fn validate_url(value: &str) -> Result<()> {
    let valid = Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "https" | "http")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if valid {
        Ok(())
    } else {
        Err(LlmError::Settings(
            "Укажите адрес HTTP(S) без учётных данных и параметров",
        ))
    }
}

fn validate_model(provider: Provider, model: &str) -> Result<()> {
    let model = if provider == Provider::Gemini {
        model.strip_prefix("models/").unwrap_or(model)
    } else {
        model
    };
    let valid = !model.is_empty()
        && if provider == Provider::Gemini {
            model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        } else {
            !model
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        };
    if valid {
        Ok(())
    } else {
        Err(LlmError::Settings("Укажите корректный ID модели"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_win_over_environment_and_openrouter_alias_wins_over_file() {
        let mut settings = Settings::default();
        settings
            .apply(SettingsPatch {
                provider: Some(Provider::Openai),
                api_key: Some("stored".into()),
                base_url: Some(crate::openrouter::OPENROUTER_URL.into()),
                model: Some("stored/model".into()),
                response_format: Some(ResponseFormat::JsonObject),
                ..SettingsPatch::default()
            })
            .unwrap();
        let get = |name| {
            Ok(match name {
                "RS_PROVIDER" => Some("gemini".into()),
                "OPENAI_MODEL" => Some("env/model".into()),
                "OPENROUTER_API_KEY" => Some("alias-key".into()),
                _ => None,
            })
        };
        assert_eq!(
            settings.resolve_with(None, get).unwrap().provider,
            Provider::Gemini
        );
        let config = settings.resolve_with(Some(Provider::Openai), get).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("alias-key"));
        assert_eq!(config.model, "env/model");
        assert_eq!(config.response_format, ResponseFormat::JsonObject);
        let primary = settings
            .resolve_with(Some(Provider::Openai), |name| {
                if name == "OPENAI_API_KEY" {
                    Ok(Some("primary-key".into()))
                } else {
                    get(name)
                }
            })
            .unwrap();
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
    }
}
