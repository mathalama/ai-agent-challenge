use std::{collections::BTreeMap, path::Path, sync::Arc};

use doc::{Document, ParserRegistry};
use llm::{LlmProvider, TraceLog};
use review::{
    JsonRenderer, MarkdownRenderer, ProgressEvent, ProgressSink, Question, Readiness, Report,
    ReportRenderer, ReportSummary,
};
use serde::Serialize;

use crate::{Answers, Result, progress_trace, review};

const HELP: &str = "Загрузите документ командой /load <путь> или передайте файл. Для проверки введите /check. После проверки доступны /findings, /questions, /verdict и /export md|json. Для ответа введите /answer <ID> <ответ> или ответьте на текущий вопрос. Сохранение рассуждений переключается командой /reasoning on|off.";

pub enum Input<'a> {
    Text(&'a str),
    /// Байты предоставляет клиент. Имя используется для выбора парсера.
    Document {
        name: &'a Path,
        bytes: &'a [u8],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Empty,
    Loaded,
    Reviewing,
    Reviewed,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Assistant,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Md,
    Json,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    Message {
        role: Role,
        text: String,
    },
    Progress {
        progress: ProgressEvent,
    },
    ReportReady {
        summary: ReportSummary,
    },
    Question {
        question: Question,
    },
    AnswersUpdated {
        answers: Answers,
    },
    Export {
        format: ExportFormat,
        content: String,
    },
    Error {
        text: String,
    },
}

type ProviderFactory = dyn Fn(TraceLog, bool) -> Result<Box<dyn LlmProvider>> + Send + Sync;
type EventHandler = dyn Fn(AgentEvent) + Send + Sync;

pub struct Session {
    state: SessionState,
    document: Option<Document>,
    report: Option<Report>,
    answers: Answers,
    current_question: Option<String>,
    include_reasoning: bool,
    provider: Box<ProviderFactory>,
}

impl Session {
    /// Провайдер создаётся только при запуске проверки.
    pub fn new(
        provider: impl Fn(TraceLog, bool) -> Result<Box<dyn LlmProvider>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: SessionState::Empty,
            document: None,
            report: None,
            answers: Answers::new(),
            current_question: None,
            include_reasoning: true,
            provider: Box::new(provider),
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    pub fn greeting() -> AgentEvent {
        message(format!("Я проверяю документы с требованиями. {HELP}"))
    }

    /// События передаются по мере выполнения. Обработчик должен возвращаться быстро.
    pub async fn handle(
        &mut self,
        input: Input<'_>,
        emit: impl Fn(AgentEvent) + Send + Sync + 'static,
    ) {
        let emit: Arc<EventHandler> = Arc::new(emit);
        let result = match input {
            Input::Document { name, bytes } => self.load(name, bytes).map(|event| emit(event)),
            Input::Text(text) => self.handle_text(text.trim(), &emit).await,
        };
        if let Err(error) = result {
            emit(AgentEvent::Error {
                text: error.to_string(),
            });
        }
    }

    fn load(&mut self, name: &Path, bytes: &[u8]) -> Result<AgentEvent> {
        let document = ParserRegistry::default().parse(name, bytes)?;
        let mut last_line_by_page = BTreeMap::<_, usize>::new();
        for block in &document.blocks {
            let last_line = last_line_by_page.entry(block.location.page).or_default();
            *last_line = (*last_line).max(block.location.end_line);
        }
        let lines: usize = last_line_by_page.values().sum();
        let event = message(format!(
            "Документ «{}» загружен. Блоков — {}, строк — {lines}. Введите /check для проверки.",
            document.title,
            document.blocks.len()
        ));
        self.state = SessionState::Loaded;
        self.document = Some(document);
        self.report = None;
        self.answers.clear();
        self.current_question = None;
        Ok(event)
    }

    async fn handle_text(&mut self, text: &str, emit: &Arc<EventHandler>) -> Result<()> {
        let (command, arguments) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let arguments = arguments.trim();
        if self.document.is_none() || matches!(command, "/help" | "/load") {
            emit(message(HELP));
        } else if command == "/check" || command.to_lowercase() == "проверь" {
            self.check(Arc::clone(emit)).await?;
        } else if command == "/reasoning" {
            self.include_reasoning = match arguments {
                "on" => true,
                "off" => false,
                _ => return Err("Используйте /reasoning on или /reasoning off.".into()),
            };
            emit(message(if self.include_reasoning {
                "Сохранение рассуждений включено."
            } else {
                "Сохранение рассуждений выключено."
            }));
        } else if self.report.is_some() {
            self.handle_report(command, arguments, text, emit.as_ref())?;
        } else {
            emit(message(
                "Документ загружен. Введите /check для проверки или /help для справки.",
            ));
        }
        Ok(())
    }

    fn handle_report(
        &mut self,
        command: &str,
        arguments: &str,
        text: &str,
        emit: &EventHandler,
    ) -> Result<()> {
        let report = self.report.as_ref().expect("проверка завершена");
        match command {
            "/findings" => {
                let findings = report
                    .findings
                    .iter()
                    .map(|finding| format!("{} — {}", finding.id, finding.explanation))
                    .collect::<Vec<_>>()
                    .join("\n");
                emit(message(if findings.is_empty() {
                    "Находок нет.".into()
                } else {
                    findings
                }));
            }
            "/questions" => {
                self.current_question =
                    report.questions.first().map(|question| question.id.clone());
                if report.questions.is_empty() {
                    emit(message("Открытых вопросов нет."));
                }
                for question in &report.questions {
                    emit(AgentEvent::Question {
                        question: question.clone(),
                    });
                }
            }
            "/verdict" => emit(message(match report.readiness {
                Readiness::Blocked => "Не готово.",
                Readiness::ChecksIncomplete => "Проверка завершена не полностью.",
                Readiness::Ready => "Готово.",
            })),
            "/export" => {
                let (format, content) = match arguments {
                    "md" => (ExportFormat::Md, MarkdownRenderer.render(report)?),
                    "json" => (ExportFormat::Json, JsonRenderer.render(report)?),
                    _ => return Err("Используйте /export md или /export json.".into()),
                };
                emit(AgentEvent::Export { format, content });
            }
            "/answer" => {
                let (id, answer) = arguments
                    .split_once(char::is_whitespace)
                    .ok_or("Используйте /answer <ID> <ответ>.")?;
                self.answer(id, answer.trim(), emit)?;
            }
            _ if !text.starts_with('/') && !text.is_empty() && self.current_question.is_some() => {
                let id = self.current_question.clone().expect("открыт вопрос");
                self.answer(&id, text, emit)?;
            }
            _ => emit(message(HELP)),
        }
        Ok(())
    }

    async fn check(&mut self, emit: Arc<EventHandler>) -> Result<()> {
        let document = self.document.as_ref().expect("документ загружен").clone();
        let sink: Arc<dyn ProgressSink> = Arc::new(SessionProgress(Arc::clone(&emit)));
        let trace = progress_trace(Arc::clone(&sink));
        self.state = SessionState::Reviewing;
        let mut state = ReviewState {
            state: &mut self.state,
            next: if self.report.is_some() {
                SessionState::Reviewed
            } else {
                SessionState::Loaded
            },
        };
        let provider = (self.provider)(trace.clone(), self.include_reasoning)?;
        let run = review(
            document,
            provider.as_ref(),
            sink.as_ref(),
            trace,
            &self.answers,
        )
        .await;
        let report = run.report?;
        let summary = report.summary.clone();
        self.report = Some(report);
        self.current_question = None;
        state.next = SessionState::Reviewed;
        drop(state);
        emit(AgentEvent::ReportReady { summary });
        Ok(())
    }

    fn answer(&mut self, id: &str, text: &str, emit: &EventHandler) -> Result<()> {
        let report = self.report.as_mut().expect("проверка завершена");
        if !report.answer_question(id, text) {
            return Err("Укажите ID открытого вопроса и непустой ответ.".into());
        }
        self.answers.insert(id.into(), text.into());
        emit(AgentEvent::AnswersUpdated {
            answers: self.answers.clone(),
        });
        emit(message(format!("Ответ на {id} принят.")));
        if self.current_question.as_deref() == Some(id) {
            self.current_question = report.questions.first().map(|question| question.id.clone());
            if let Some(question) = report.questions.first() {
                emit(AgentEvent::Question {
                    question: question.clone(),
                });
            }
        }
        Ok(())
    }
}

fn message(text: impl Into<String>) -> AgentEvent {
    AgentEvent::Message {
        role: Role::Assistant,
        text: text.into(),
    }
}

struct SessionProgress(Arc<EventHandler>);

impl ProgressSink for SessionProgress {
    fn emit(&self, event: &ProgressEvent) {
        (self.0)(AgentEvent::Progress {
            progress: event.clone(),
        });
    }
}

// Возвращает сессию в рабочее состояние и при отмене будущего результата проверки.
struct ReviewState<'a> {
    state: &'a mut SessionState,
    next: SessionState,
}

impl Drop for ReviewState<'_> {
    fn drop(&mut self) {
        *self.state = self.next;
    }
}
