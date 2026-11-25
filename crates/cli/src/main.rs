mod ask;
mod chat;
mod progress;

use std::{
    fs,
    io::{self, IsTerminal, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use agent::{Answers, Result, build_provider, progress_trace, read_answers};
use clap::{
    Parser, Subcommand, ValueEnum,
    error::{ContextKind, ErrorKind},
};
use doc::ParserRegistry;
use review::{
    AGENT_VERSION, CheckRegistry, JsonRenderer, MarkdownRenderer, Pipeline, ProgressSink, Report,
    ReportRenderer, RunContext, discover_golden_files, evaluate_document, format_eval_report,
    load_golden_file,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "rs",
    bin_name = "rs",
    version = AGENT_VERSION,
    about = "Локальный агент проверки требований"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Открыть интерактивную чат-сессию.
    Chat {
        input: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "gemini")]
        provider: Provider,
        #[arg(long)]
        no_cache: bool,
    },
    /// Запустить локальный веб-сервер.
    Serve {
        #[arg(long, value_enum, default_value = "gemini")]
        provider: Provider,
        #[arg(long)]
        no_cache: bool,
        #[arg(long, default_value = "127.0.0.1:3333")]
        listen: SocketAddr,
        #[arg(long, default_value = "web/dist")]
        web_dir: PathBuf,
    },
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
        /// Сохранять рассуждения LLM в кэше и трассе.
        #[arg(long)]
        reasoning: bool,
        /// Писать события прогресса в JSON Lines по мере выполнения.
        #[arg(long)]
        events: Option<PathBuf>,
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
    /// Вывести JSON Schema отчёта.
    Schema {
        /// Записать схему в файл вместо стандартного вывода.
        #[arg(short, long)]
        output: Option<PathBuf>,
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
    /// Любой API, совместимый с OpenAI Chat Completions (OPENAI_BASE_URL).
    Openai,
    Mock,
}

impl From<Provider> for agent::Provider {
    fn from(provider: Provider) -> Self {
        match provider {
            Provider::Gemini => Self::Gemini,
            Provider::Openai => Self::Openai,
            Provider::Mock => Self::Mock,
        }
    }
}

#[tokio::main]
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
    if cli.command.is_none() && !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        eprintln!(
            "Укажите команду. Для чата используйте rs chat. Справка доступна через rs --help."
        );
        return ExitCode::from(2);
    }
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

    match cli.command.unwrap_or(Command::Chat {
        input: None,
        provider: Provider::Gemini,
        no_cache: false,
    }) {
        Command::Chat {
            input,
            provider,
            no_cache,
        } => chat::run(input, provider.into(), no_cache).await,
        Command::Serve {
            provider,
            no_cache,
            listen,
            web_dir,
        } => server::serve(listen, provider.into(), no_cache, web_dir).await,
        Command::Check {
            input,
            output,
            format,
            provider,
            no_cache,
            trace,
            reasoning,
            events,
        } => {
            let options = RunOptions {
                trace: trace.as_deref(),
                reasoning,
                events: events.as_deref(),
            };
            check(&input, output, format, provider, no_cache, options).await
        }
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
        Command::Schema { output } => {
            let schema = serde_json::to_string_pretty(&Report::json_schema())? + "\n";
            if let Some(path) = output {
                write_output(&path, &schema)?;
            } else {
                io::stdout().write_all(schema.as_bytes())?;
            }
            Ok(())
        }
    }
}

async fn check(
    input: &Path,
    output: Option<PathBuf>,
    format: Format,
    provider: Provider,
    no_cache: bool,
    options: RunOptions<'_>,
) -> Result<()> {
    let output = output.unwrap_or_else(|| {
        PathBuf::from(match format {
            Format::Md => "reports/report.md",
            Format::Json => "reports/report.json",
        })
    });
    let answers_path = answers_path(input, &output)?;
    for (path, label) in [(options.trace, "трассы"), (options.events, "событий")] {
        if let Some(path) = path
            && (same_path(path, input)?
                || same_path(path, &output)?
                || same_path(path, &answers_path)?)
        {
            return Err(format!(
                "Для {label} укажите путь, отличный от путей документа, отчёта и ответов"
            )
            .into());
        }
    }
    if let (Some(trace), Some(events)) = (options.trace, options.events)
        && same_path(trace, events)?
    {
        return Err("Для трассы и событий укажите разные пути".into());
    }

    let answers = read_answers(&answers_path)?;
    let report = review(input, provider, no_cache, options, &answers).await?;
    let rendered = match format {
        Format::Md => MarkdownRenderer.render(&report)?,
        Format::Json => JsonRenderer.render(&report)?,
    };
    write_output(&output, &rendered)?;
    println!("Отчёт записан в «{}»", output.display());
    Ok(())
}

/// Параметры записи трассы и событий прогона.
#[derive(Clone, Copy, Default)]
struct RunOptions<'a> {
    trace: Option<&'a Path>,
    reasoning: bool,
    events: Option<&'a Path>,
}

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

async fn review(
    input: &Path,
    provider: Provider,
    no_cache: bool,
    options: RunOptions<'_>,
    answers: &Answers,
) -> Result<Report> {
    let bytes = fs::read(input)
        .map_err(|error| format!("Не удалось прочитать «{}»: {error}", input.display()))?;
    let document = ParserRegistry::default().parse(input, &bytes)?;
    let sink = progress_sink(options.events)?;
    let trace_log = progress_trace(Arc::clone(&sink));
    let provider = build_provider(
        provider.into(),
        no_cache,
        trace_log.clone(),
        options.reasoning,
    )?;
    let run = agent::review(
        document,
        provider.as_ref(),
        sink.as_ref(),
        trace_log.clone(),
        answers,
    )
    .await;
    if let Some(path) = options.trace {
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "stages": run.stages,
            "llm_calls": trace_log.entries(),
        }))? + "\n";
        write_output(path, &json)?;
    }
    let report = run.report?;
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

    let sink = progress_sink(None)?;
    let trace_log = progress_trace(Arc::clone(&sink));
    let provider = build_provider(provider_kind.into(), no_cache, trace_log.clone(), false)?;
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
        let mut ctx = RunContext::new(document, provider.as_ref(), &checks)
            .with_progress(sink.as_ref(), trace_log.clone());
        pipeline.run(&mut ctx).await?;
        results.push(evaluate_document(&item, &ctx.requirements, &ctx.findings));
    }

    print!("{}", format_eval_report(&results));
    Ok(())
}

/// Прогресс всегда печатается в stderr, а при `--events` ещё и пишется в файл.
fn progress_sink(events: Option<&Path>) -> Result<Arc<dyn ProgressSink>> {
    let mut sinks: Vec<Box<dyn ProgressSink>> =
        vec![Box::new(progress::ConsoleProgress::default())];
    if let Some(path) = events {
        let file = progress::JsonLinesProgress::create(path).map_err(|error| {
            format!(
                "Не удалось создать файл событий «{}»: {error}",
                path.display()
            )
        })?;
        sinks.push(Box::new(file));
    }
    Ok(Arc::new(progress::Fanout(sinks)))
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
