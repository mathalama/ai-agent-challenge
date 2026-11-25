//! Ядро чат-сессии и общий запуск проверки.

mod run;
mod session;

pub use run::{
    Answers, Provider, Result, ReviewRun, build_provider, progress_trace, read_answers, review,
};
pub use session::{AgentEvent, ExportFormat, Input, Role, Session, SessionState};
