//! Вывод событий прогресса: строки в stderr и JSON Lines в файл.

use std::{
    fs::File,
    io::{self, BufWriter, Write},
    path::Path,
    sync::Mutex,
};

use review::{ProgressEvent, ProgressSink};

/// Печатает ход прогона в stderr, чтобы stdout оставался для результатов.
pub struct ConsoleProgress;

impl ProgressSink for ConsoleProgress {
    fn emit(&self, event: &ProgressEvent) {
        if let Some(line) = console_line(event) {
            eprintln!("{line}");
        }
    }
}

fn console_line(event: &ProgressEvent) -> Option<String> {
    Some(match event {
        ProgressEvent::RunStarted {
            document,
            model,
            stages,
        } => format!(
            "Проверка «{document}»: модель {model}, стадий {}",
            stages.len()
        ),
        ProgressEvent::StageStarted {
            stage,
            index,
            total,
        } => format!("[{index}/{total}] {stage}…"),
        ProgressEvent::StageFinished {
            stage,
            index,
            total,
            duration_ms,
            succeeded,
            llm_calls,
            cached_calls,
            input_tokens,
            output_tokens,
            requirements,
            findings,
            questions,
        } => {
            let status = if *succeeded {
                "готово"
            } else {
                "ошибка"
            };
            let llm = if *llm_calls == 0 {
                "без LLM".to_owned()
            } else if cached_calls == llm_calls {
                "из кэша".to_owned()
            } else {
                format!(
                    "токены {} → {}",
                    group_digits(*input_tokens),
                    group_digits(*output_tokens)
                )
            };
            format!(
                "[{index}/{total}] {stage}: {status} за {}, {llm}; требований {requirements}, находок {findings}, вопросов {questions}",
                format_duration(*duration_ms)
            )
        }
        ProgressEvent::LlmRetry {
            stage,
            reason,
            status,
            delay_ms,
        } => {
            let cause = match (reason, status) {
                (_, Some(status)) => format!("HTTP {status}"),
                (&"transport", None) => "сетевая ошибка".to_owned(),
                (&"invalid_response", None) => "ответ не по схеме".to_owned(),
                (reason, None) => (*reason).to_owned(),
            };
            format!(
                "      {stage}: {cause}, повтор через {}",
                format_duration(*delay_ms)
            )
        }
        ProgressEvent::FactBatchSplit { source_ids } => format!(
            "      facts: превышен лимит ответа, делим пачку из {} источников ({})",
            source_ids.len(),
            source_ids.join(", ")
        ),
        ProgressEvent::FactSourceSkipped {
            source_id,
            location,
        } => {
            let page = location
                .page
                .map_or_else(String::new, |page| format!(", стр. {page}"));
            format!(
                "      facts: предупреждение — источник {source_id} пропущен (строки {}–{}{page}, превышен лимит ответа)",
                location.start_line, location.end_line,
            )
        }
        ProgressEvent::RunFinished {
            duration_ms,
            succeeded: true,
        } => format!("Прогон завершён за {}", format_duration(*duration_ms)),
        ProgressEvent::RunFinished {
            succeeded: false, ..
        } => return None,
    })
}

fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        let tenths = ms.div_ceil(100);
        format!("{},{} с", tenths / 10, tenths % 10)
    } else {
        let seconds = ms / 1000;
        format!("{} мин {} с", seconds / 60, seconds % 60)
    }
}

fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    grouped
}

/// Пишет каждое событие отдельной строкой JSON и сразу сбрасывает буфер,
/// чтобы файл можно было читать во время прогона.
pub struct JsonLinesProgress(Mutex<BufWriter<File>>);

impl JsonLinesProgress {
    pub fn create(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self(Mutex::new(BufWriter::new(File::create(path)?))))
    }
}

impl ProgressSink for JsonLinesProgress {
    fn emit(&self, event: &ProgressEvent) {
        let mut writer = self.0.lock().expect("блокировка файла событий");
        let written = serde_json::to_writer(&mut *writer, event)
            .map_err(io::Error::from)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush());
        if let Err(error) = written {
            eprintln!("Предупреждение: не удалось записать событие прогресса: {error}");
        }
    }
}

/// Передаёт событие всем получателям по порядку.
pub struct Fanout(pub Vec<Box<dyn ProgressSink>>);

impl ProgressSink for Fanout {
    fn emit(&self, event: &ProgressEvent) {
        for sink in &self.0 {
            sink.emit(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_lines_are_readable() {
        let finished = ProgressEvent::StageFinished {
            stage: "requirement_checks",
            index: 4,
            total: 9,
            duration_ms: 38_150,
            succeeded: true,
            llm_calls: 1,
            cached_calls: 0,
            input_tokens: 7_100,
            output_tokens: 1_600,
            requirements: 20,
            findings: 12,
            questions: 0,
        };
        assert_eq!(
            console_line(&finished).unwrap(),
            "[4/9] requirement_checks: готово за 38,2 с, токены 7 100 → 1 600; требований 20, находок 12, вопросов 0"
        );
        let retry = ProgressEvent::LlmRetry {
            stage: "segment".into(),
            reason: "http",
            status: Some(503),
            delay_ms: 15_000,
        };
        assert_eq!(
            console_line(&retry).unwrap(),
            "      segment: HTTP 503, повтор через 15,0 с"
        );
        assert_eq!(format_duration(192_400), "3 мин 12 с");
        assert_eq!(group_digits(1_234_567), "1 234 567");
    }
}
