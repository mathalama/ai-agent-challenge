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
    pub env_overrides: Vec<&'static str>,
}

impl ResolvedSettings {
    pub fn name(&self) -> &'static str {
        if self.provider == Provider::Openai && is_openrouter(&self.base_url) {
            "OpenRouter"
        } else {
            self.provider.name()
        }
    }

    pub fn masked(&self, env_overrides: Vec<&'static str>) -> ProfileView {
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
            env_overrides,
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
    pub fn preview(&self, patch: SettingsPatch, provider: Provider) -> Result<ResolvedSettings> {
        self.preview_with(patch, provider, optional_env)
    }

    fn preview_with(
        &self,
        mut patch: SettingsPatch,
        provider: Provider,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<ResolvedSettings> {
        let explicit_key = patch.api_key.is_some();
        let explicit_url = patch.base_url.is_some();
        let explicit_model = patch.model.is_some();
        let explicit_format = patch.response_format.is_some();
        patch.provider = Some(provider);
        let mut candidate = self.clone();
        candidate.apply(patch)?;
        let address = match provider {
            Provider::Gemini => candidate.gemini.base_url.as_deref().unwrap_or(GEMINI_URL),
            Provider::Openai => candidate.openai.base_url.as_deref().unwrap_or(OPENAI_URL),
            Provider::Mock => "",
        };
        candidate.resolve_at(Some(provider), explicit_url.then_some(address), |name| {
            let explicit = match name {
                "OPENAI_API_KEY" | "OPENROUTER_API_KEY" | "GEMINI_API_KEY" => explicit_key,
                "OPENAI_MODEL" | "GEMINI_MODEL" => explicit_model,
                "OPENAI_RESPONSE_FORMAT" => explicit_format,
                _ => false,
            };
            if explicit { Ok(None) } else { get(name) }
        })
    }

    fn resolve_with(
        &self,
        provider: Option<Provider>,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<ResolvedSettings> {
        self.resolve_at(provider, None, get)
    }

    fn resolve_at(
        &self,
        provider: Option<Provider>,
        address: Option<&str>,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<ResolvedSettings> {
        let provider = match provider {
            Some(provider) => provider,
            None => get("RS_PROVIDER")?
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(self.provider),
        };
        let (profile, default_url, url_var, key_var, model_var) = match provider {
            Provider::Gemini => (
                &self.gemini,
                GEMINI_URL,
                "GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                "GEMINI_MODEL",
            ),
            Provider::Openai => (
                &self.openai,
                OPENAI_URL,
                "OPENAI_BASE_URL",
                "OPENAI_API_KEY",
                "OPENAI_MODEL",
            ),
            Provider::Mock => {
                return Ok(ResolvedSettings {
                    provider,
                    api_key: None,
                    model: "mock".into(),
                    base_url: String::new(),
                    response_format: ResponseFormat::None,
                    free_only: false,
                });
            }
        };
        let env_url = get(url_var)?;
        let saved_url = profile.base_url.as_deref().unwrap_or(default_url);
        let base_url = address.or(env_url.as_deref()).unwrap_or(saved_url);
        // Ключи из профиля и окружения принадлежат адресам, заданным в том же источнике.
        let env_key = if same_api_url(base_url, env_url.as_deref().unwrap_or(default_url)) {
            get(key_var)?
        } else {
            None
        };
        let alias = if env_key.is_none() && provider == Provider::Openai && is_openrouter(base_url)
        {
            get("OPENROUTER_API_KEY")?
        } else {
            None
        };
        let saved_key = if same_api_url(base_url, saved_url) {
            profile.api_key.clone()
        } else {
            None
        };
        let model = get(model_var)?
            .or_else(|| profile.model.clone())
            .unwrap_or_else(|| {
                if provider == Provider::Gemini {
                    GEMINI_MODEL.into()
                } else {
                    String::new()
                }
            });
        let response_format = if provider == Provider::Openai {
            get("OPENAI_RESPONSE_FORMAT")?
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(profile.response_format)
        } else {
            ResponseFormat::JsonSchema
        };
        Ok(ResolvedSettings {
            provider,
            api_key: env_key.or(alias).or(saved_key),
            model,
            base_url: base_url.to_owned(),
            response_format,
            free_only: self.openai.free_only.unwrap_or(true),
        })
    }

    pub fn view(&self, provider: Option<Provider>) -> Result<SettingsView> {
        self.view_with(provider, optional_env)
    }

    fn view_with(
        &self,
        provider: Option<Provider>,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<SettingsView> {
        let effective_provider = self.resolve_with(provider, &get)?.provider;
        let gemini_resolved = self.resolve_with(Some(Provider::Gemini), &get)?;
        let gemini_env =
            self.env_overrides_for(Provider::Gemini, &gemini_resolved.base_url, &get)?;
        let openai_resolved = self.resolve_with(Some(Provider::Openai), &get)?;
        let openai_env =
            self.env_overrides_for(Provider::Openai, &openai_resolved.base_url, &get)?;
        Ok(SettingsView {
            provider: self.provider,
            effective_provider,
            gemini: gemini_resolved.masked(gemini_env),
            openai: openai_resolved.masked(openai_env),
        })
    }

    fn env_overrides_for(
        &self,
        provider: Provider,
        base_url: &str,
        get: impl Fn(&'static str) -> Result<Option<String>>,
    ) -> Result<Vec<&'static str>> {
        let (default_url, url_var, key_var, model_var) = match provider {
            Provider::Gemini => (
                GEMINI_URL,
                "GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                "GEMINI_MODEL",
            ),
            Provider::Openai => (
                OPENAI_URL,
                "OPENAI_BASE_URL",
                "OPENAI_API_KEY",
                "OPENAI_MODEL",
            ),
            Provider::Mock => return Ok(Vec::new()),
        };
        let mut overrides = Vec::new();
        let env_url = get(url_var)?;
        if env_url.is_some() {
            overrides.push(url_var);
        }
        let env_key = if same_api_url(base_url, env_url.as_deref().unwrap_or(default_url)) {
            get(key_var)?
        } else {
            None
        };
        if env_key.is_some() {
            overrides.push(key_var);
        } else if provider == Provider::Openai
            && is_openrouter(base_url)
            && get("OPENROUTER_API_KEY")?.is_some()
        {
            overrides.push("OPENROUTER_API_KEY");
        }
        if get(model_var)?.is_some() {
            overrides.push(model_var);
        }
        if provider == Provider::Openai && get("OPENAI_RESPONSE_FORMAT")?.is_some() {
            overrides.push("OPENAI_RESPONSE_FORMAT");
        }
        Ok(overrides)
    }

    pub fn apply(&mut self, patch: SettingsPatch) -> Result<()> {
        let provider = patch.provider.unwrap_or(self.provider);
        let (profile, default_url) = match provider {
            Provider::Gemini => (&mut self.gemini, GEMINI_URL),
            Provider::Openai => (&mut self.openai, OPENAI_URL),
            Provider::Mock => {
                if patch.api_key.is_some()
                    || patch.model.is_some()
                    || patch.base_url.is_some()
                    || patch.response_format.is_some()
                    || patch.free_only.is_some()
                {
                    return Err(LlmError::Settings(
                        "Для настройки модели, адреса или ключа выберите другой провайдер",
                    ));
                }
                self.provider = provider;
                return Ok(());
            }
        };
        let key = patch.api_key.as_deref();
        let model = patch.model.as_deref().map(str::trim);
        let url = patch
            .base_url
            .as_deref()
            .map(|url| url.trim().trim_end_matches('/'));
        if let Some(key) = key.map(str::trim).filter(|key| !key.is_empty())
            && reqwest::header::HeaderValue::from_str(key).is_err()
        {
            return Err(LlmError::Settings("Некорректный API-ключ"));
        }
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            validate_model(provider, model)?;
        }
        if let Some(url) = url {
            let url = (!url.is_empty()).then_some(url);
            if let Some(url) = url {
                validate_url(url)?;
            }
            if !same_api_url(
                profile.base_url.as_deref().unwrap_or(default_url),
                url.unwrap_or(default_url),
            ) {
                profile.api_key = None;
            }
            profile.base_url = url.map(str::to_owned);
        }
        if let Some(key) = key.map(str::trim) {
            profile.api_key = (!key.is_empty()).then(|| key.to_owned());
        }
        if let Some(model) = model {
            profile.model = (!model.is_empty()).then(|| model.to_owned());
        }
        if let Some(format) = patch.response_format {
            profile.response_format = format;
        }
        if let Some(free_only) = patch.free_only {
            profile.free_only = Some(free_only);
        }
        self.provider = provider;
        Ok(())
    }

    pub fn load(path: &Path) -> std::result::Result<Self, io::Error> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Некорректный файл настроек «{}»", path.display()),
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "Не удалось прочитать файл настроек «{}»: {error}",
                    path.display()
                ),
            )),
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

pub fn same_api_url(left: &str, right: &str) -> bool {
    let parse = |url: &str| Url::parse(url.trim().trim_end_matches('/'));
    matches!((parse(left), parse(right)), (Ok(left), Ok(right)) if left == right)
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
    fn preview_and_save_drop_keys_when_the_api_address_changes() {
        let old = crate::openrouter::OPENROUTER_URL;
        let new = "https://api.groq.com/openai/v1";
        let mut settings = Settings::default();
        settings
            .apply(SettingsPatch {
                provider: Some(Provider::Openai),
                base_url: Some(old.into()),
                api_key: Some("stored-router-key".into()),
                ..Default::default()
            })
            .unwrap();
        let change = SettingsPatch {
            base_url: Some(new.into()),
            ..Default::default()
        };
        assert!(
            settings
                .preview_with(change.clone(), Provider::Openai, |_| Ok(None))
                .unwrap()
                .api_key
                .is_none()
        );
        assert_eq!(
            settings
                .resolve_with(None, |_| Ok(None))
                .unwrap()
                .api_key
                .as_deref(),
            Some("stored-router-key")
        );
        settings.apply(change).unwrap();
        assert!(
            settings
                .resolve_with(None, |_| Ok(None))
                .unwrap()
                .api_key
                .is_none()
        );
        assert!(
            settings
                .resolve_with(None, |name| Ok(
                    (name == "OPENAI_BASE_URL").then(|| old.to_owned())
                ))
                .unwrap()
                .api_key
                .is_none()
        );
    }

    #[test]
    fn environment_and_saved_keys_are_bound_to_their_own_api_addresses() {
        let mut settings = Settings::default();
        settings
            .apply(SettingsPatch {
                provider: Some(Provider::Openai),
                base_url: Some("https://api.groq.com/openai/v1".into()),
                api_key: Some("groq-key".into()),
                ..Default::default()
            })
            .unwrap();
        let resolved = settings
            .resolve_with(None, |name| {
                Ok(match name {
                    "OPENAI_BASE_URL" => Some(OPENAI_URL.into()),
                    _ => None,
                })
            })
            .unwrap();
        assert!(resolved.api_key.is_none());
        let resolved = settings
            .resolve_with(None, |name| {
                Ok(match name {
                    "OPENAI_API_KEY" => Some("openai-key".into()),
                    "PROVIDER" => Some("mock".into()),
                    _ => None,
                })
            })
            .unwrap();
        assert_eq!(resolved.provider, Provider::Openai);
        assert_eq!(resolved.api_key.as_deref(), Some("groq-key"));
    }

    #[test]
    fn draft_uses_only_credentials_for_the_requested_endpoint() {
        let router = crate::openrouter::OPENROUTER_URL;
        let settings = Settings::default();
        let env = |name| {
            Ok(match name {
                "OPENAI_BASE_URL" => Some(router.into()),
                "OPENAI_API_KEY" => Some("env-router-key".into()),
                _ => None,
            })
        };
        let preview = |url: &str, key: Option<&str>| {
            settings
                .preview_with(
                    SettingsPatch {
                        base_url: Some(url.into()),
                        api_key: key.map(str::to_owned),
                        ..Default::default()
                    },
                    Provider::Openai,
                    env,
                )
                .unwrap()
        };
        let groq = "https://api.groq.com/openai/v1";
        assert!(preview(groq, None).api_key.is_none());
        assert_eq!(
            preview(groq, Some("new-key")).api_key.as_deref(),
            Some("new-key")
        );
        assert_eq!(
            preview(&format!("{router}/"), None).api_key.as_deref(),
            Some("env-router-key")
        );
    }

    #[test]
    fn mock_rejects_profile_fields() {
        let mut settings = Settings {
            provider: Provider::Mock,
            ..Default::default()
        };
        assert!(
            settings
                .apply(SettingsPatch {
                    model: Some("different".into()),
                    ..Default::default()
                })
                .is_err()
        );
    }

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
            .resolve_with(Some(Provider::Openai), |name| match name {
                "OPENAI_API_KEY" => Ok(Some("primary-key".into())),
                "OPENAI_BASE_URL" => Ok(Some(crate::openrouter::OPENROUTER_URL.into())),
                _ => get(name),
            })
            .unwrap();
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
    }

    #[test]
    fn view_reports_env_overrides_and_load_includes_path_on_error() {
        let settings = Settings::default();
        let view = settings
            .view_with(None, |name| {
                Ok(match name {
                    "OPENAI_BASE_URL" => Some("https://api.deepseek.com/v1".into()),
                    "OPENAI_RESPONSE_FORMAT" => Some("json_object".into()),
                    "GEMINI_MODEL" => Some("gemini-2.5-flash".into()),
                    _ => None,
                })
            })
            .unwrap();
        assert_eq!(
            view.openai.env_overrides,
            vec!["OPENAI_BASE_URL", "OPENAI_RESPONSE_FORMAT"]
        );
        assert_eq!(view.gemini.env_overrides, vec!["GEMINI_MODEL"]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{broken").unwrap();
        let err = Settings::load(&path).err().unwrap();
        assert!(err.to_string().contains("config.json"));
    }
}
