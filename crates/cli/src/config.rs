use std::{
    io::{self, IsTerminal, Read, Write},
    path::Path,
};

use agent::Result;
use clap::Subcommand;
use llm::{
    openrouter::{self, NO_JSON_WARNING, OPENROUTER_URL},
    settings::{Provider, ResponseFormat, Settings, SettingsPatch},
};

#[derive(Subcommand)]
pub enum Action {
    /// Показать настройки с маской ключа.
    Show,
    /// Сохранить поле. Для api-key передайте ключ через stdin.
    Set {
        field: String,
        value: Option<String>,
    },
    /// Проверить соединение с текущим провайдером.
    Test,
}

pub async fn run(path: &Path, action: Option<Action>) -> Result<()> {
    let mut settings = Settings::load(path)?;
    match action {
        Some(Action::Show) => println!("{}", serde_json::to_string_pretty(&settings.view(None)?)?),
        Some(Action::Set { field, value }) => {
            let patch = field_patch(&field, value)?;
            settings.apply(patch)?;
            settings.save(path)?;
            println!("Настройки сохранены.");
        }
        Some(Action::Test) => println!("{}", llm::test_settings(&settings.resolve(None)?).await?),
        None => {
            wizard(path, settings).await?;
        }
    }
    Ok(())
}

pub async fn wizard(path: &Path, mut settings: Settings) -> Result<Settings> {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err("Для мастера нужен терминал. В пайпе используйте rs config set.".into());
    }
    let current = settings.resolve(None)?;
    println!("Настройка провайдера");
    let name = prompt(
        "Провайдер (mock / gemini / openai / openrouter / groq / deepseek / ollama)",
        &format!("{:?}", current.provider).to_ascii_lowercase(),
    )?;
    let (provider, preset, preset_format) = match name.to_ascii_lowercase().as_str() {
        "openrouter" => (
            Provider::Openai,
            Some(OPENROUTER_URL),
            ResponseFormat::JsonSchema,
        ),
        "groq" => (
            Provider::Openai,
            Some("https://api.groq.com/openai/v1"),
            ResponseFormat::JsonObject,
        ),
        "deepseek" => (
            Provider::Openai,
            Some("https://api.deepseek.com/v1"),
            ResponseFormat::JsonObject,
        ),
        "ollama" => (
            Provider::Openai,
            Some("http://localhost:11434/v1"),
            ResponseFormat::JsonObject,
        ),
        _ => (name.parse()?, None, ResponseFormat::JsonSchema),
    };
    let profile = settings.resolve(Some(provider))?;
    let mut patch = SettingsPatch {
        provider: Some(provider),
        ..Default::default()
    };
    if provider != Provider::Mock {
        let base_url = prompt("Адрес API", preset.unwrap_or(&profile.base_url))?;
        let use_router = provider == Provider::Openai && openrouter::is_openrouter(&base_url);
        patch.base_url = Some(base_url);
        let api_key = rpassword::prompt_password("API-ключ (Enter сохраняет текущий): ")?;
        if !api_key.trim().is_empty() {
            patch.api_key = Some(api_key);
        }
        let mut selected = None;
        if use_router {
            let free_only = yes(&prompt(
                "Только бесплатные (д/н)",
                if profile.free_only { "д" } else { "н" },
            )?);
            patch.free_only = Some(free_only);
            match openrouter::models(free_only).await {
                Ok(catalog) if !catalog.is_empty() => {
                    for (index, model) in catalog.iter().enumerate() {
                        println!(
                            "{:>3}. {} · {} токенов · {}",
                            index + 1,
                            model.id,
                            model.context_length,
                            format_label(model.response_format)
                        );
                    }
                    let choice = prompt("Введите номер модели или ID", &profile.model)?;
                    let matched = choice
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| index.checked_sub(1))
                        .and_then(|index| catalog.get(index))
                        .or_else(|| catalog.iter().find(|model| model.id == choice));
                    selected = matched.map(|model| model.response_format);
                    patch.model = Some(matched.map_or(choice, |model| model.id.clone()));
                }
                _ => println!("Каталог OpenRouter недоступен или пуст. Введите ID модели вручную."),
            }
        }
        if patch.model.is_none() {
            patch.model = Some(prompt("Модель", &profile.model)?);
        }
        if provider == Provider::Openai {
            let default = if preset.is_some() {
                preset_format
            } else {
                profile.response_format
            };
            let format = match selected {
                Some(format) => format,
                None => prompt(
                    "Формат ответа (json_schema / json_object / none)",
                    format_value(default),
                )?
                .parse()?,
            };
            if format == ResponseFormat::None {
                println!("{NO_JSON_WARNING}");
            }
            patch.response_format = Some(format);
        }
    }
    let resolved = settings.preview(patch.clone(), provider)?;
    settings.apply(patch)?;
    if provider != Provider::Mock {
        resolved.validate()?;
    }
    if yes(&prompt("Проверить соединение (д/н)", "д")?) {
        match llm::test_settings(&resolved).await {
            Ok(message) => println!("{message}"),
            Err(error) => eprintln!("Ошибка проверки: {error}"),
        }
    }
    settings.save(path)?;
    println!("Настройки сохранены.");
    Ok(settings)
}

fn field_patch(field: &str, value: Option<String>) -> Result<SettingsPatch> {
    let mut patch = SettingsPatch::default();
    if matches!(field, "api-key" | "api_key" | "key") {
        if value.is_some() {
            return Err("Передайте ключ через stdin или запустите мастер rs config.".into());
        }
        if io::stdin().is_terminal() {
            return Err("В терминале используйте защищённый ввод в мастере rs config.".into());
        }
        let mut key = String::new();
        io::stdin().take(8193).read_to_string(&mut key)?;
        if key.len() > 8192 {
            return Err("Слишком длинный API-ключ".into());
        }
        patch.api_key = Some(key);
        return Ok(patch);
    }
    let value = value.ok_or("Укажите значение поля")?;
    match field {
        "provider" => patch.provider = Some(value.parse()?),
        "model" => patch.model = Some(value),
        "base-url" | "base_url" => patch.base_url = Some(value),
        "response-format" | "response_format" => patch.response_format = Some(value.parse()?),
        "free-only" | "free_only" => patch.free_only = Some(value.parse()?),
        _ => {
            return Err(
                "Допустимые поля: provider, model, base-url, api-key, response-format, free-only"
                    .into(),
            );
        }
    }
    Ok(patch)
}

fn prompt(label: &str, default: &str) -> io::Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    io::stdout().flush()?;
    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Ввод прерван"));
    }
    let input = input.trim();
    Ok(if input.is_empty() { default } else { input }.to_owned())
}

fn yes(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "д" | "да" | "y" | "yes"
    )
}

fn format_value(format: ResponseFormat) -> &'static str {
    match format {
        ResponseFormat::JsonSchema => "json_schema",
        ResponseFormat::JsonObject => "json_object",
        ResponseFormat::None => "none",
    }
}

fn format_label(format: ResponseFormat) -> &'static str {
    match format {
        ResponseFormat::None => NO_JSON_WARNING,
        format => format_value(format),
    }
}
