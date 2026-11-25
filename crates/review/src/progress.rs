//! События прогресса прогона. CLI печатает их в консоль и пишет в JSON Lines,
//! а HTTP-сервер сможет отдавать те же события через SSE.

use doc::SourceSpan;
use llm::RetryNotice;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    RunStarted {
        document: String,
        model: String,
        stages: Vec<&'static str>,
    },
    StageStarted {
        stage: &'static str,
        /// Номер стадии с единицы.
        index: usize,
        total: usize,
    },
    StageFinished {
        stage: &'static str,
        index: usize,
        total: usize,
        duration_ms: u64,
        succeeded: bool,
        /// Попытки запросов к LLM на этой стадии, включая повторы и попадания в кэш.
        llm_calls: usize,
        /// Сколько из них взято из кэша без обращения к модели.
        cached_calls: usize,
        input_tokens: u64,
        output_tokens: u64,
        /// Состояние прогона после стадии.
        requirements: usize,
        findings: usize,
        questions: usize,
    },
    LlmRetry {
        stage: String,
        reason: &'static str,
        status: Option<u16>,
        delay_ms: u64,
    },
    FactBatchSplit {
        source_ids: Vec<String>,
    },
    FactSourceSkipped {
        source_id: String,
        location: SourceSpan,
    },
    RunFinished {
        duration_ms: u64,
        succeeded: bool,
    },
}

impl From<&RetryNotice> for ProgressEvent {
    fn from(notice: &RetryNotice) -> Self {
        Self::LlmRetry {
            stage: notice.stage.clone(),
            reason: notice.reason,
            status: notice.status,
            delay_ms: notice.delay_ms,
        }
    }
}

/// Получатель событий прогресса. Вызывается синхронно, поэтому должен быть быстрым.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: &ProgressEvent);
}
