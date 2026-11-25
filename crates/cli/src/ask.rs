use std::{
    io::{self, BufRead, Write},
    path::Path,
};

use review::RelatedId;

use crate::{Provider, Result, RunOptions, answers_path, read_answers, review, write_output};

pub(crate) async fn run(
    input: &Path,
    output: &Path,
    provider: Provider,
    no_cache: bool,
) -> Result<()> {
    let path = answers_path(input, output)?;
    let mut answers = read_answers(&path)?;
    let report = review(input, provider, no_cache, RunOptions::default(), &answers).await?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    if report.questions.is_empty() {
        writeln!(stdout, "Открытых вопросов нет.")?;
        return Ok(());
    }

    let mut line = String::new();
    let mut saved = false;
    'questions: for question in report.questions {
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
        if !findings.is_empty() {
            writeln!(stdout, "Находки — {findings}")?;
        }
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
        let numeric_choices = question.options.iter().all(|option| is_integer(option));
        let answer = loop {
            if question.options.is_empty() {
                write!(stdout, "Введите ответ или нажмите Enter для пропуска > ")?;
            } else if numeric_choices {
                write!(
                    stdout,
                    "Введите свой ответ или выберите вариант через #N (например, #1). Enter — пропустить > "
                )?;
            } else {
                write!(
                    stdout,
                    "Введите номер варианта или свой ответ (для целого числа — =число). Enter — пропустить > "
                )?;
            }
            stdout.flush()?;

            line.clear();
            if stdin.read_line(&mut line)? == 0 {
                writeln!(stdout)?;
                break 'questions;
            }
            let input = line.trim();
            if input.is_empty() {
                continue 'questions;
            }
            if let Some(answer) = parse_answer(input, &question.options, numeric_choices) {
                break answer;
            }
            writeln!(
                stdout,
                "Допустимые номера — от 1 до {}.",
                question.options.len()
            )?;
        };
        answers.insert(question.id, answer.to_owned());
        write_output(&path, &(serde_json::to_string_pretty(&answers)? + "\n"))?;
        saved = true;
    }
    if saved {
        writeln!(stdout, "Ответы записаны в «{}».", path.display())?;
    }
    Ok(())
}

fn parse_answer<'a>(
    input: &'a str,
    options: &'a [String],
    numeric_choices: bool,
) -> Option<&'a str> {
    if options.is_empty() {
        return Some(input);
    }
    if let Some(value) = input.strip_prefix('=').filter(|value| is_integer(value)) {
        return Some(value);
    }
    let index = input
        .strip_prefix('#')
        .filter(|value| is_integer(value))
        .or_else(|| (!numeric_choices && is_integer(input)).then_some(input));
    match index {
        Some(number) => number
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| options.get(index))
            .map(String::as_str),
        None => Some(input),
    }
}

fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_choices_accept_a_bare_numeric_answer_and_use_explicit_indices() {
        let options = ["5000".to_owned(), "10000".to_owned()];
        assert_eq!(parse_answer("15000", &options, true), Some("15000"));
        assert_eq!(parse_answer("1", &options, true), Some("1"));
        assert_eq!(parse_answer("#2", &options, true), Some("10000"));
        assert_eq!(parse_answer("#3", &options, true), None);
    }

    #[test]
    fn text_choices_keep_index_validation_and_allow_an_explicit_numeric_answer() {
        let options = ["Да".to_owned(), "Нет".to_owned()];
        assert_eq!(parse_answer("2", &options, false), Some("Нет"));
        assert_eq!(parse_answer("3", &options, false), None);
        assert_eq!(parse_answer("0", &options, false), None);
        assert_eq!(parse_answer("=15000", &options, false), Some("15000"));
    }
}
