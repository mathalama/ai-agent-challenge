use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex},
};

use agent::AgentEvent;
use axum::response::{
    IntoResponse, Response, Sse,
    sse::{Event, KeepAlive},
};
use futures_util::stream;
use tokio::sync::watch;

const MAX_EVENTS: usize = 8192;
const MAX_BYTES: usize = 8 * 1024 * 1024;

struct Entry {
    id: u64,
    kind: &'static str,
    json: String,
}

#[derive(Default)]
struct Journal {
    entries: VecDeque<Entry>,
    next_id: u64,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Events {
    journal: Mutex<Journal>,
    changed: watch::Sender<u64>,
}

impl Events {
    pub(super) fn push(&self, event: AgentEvent) {
        match serde_json::to_string(&event) {
            Ok(json) if json.len() <= MAX_BYTES => self.append("message", json),
            _ => self.append("message", r#"{"event":"error","text":"Событие слишком большое. Скачайте отчёт на экране экспорта."}"#.into()),
        }
    }

    pub(super) fn done(&self) {
        self.append("done", "{}".into());
    }

    fn append(&self, kind: &'static str, json: String) {
        let mut journal = self.journal.lock().expect("блокировка событий");
        journal.next_id += 1;
        let id = journal.next_id;
        journal.bytes += json.len();
        journal.entries.push_back(Entry { id, kind, json });
        while journal.entries.len() > MAX_EVENTS || journal.bytes > MAX_BYTES {
            let removed = journal.entries.pop_front().expect("событие для вытеснения");
            journal.bytes -= removed.json.len();
        }
        self.changed.send_replace(id);
    }

    fn after(&self, cursor: u64) -> Option<(u64, Event)> {
        let journal = self.journal.lock().expect("блокировка событий");
        let first = journal.entries.front()?.id;
        let index = cursor.saturating_add(1).saturating_sub(first);
        let next = journal.entries.get(usize::try_from(index).ok()?)?;
        Some((
            next.id,
            Event::default()
                .id(next.id.to_string())
                .event(next.kind)
                .data(&next.json),
        ))
    }

    pub(super) fn subscribe(self: Arc<Self>, cursor: u64) -> Response {
        let changed = self.changed.subscribe();
        let events = stream::unfold(
            (self, changed, cursor),
            |(events, mut changed, cursor)| async move {
                loop {
                    changed.borrow_and_update();
                    if let Some((id, event)) = events.after(cursor) {
                        return Some((Ok::<_, Infallible>(event), (events, changed, id)));
                    }
                    changed.changed().await.ok()?;
                }
            },
        );
        Sse::new(events)
            .keep_alive(KeepAlive::default())
            .into_response()
    }
}
