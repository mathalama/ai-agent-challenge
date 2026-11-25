use std::{
    io::{self, BufRead, Write},
    path::Path,
};

use review::RelatedId;

use crate::{Provider, Result, answers_path, read_answers, review, write_output};

pub(crate) async fn run(
    input: &Path,
    output: &Path,
    provider: Provider,
    no_cache: bool,
) -> Result<()> {
    let path = answers_path(input, output)?;
    let mut answers = read_answers(&path)?;
    let report = review(input, provider, no_cache, None, &answers).await?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    if report.questions.is_empty() {
        writeln!(stdout, "Открытых вопросов нет.")?;
        return Ok(());
    }

    let mut line = String::new();
    let mut saved = false;
    for question in report.questions {
        writeln!(
            stdout,
            "\n{} — {} · {}\n{}",
            question.id,
            question.topic.russian_name(),
            question.priority.russian_name(),
            terminal_text(&question.text),
        )?;
        let findings = question
            .related
            .iter()
            .filter_map(|related| match related {
                RelatedId::Finding(id) => Some(terminal_text(id)),
                RelatedId::Requirement(_) => None,
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(stdout, "Находки — {findings}")?;
        for evidence in &question.evidence {
            writeln!(
                stdout,
                "  «{}» (строки {}–{})",
                terminal_text(&evidence.quote),
                evidence.location.start_line,
                evidence.location.end_line,
            )?;
        }
        for (index, option) in question.options.iter().enumerate() {
            writeln!(stdout, "  {}. {}", index + 1, terminal_text(option))?;
        }
        if question.options.is_empty() {
            write!(stdout, "Введите ответ или нажмите Enter для пропуска > ")?;
        } else {
            write!(
                stdout,
                "Введите номер варианта или свой ответ. Enter — пропустить > "
            )?;
        }
        stdout.flush()?;

        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            writeln!(stdout)?;
            break;
        }
        let answer = line.trim();
        if answer.is_empty() {
            continue;
        }
        let answer = answer
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| question.options.get(index))
            .map_or(answer, String::as_str);
        answers.insert(question.id, answer.to_owned());
        write_output(&path, &(serde_json::to_string_pretty(&answers)? + "\n"))?;
        saved = true;
    }
    if saved {
        writeln!(stdout, "Ответы записаны в «{}».", path.display())?;
    }
    Ok(())
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}
