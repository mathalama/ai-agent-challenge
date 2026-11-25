use std::{
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use chrono::Local;
use clap::{
    Parser, Subcommand, ValueEnum,
    error::{ContextKind, ErrorKind},
};
use doc::ParserRegistry;
use llm::{
    CachedProvider, GeminiProvider, LlmProvider, MockProvider, RateLimitedProvider, RetryProvider,
    TraceLog, TracingProvider,
};
use review::{
    AGENT_VERSION, CheckRegistry, JsonRenderer, MarkdownRenderer, Pipeline, Report, ReportRenderer,
    RunContext, RunPassport,
};
use tracing_subscriber::EnvFilter;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(
    name = "rs",
    bin_name = "rs",
    version = AGENT_VERSION,
    about = "Локальный агент проверки требований"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Проверить документ и записать отчёт.
    Check {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "md")]
        format: Format,
        #[arg(long, value_enum, default_value = "gemini")]
        provider: Provider,
        #[arg(long)]
        no_cache: bool,
        #[arg(long)]
        trace: Option<PathBuf>,
    },
    /// Задать уточняющие вопросы автору документа.
    Ask { input: PathBuf },
    /// Оценить качество агента на эталонной разметке.
    Eval {
        #[arg(long, default_value = "eval/")]
        golden: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Md,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    Gemini,
    Mock,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            let reason = match error.kind() {
                ErrorKind::UnknownArgument => "Неизвестный аргумент",
                ErrorKind::InvalidSubcommand => "Неизвестная команда",
                ErrorKind::MissingRequiredArgument => "Отсутствует обязательный аргумент",
                ErrorKind::MissingSubcommand
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    "Укажите команду check, ask или eval"
                }
                ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
                    "Недопустимое значение аргумента"
                }
                ErrorKind::ArgumentConflict => "Указаны несовместимые аргументы",
                ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
                    "Укажите значение аргумента"
                }
                _ => "Не удалось разобрать аргументы",
            };
            let detail = [
                ContextKind::InvalidSubcommand,
                ContextKind::InvalidArg,
                ContextKind::InvalidValue,
            ]
            .into_iter()
            .find_map(|kind| error.get(kind))
            .map_or_else(String::new, |value| format!(" — {value}"));
            eprintln!("Ошибка: {reason}{detail}. Для справки используйте --help.");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = run(cli).await {
        eprintln!("Ошибка: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run(cli: Cli) -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(error) if error.not_found() => {}
        Err(dotenvy::Error::LineParse(..)) => return Err("Некорректная запись в .env".into()),
        Err(_) => return Err("Не удалось прочитать .env".into()),
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(io::stderr)
        .init();

    match cli.command {
        Command::Check {
            input,
            output,
            format,
            provider,
            no_cache,
            trace,
        } => check(&input, output, format, provider, no_cache, trace.as_deref()).await,
        Command::Ask { input } => {
            // TODO(issue): провести интерактивный диалог и сохранить ответы.
            Err(format!("Команда ask для «{}» пока в разработке", input.display()).into())
        }
        Command::Eval { golden } => {
            // TODO(issue): рассчитать метрики на эталонных данных.
            Err(format!("Команда eval для «{}» пока в разработке", golden.display()).into())
        }
    }
}

async fn check(
    input: &Path,
    output: Option<PathBuf>,
    format: Format,
    provider: Provider,
    no_cache: bool,
    trace: Option<&Path>,
) -> Result<()> {
    let output = output.unwrap_or_else(|| {
        PathBuf::from(match format {
            Format::Md => "reports/report.md",
            Format::Json => "reports/report.json",
        })
    });
    if same_path(input, &output)? {
        return Err("Выходной файл совпадает с исходным документом".into());
    }
    if let Some(path) = trace
        && (same_path(path, input)? || same_path(path, &output)?)
    {
        return Err("Для трассы укажите путь, отличный от путей документа и отчёта".into());
    }

    let bytes = fs::read(input)
        .map_err(|error| format!("Не удалось прочитать «{}»: {error}", input.display()))?;
    let document = ParserRegistry::default().parse(input, &bytes)?;
    let provider: Box<dyn LlmProvider> = match provider {
        Provider::Mock => Box::new(MockProvider),
        Provider::Gemini => Box::new(RateLimitedProvider::from_env(Box::new(
            GeminiProvider::from_env()?,
        ))?),
    };
    let trace_log = TraceLog::default();
    let mut provider: Box<dyn LlmProvider> = Box::new(RetryProvider::new(provider));
    if !no_cache {
        provider = Box::new(CachedProvider::new(provider, ".cache/llm"));
    }
    let provider = TracingProvider::new(provider, trace_log.clone());
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &provider, &checks);
    let result = Pipeline::default().run(&mut ctx).await;
    if let Some(path) = trace {
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "stages": ctx.trace,
            "llm_calls": trace_log.entries(),
        }))? + "\n";
        write_output(path, &json)?;
    }
    result?;

    let passport = RunPassport {
        document_title: ctx.document.title.clone(),
        date: Local::now().date_naive().to_string(),
        model: provider.model().into(),
        agent_version: AGENT_VERSION.into(),
    };
    let report = Report::from_run(ctx, passport);
    let rendered = match format {
        Format::Md => MarkdownRenderer.render(&report)?,
        Format::Json => JsonRenderer.render(&report)?,
    };
    write_output(&output, &rendered)?;
    println!("Отчёт записан в «{}»", output.display());
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> io::Result<bool> {
    fn resolve(path: &Path) -> io::Result<PathBuf> {
        if let Ok(canonical) = path.canonicalize() {
            return Ok(canonical);
        }
        let absolute = std::path::absolute(path)?;
        for parent in absolute.ancestors().skip(1) {
            if let Ok(canonical) = parent.canonicalize() {
                let suffix = absolute.strip_prefix(parent).expect("родительский путь");
                return Ok(canonical.join(suffix).components().fold(
                    PathBuf::new(),
                    |mut path, component| {
                        if component == std::path::Component::ParentDir {
                            path.pop();
                        } else {
                            path.push(component);
                        }
                        path
                    },
                ));
            }
        }
        Ok(absolute)
    }
    let left = resolve(left)?;
    let right = resolve(right)?;
    if cfg!(windows) {
        Ok(left
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy()))
    } else {
        Ok(left == right)
    }
}

fn write_output(path: &Path, content: &str) -> Result<()> {
    let write = || -> io::Result<()> {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(content.as_bytes())?;
        temporary.persist(path)?;
        Ok(())
    };
    write().map_err(|error| format!("Не удалось записать «{}»: {error}", path.display()).into())
}
