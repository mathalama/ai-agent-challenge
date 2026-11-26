use std::{collections::BTreeMap, path::Path, sync::Arc};

use doc::{Document, ParserRegistry};
use llm::{LlmProvider, TraceLog, generate_typed};
use review::{
    Evidence, JsonRenderer, MarkdownRenderer, ProgressEvent, ProgressSink, Question, Readiness,
    Report, ReportRenderer, ReportSummary,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Answers, Result, progress_trace, review};

const HELP: &str = "Загрузите документ через клиент. В терминале используйте /load <путь>. Для проверки введите /check. После проверки доступны /findings, /questions, /verdict, /export md|json и /ask <вопрос>. Ответ на уточнение отправьте командой /answer <ID> <ответ>. Рассуждения включаются командой /reasoning on.";

pub enum Input<'a> {
    Text(&'a str),
    /// Клиент обрабатывает `/load <путь>` или загрузку файла и передаёт байты.
    /// Имя используется для выбора парсера.
    Document {
        name: &'a Path,
        bytes: &'a [u8],
        answers: Answers,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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
        #[serde(skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Evidence>,
    },
    Progress {
        progress: ProgressEvent,
    },
    Reasoning {
        stage: String,
        delta: String,
        complete: bool,
    },
    StateChanged {
        state: SessionState,
    },
    ReportReady {
        summary: ReportSummary,
        readiness: Readiness,
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

#[derive(Deserialize, JsonSchema)]
struct ChatReply {
    text: String,
    citations: Vec<Evidence>,
}

pub struct Session {
    document: Option<Document>,
    report: Option<Report>,
    answers: Answers,
    include_reasoning: bool,
    provider: Box<ProviderFactory>,
}

impl Session {
    /// Провайдер создаётся только при запуске проверки.
    pub fn new(
        provider: impl Fn(TraceLog, bool) -> Result<Box<dyn LlmProvider>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            document: None,
            report: None,
            answers: Answers::new(),
            include_reasoning: false,
            provider: Box::new(provider),
        }
    }

    /// Состояние между командами. Во время проверки клиент получает `Reviewing` событием.
    pub fn state(&self) -> SessionState {
        if self.report.is_some() {
            SessionState::Reviewed
        } else if self.document.is_some() {
            SessionState::Loaded
        } else {
            SessionState::Empty
        }
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
            Input::Document {
                name,
                bytes,
                answers,
            } => self.load(name, bytes, answers).map(|event| {
                emit(AgentEvent::StateChanged {
                    state: self.state(),
                });
                emit(event);
            }),
            Input::Text(text) => self.handle_text(text.trim(), &emit).await,
        };
        if let Err(error) = result {
            emit(AgentEvent::Error {
                text: error.to_string(),
            });
        }
    }

    fn load(&mut self, name: &Path, bytes: &[u8], answers: Answers) -> Result<AgentEvent> {
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
        self.document = Some(document);
        self.report = None;
        self.answers = answers;
        Ok(event)
    }

    async fn handle_text(&mut self, text: &str, emit: &Arc<EventHandler>) -> Result<()> {
        let (command, arguments) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let arguments = arguments.trim();
        if command == "/reasoning" {
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
        } else if command == "/load" {
            emit(message(
                "Команду /load <путь> обрабатывает клиент. Загрузите файл через интерфейс клиента.",
            ));
        } else if self.document.is_none() || command == "/help" {
            emit(message(HELP));
        } else if command == "/check" || text.to_lowercase() == "проверь" {
            self.check(Arc::clone(emit)).await?;
        } else if self.report.is_some() {
            if command == "/ask" {
                if arguments.is_empty() {
                    return Err("Используйте /ask <вопрос>.".into());
                }
                self.ask(arguments, Arc::clone(emit)).await?;
            } else if !text.is_empty() && !text.starts_with('/') {
                self.ask(text, Arc::clone(emit)).await?;
            } else {
                self.handle_report(command, arguments, emit.as_ref())?;
            }
        } else {
            emit(message(
                "Документ загружен. Введите /check для проверки или /help для справки.",
            ));
        }
        Ok(())
    }

    fn handle_report(&mut self, command: &str, arguments: &str, emit: &EventHandler) -> Result<()> {
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
                Readiness::Blocked => "Не готово.".into(),
                Readiness::ChecksIncomplete => "Проверка завершена не полностью.".into(),
                Readiness::Ready => "Готово.".into(),
                Readiness::NotRequirements => match &report.triage {
                    Some(triage) => format!(
                        "Документ не является спецификацией требований. {}",
                        triage.reason
                    ),
                    None => "Документ не является спецификацией требований.".into(),
                },
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
            _ => emit(message(HELP)),
        }
        Ok(())
    }

    async fn check(&mut self, emit: Arc<EventHandler>) -> Result<()> {
        emit(AgentEvent::StateChanged {
            state: SessionState::Reviewing,
        });
        let result = self.run_check(Arc::clone(&emit)).await.map(|report| {
            let summary = report.summary.clone();
            let readiness = report.readiness;
            self.report = Some(report);
            (summary, readiness)
        });
        emit(AgentEvent::StateChanged {
            state: self.state(),
        });
        let (summary, readiness) = result?;
        emit(AgentEvent::ReportReady { summary, readiness });
        Ok(())
    }

    async fn run_check(&self, emit: Arc<EventHandler>) -> Result<Report> {
        let document = self.document.as_ref().expect("документ загружен").clone();
        let sink: Arc<dyn ProgressSink> = Arc::new(SessionProgress(emit));
        let trace = progress_trace(Arc::clone(&sink));
        let provider = (self.provider)(trace.clone(), self.include_reasoning)?;
        let run = review(
            document,
            provider.as_ref(),
            sink.as_ref(),
            trace,
            &self.answers,
        )
        .await;
        Ok(run.report?)
    }

    async fn ask(&self, question: &str, emit: Arc<EventHandler>) -> Result<()> {
        let document = self.document.as_ref().expect("документ загружен");
        let report = self.report.as_ref().expect("проверка завершена");
        let sink = Arc::new(SessionProgress(Arc::clone(&emit)));
        let provider = (self.provider)(progress_trace(sink), self.include_reasoning)?;
        let payload =
            serde_json::to_string(&serde_json::json!({ "question": question, "report": report }))?;
        let mut reply = generate_typed::<ChatReply>(
            provider.as_ref(),
            "chat",
            include_str!("../../../prompts/chat.md"),
            payload,
        )
        .await?;
        reply
            .citations
            .retain(|item| document.contains_quote(item.location, &item.quote));
        emit(AgentEvent::Message {
            role: Role::Assistant,
            text: reply.text,
            citations: reply.citations,
        });
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
        Ok(())
    }
}

fn message(text: impl Into<String>) -> AgentEvent {
    AgentEvent::Message {
        role: Role::Assistant,
        text: text.into(),
        citations: Vec::new(),
    }
}

struct SessionProgress(Arc<EventHandler>);

impl ProgressSink for SessionProgress {
    fn emit(&self, event: &ProgressEvent) {
        (self.0)(match event {
            ProgressEvent::LlmReasoning { stage, text } => AgentEvent::Reasoning {
                stage: stage.clone(),
                delta: text.clone(),
                complete: true,
            },
            ProgressEvent::LlmReasoningDelta { stage, delta } => AgentEvent::Reasoning {
                stage: stage.clone(),
                delta: delta.clone(),
                complete: false,
            },
            _ => AgentEvent::Progress {
                progress: event.clone(),
            },
        });
    }
}
