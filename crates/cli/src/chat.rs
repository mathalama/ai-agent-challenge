use std::{
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use agent::{
    AgentEvent, ExportFormat, Input, Provider, Result, Session, build_provider, read_answers,
};
use review::ProgressSink;

use crate::{progress::ConsoleProgress, same_path, write_output};

pub async fn run(input: Option<PathBuf>, provider: Provider, no_cache: bool) -> Result<()> {
    let mut session =
        Session::new(move |trace, reasoning| build_provider(provider, no_cache, trace, reasoning));
    let mut path = None;
    let progress = Arc::new(ConsoleProgress::default());
    print_event(Session::greeting(), None, &progress)?;
    if let Some(input) = input {
        load(&mut session, input, &mut path, &progress).await;
    }

    let mut stdin = io::stdin().lock();
    let mut line = String::new();
    loop {
        print!("> ");
        io::stdout().flush()?;
        line.clear();
        if stdin.read_line(&mut line)? == 0 || line.trim() == "/exit" {
            println!();
            break;
        }
        let text = line.trim();
        if let Some(("/load", name)) = text.split_once(char::is_whitespace) {
            let name = name.trim();
            let name = name
                .strip_prefix('"')
                .and_then(|name| name.strip_suffix('"'))
                .unwrap_or(name);
            load(&mut session, name.into(), &mut path, &progress).await;
        } else if !text.is_empty() {
            handle(&mut session, Input::Text(text), path.as_deref(), &progress).await;
        }
    }
    Ok(())
}

async fn load(
    session: &mut Session,
    input: PathBuf,
    path: &mut Option<PathBuf>,
    progress: &Arc<ConsoleProgress>,
) {
    let prepared = (|| {
        let answers_path = sidecar(&input)?;
        let bytes = fs::read(&input)
            .map_err(|error| format!("Не удалось прочитать «{}»: {error}", input.display()))?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((bytes, read_answers(&answers_path)?))
    })();
    match prepared {
        Ok((bytes, answers)) => {
            if handle(
                session,
                Input::Document {
                    name: &input,
                    bytes: &bytes,
                    answers,
                },
                Some(&input),
                progress,
            )
            .await
            {
                *path = Some(input);
            }
        }
        Err(error) => eprintln!("Ошибка: {error}"),
    }
}

async fn handle(
    session: &mut Session,
    input: Input<'_>,
    path: Option<&Path>,
    progress: &Arc<ConsoleProgress>,
) -> bool {
    let progress = Arc::clone(progress);
    let path = path.map(Path::to_path_buf);
    let error = Arc::new(AtomicBool::new(false));
    let error_flag = Arc::clone(&error);
    session
        .handle(input, move |event| {
            if matches!(event, AgentEvent::Error { .. }) {
                error_flag.store(true, Ordering::Relaxed);
            }
            if let Err(error) = print_event(event, path.as_deref(), &progress) {
                error_flag.store(true, Ordering::Relaxed);
                eprintln!("Ошибка: {error}");
            }
        })
        .await;
    !error.load(Ordering::Relaxed)
}

fn sidecar(path: &Path) -> Result<PathBuf> {
    let answers = path.with_extension("answers.json");
    if same_path(path, &answers)? {
        return Err("Файл ответов совпадает с путём документа".into());
    }
    Ok(answers)
}

fn print_event(event: AgentEvent, path: Option<&Path>, progress: &ConsoleProgress) -> Result<()> {
    match event {
        AgentEvent::Progress { progress: event } => {
            progress.emit(&event);
            return Ok(());
        }
        AgentEvent::Reasoning {
            stage,
            delta,
            complete,
        } => {
            progress.reasoning(&stage, &delta, complete);
            return Ok(());
        }
        AgentEvent::StateChanged { .. } => return Ok(()),
        _ => progress.flush_reasoning(),
    }
    match event {
        AgentEvent::Message {
            text, citations, ..
        } => {
            println!("{}", terminal_text(&text));
            for citation in citations {
                let location = citation.location;
                let page = location
                    .page
                    .map_or_else(String::new, |page| format!(", стр. {page}"));
                println!(
                    "  «{}» (строки {}–{}{page})",
                    terminal_text(&citation.quote),
                    location.start_line,
                    location.end_line
                );
            }
        }
        AgentEvent::ReportReady { summary, .. } => println!(
            "Отчёт готов. Требований — {}, находок — {}. Доступны /verdict, /findings, /questions и /export md|json.",
            summary.requirement_count, summary.finding_count,
        ),
        AgentEvent::Question { question } => {
            println!("{} — {}", question.id, terminal_text(&question.text));
            for option in question.options {
                println!("  • {}", terminal_text(&option));
            }
        }
        AgentEvent::AnswersUpdated { answers } => {
            let path = sidecar(path.ok_or("Путь к документу отсутствует")?)?;
            write_output(&path, &(serde_json::to_string_pretty(&answers)? + "\n"))?;
            println!("Ответы записаны в «{}».", path.display());
        }
        AgentEvent::Export { format, content } => {
            let output = Path::new(match format {
                ExportFormat::Md => "reports/report.md",
                ExportFormat::Json => "reports/report.json",
            });
            if let Some(input) = path
                && same_path(input, output)?
            {
                return Err("Выходной файл совпадает с исходным документом".into());
            }
            write_output(output, &content)?;
            println!("Отчёт записан в «{}».", output.display());
        }
        AgentEvent::Error { text } => eprintln!("Ошибка: {}", terminal_text(&text)),
        _ => {}
    }
    Ok(())
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect()
}
