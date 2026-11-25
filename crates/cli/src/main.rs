mod ask;

use std::{
    collections::BTreeMap,
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
    TraceLog,
};
use review::{
    AGENT_VERSION, CheckRegistry, JsonRenderer, MarkdownRenderer, Pipeline, Report, ReportRenderer,
    RunContext, RunPassport, discover_golden_files, evaluate_document, format_eval_report,
    load_golden_file,
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
    Ask {
        input: PathBuf,
        /// Путь к отчёту, рядом с которым сохраняются ответы.
        #[arg(short, long, default_value = "reports/report.md")]
        output: PathBuf,
        #[arg(long, value_enum, default_value = "gemini")]
        provider: Provider,
        #[arg(long)]
        no_cache: bool,
    },
    /// Оценить качество агента на эталонной разметке.
    Eval {
        #[arg(long, default_value = "eval/")]
        golden: PathBuf,
        #[arg(long, value_enum, default_value = "gemini")]
        provider: Provider,
        #[arg(long)]
        no_cache: bool,
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
        Command::Ask {
            input,
            output,
            provider,
            no_cache,
        } => ask::run(&input, &output, provider, no_cache).await,
        Command::Eval {
            golden,
            provider,
            no_cache,
        } => eval_golden(&golden, provider, no_cache).await,
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
    let answers_path = answers_path(input, &output)?;
    if let Some(path) = trace
        && (same_path(path, input)? || same_path(path, &output)? || same_path(path, &answers_path)?)
    {
        return Err(
            "Для трассы укажите путь, отличный от путей документа, отчёта и ответов".into(),
        );
    }

    let answers = read_answers(&answers_path)?;
    let report = review(input, provider, no_cache, trace, &answers).await?;
    let rendered = match format {
        Format::Md => MarkdownRenderer.render(&report)?,
        Format::Json => JsonRenderer.render(&report)?,
    };
    write_output(&output, &rendered)?;
    println!("Отчёт записан в «{}»", output.display());
    Ok(())
}

type Answers = BTreeMap<String, String>;

fn answers_path(input: &Path, output: &Path) -> Result<PathBuf> {
    if same_path(input, output)? {
        return Err("Выходной файл совпадает с исходным документом".into());
    }
    let answers = output.with_extension("answers.json");
    if same_path(input, &answers)? || same_path(output, &answers)? {
        return Err("Файл ответов совпадает с путём документа или отчёта".into());
    }
    Ok(answers)
}

fn read_answers(path: &Path) -> Result<Answers> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            format!("Некорректный файл ответов «{}»: {error}", path.display()).into()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Answers::new()),
        Err(error) => Err(format!("Не удалось прочитать «{}»: {error}", path.display()).into()),
    }
}

async fn review(
    input: &Path,
    provider: Provider,
    no_cache: bool,
    trace: Option<&Path>,
    answers: &Answers,
) -> Result<Report> {
    let bytes = fs::read(input)
        .map_err(|error| format!("Не удалось прочитать «{}»: {error}", input.display()))?;
    let document = ParserRegistry::default().parse(input, &bytes)?;
    let trace_log = TraceLog::default();
    let provider = build_provider(provider, no_cache, Some(trace_log.clone()))?;
    let checks = CheckRegistry::with_builtin_rules();
    let mut ctx = RunContext::new(document, provider.as_ref(), &checks);
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
    let report = Report::with_answers(ctx, passport, answers);
    if !report.unmatched_answers.is_empty() {
        eprintln!(
            "Предупреждение: сохранённые ответы без текущего вопроса — {:?}.",
            report.unmatched_answers.keys().collect::<Vec<_>>(),
        );
    }
    Ok(report)
}

async fn eval_golden(golden: &Path, provider_kind: Provider, no_cache: bool) -> Result<()> {
    let golden_files = discover_golden_files(golden)?;
    let mut loaded = Vec::with_capacity(golden_files.len());
    for file in golden_files {
        loaded.push(load_golden_file(&file)?);
    }

    let provider = build_provider(provider_kind, no_cache, None)?;
    let registry = ParserRegistry::default();
    let checks = CheckRegistry::with_builtin_rules();
    let pipeline = Pipeline::default();

    let mut results = Vec::with_capacity(loaded.len());
    for item in loaded {
        let bytes = fs::read(&item.document_path).map_err(|error| {
            format!(
                "Не удалось прочитать «{}»: {error}",
                item.document_path.display()
            )
        })?;
        let document = registry.parse(&item.document_path, &bytes)?;
        let mut ctx = RunContext::new(document, provider.as_ref(), &checks);
        pipeline.run(&mut ctx).await?;
        results.push(evaluate_document(&item, &ctx.requirements, &ctx.findings));
    }

    print!("{}", format_eval_report(&results));
    Ok(())
}

fn build_provider(
    provider: Provider,
    no_cache: bool,
    trace_log: Option<TraceLog>,
) -> Result<Box<dyn LlmProvider>> {
    let base: Box<dyn LlmProvider> = match provider {
        Provider::Mock => Box::new(MockProvider),
        Provider::Gemini => Box::new(RateLimitedProvider::from_env(Box::new(
            GeminiProvider::from_env()?,
        ))?),
    };
    let retry = match trace_log {
        Some(log) => RetryProvider::new(base).with_trace(log),
        None => RetryProvider::new(base),
    };
    Ok(if no_cache {
        Box::new(retry)
    } else {
        Box::new(CachedProvider::new(retry, ".cache/llm"))
    })
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
