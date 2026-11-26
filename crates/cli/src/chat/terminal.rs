use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use agent::{Input, ProviderFactory, Result, Session};
use crossterm::{
    cursor::{MoveDown, MoveToColumn, MoveUp},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    queue,
    terminal::{self, Clear, ClearType},
};

use super::{ConsoleProgress, handle, load, load_path, terminal_text};

const COMMANDS: &[&str] = &[
    "/load",
    "/check",
    "/provider",
    "/model",
    "/config",
    "/findings",
    "/questions",
    "/export",
    "/ask",
    "/answer",
    "/verdict",
    "/reasoning",
    "/help",
    "/exit",
];

pub async fn run(
    session: &mut Session,
    input: Option<PathBuf>,
    factory: &ProviderFactory,
    config: Option<&Path>,
    progress: &Arc<ConsoleProgress>,
) -> Result<()> {
    let width = width();
    let provider = factory.resolved()?;
    let cwd = std::env::current_dir()?;
    let welcome = [
        format!("rs {} · Проверка требований", review::AGENT_VERSION),
        format!("{} · {}", provider.name(), display_model(&provider.model)),
        format!("Рабочая папка · {}", terminal_text(&cwd.to_string_lossy())),
        String::from("Введите / для подсказок · /load <путь> для загрузки"),
    ];
    println!("╭{}╮", "─".repeat(width - 2));
    for line in welcome {
        println!("│ {:<size$} │", clip(&line, width - 4), size = width - 4);
    }
    println!("╰{}╯", "─".repeat(width - 2));

    let mut document = None;
    let mut next_input = input;
    loop {
        if let Some(file) = next_input.take() {
            let cancelled =
                cancellable(load(session, file, &mut document, progress), progress).await?;
            if cancelled {
                interrupted();
            }
        }
        let provider = factory.resolved()?;
        let name = document
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy());
        let status = format!(
            "{} · {} · {} · {} токенов",
            provider.name(),
            display_model(&provider.model),
            terminal_text(name.as_deref().unwrap_or("без документа")),
            progress.tokens()
        );
        let Some(line) = read_boxed(&status)? else {
            break;
        };
        let text = line.trim();
        let (command, args) = text
            .split_once(char::is_whitespace)
            .map_or((text, ""), |(command, args)| (command, args.trim()));
        let result = match command {
            "" => continue,
            "/exit" => break,
            "/load" if !args.is_empty() => {
                next_input = Some(load_path(args));
                continue;
            }
            "/load" => {
                println!("Используйте /load <путь>.");
                continue;
            }
            "/provider" if args.is_empty() => {
                println!(
                    "Провайдер · {}. Доступны mock, gemini и openai.",
                    provider.name()
                );
                continue;
            }
            "/provider" => args
                .parse()
                .map_err(Into::into)
                .and_then(|kind| factory.switch_provider(kind)),
            "/model" if args.is_empty() => {
                println!(
                    "Модель · {}. Для смены используйте /model <ID>.",
                    display_model(&provider.model)
                );
                continue;
            }
            "/model" => factory.switch_model(args),
            "/config" => reconfigure(config, factory).await,
            "/help" => {
                println!("{}", COMMANDS.join("  "));
                super::print_event(Session::greeting(), None, progress)
            }
            _ => {
                if cancellable(
                    handle(session, Input::Text(text), document.as_deref(), progress),
                    progress,
                )
                .await?
                {
                    interrupted();
                }
                continue;
            }
        };
        if let Err(error) = result {
            eprintln!("Ошибка: {}", terminal_text(&error.to_string()));
        }
    }
    Ok(())
}

async fn reconfigure(path: Option<&Path>, factory: &ProviderFactory) -> Result<()> {
    let path = path.ok_or("Каталог профиля недоступен")?;
    let settings = crate::config::wizard(path, crate::config::load_or_recover(path)?).await?;
    factory.reload(settings);
    Ok(())
}

async fn cancellable<T>(
    operation: impl Future<Output = T>,
    progress: &ConsoleProgress,
) -> Result<bool> {
    let mut tick = tokio::time::interval(Duration::from_millis(110));
    tokio::pin!(operation);
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    let cancelled = loop {
        tokio::select! {
            biased;
            signal = &mut signal => { signal?; break true; }
            _ = &mut operation => break false,
            _ = tick.tick() => progress.tick(),
        }
    };
    progress.finish_activity();
    progress.flush_reasoning();
    Ok(cancelled)
}

fn interrupted() {
    println!("Прервано. Нажмите Ctrl+C ещё раз для выхода.");
}

struct RawMode;

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn read_boxed(status: &str) -> io::Result<Option<String>> {
    terminal::enable_raw_mode()?;
    let _raw = RawMode;
    let mut out = io::stdout().lock();
    let mut input: Vec<char> = Vec::new();
    let mut cursor = 0;
    loop {
        let width = width();
        let value: String = input.iter().collect();
        let matches = suggestions(&value);
        draw_box(&mut out, width, &input, cursor, status, &matches)?;
        let event = event::read()?;
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Enter => {
                queue!(
                    out,
                    MoveDown(4),
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine)
                )?;
                write!(out, "\r\n")?;
                out.flush()?;
                return Ok(Some(value));
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !input.is_empty() {
                    input.clear();
                    cursor = 0;
                    continue;
                }
                queue!(out, Clear(ClearType::FromCursorDown))?;
                out.flush()?;
                return Ok(None);
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
            {
                queue!(out, Clear(ClearType::FromCursorDown))?;
                out.flush()?;
                return Ok(None);
            }
            KeyCode::Tab if !matches.is_empty() => {
                input = format!("{} ", matches[0]).chars().collect();
                cursor = input.len();
            }
            KeyCode::Char(ch)
                if !ch.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.insert(cursor, ch);
                cursor += 1;
            }
            KeyCode::Backspace if cursor > 0 => {
                cursor -= 1;
                input.remove(cursor);
            }
            KeyCode::Delete if cursor < input.len() => {
                input.remove(cursor);
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => cursor = (cursor + 1).min(input.len()),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = input.len(),
            _ => {}
        }
    }
}

fn draw_box(
    out: &mut impl Write,
    width: usize,
    input: &[char],
    cursor: usize,
    status: &str,
    matches: &[&str],
) -> io::Result<()> {
    let max_input = width.saturating_sub(5);
    let start = cursor
        .saturating_sub(max_input / 2)
        .min(input.len().saturating_sub(max_input));
    let end = (start + max_input).min(input.len());
    let before: String = input[start..cursor].iter().collect();
    let current = input
        .get(cursor)
        .filter(|_| cursor < end)
        .copied()
        .unwrap_or(' ');
    let after: String = input[cursor.saturating_add(1).min(end)..end]
        .iter()
        .collect();
    let padding = max_input.saturating_sub(before.chars().count() + after.chars().count());
    queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
    write!(
        out,
        "╭{}╮\r\n│ {before}\x1b[7m{current}\x1b[0m{after}{} │\r\n╰{}╯\r\n{}",
        "─".repeat(width - 2),
        " ".repeat(padding),
        "─".repeat(width - 2),
        clip(status, width)
    )?;
    if !matches.is_empty() {
        write!(out, "\r\n{}", clip(&matches.join("  "), width))?;
    } else {
        write!(out, "\r\n")?;
    }
    queue!(out, MoveUp(4), MoveToColumn(0))?;
    out.flush()
}

fn suggestions(value: &str) -> Vec<&'static str> {
    if !value.starts_with('/') || value.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(value))
        .collect()
}

fn width() -> usize {
    usize::from(
        terminal::size()
            .map_or(80, |(width, _)| width)
            .clamp(12, 112),
    ) - 1
}

fn clip(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

fn display_model(model: &str) -> &str {
    if model.is_empty() {
        "модель не задана"
    } else {
        model
    }
}

#[cfg(test)]
mod tests {
    use super::draw_box;

    #[test]
    fn long_input_shows_the_character_under_the_cursor_and_its_tail() {
        let input: Vec<_> = "abcdefghijklmnopqrstuvwxyz012345".chars().collect();
        let mut output = Vec::new();
        draw_box(&mut output, 25, &input, 10, "", &[]).unwrap();
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("abcdefghij\x1b[7mk\x1b[0mlmnopqrst"));
    }
}
