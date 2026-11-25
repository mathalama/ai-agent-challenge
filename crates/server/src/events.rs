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
    expired: bool,
}

enum Next {
    Event(u64, Event),
    Waiting,
    Expired,
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

    pub(super) fn expire(&self) {
        let mut journal = self.journal.lock().expect("блокировка событий");
        journal.expired = true;
        journal.entries.clear();
        journal.bytes = 0;
        self.changed.send_modify(|_| {});
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

    fn after(&self, cursor: u64) -> Next {
        let journal = self.journal.lock().expect("блокировка событий");
        if journal.expired {
            return Next::Expired;
        }
        let Some(first) = journal.entries.front().map(|entry| entry.id) else {
            return Next::Waiting;
        };
        let expected = cursor.saturating_add(1);
        if expected < first {
            let id = first - 1;
            return Next::Event(
                id,
                Event::default()
                    .id(id.to_string())
                    .event("history_gap")
                    .data("{}"),
            );
        }
        let Some(next) = usize::try_from(expected - first)
            .ok()
            .and_then(|index| journal.entries.get(index))
        else {
            return Next::Waiting;
        };
        Next::Event(
            next.id,
            Event::default()
                .id(next.id.to_string())
                .event(next.kind)
                .data(&next.json),
        )
    }

    pub(super) fn subscribe(self: Arc<Self>, cursor: u64) -> Response {
        let changed = self.changed.subscribe();
        let events = stream::unfold(
            (self, changed, Some(cursor)),
            |(events, mut changed, cursor)| async move {
                let cursor = cursor?;
                loop {
                    changed.borrow_and_update();
                    match events.after(cursor) {
                        Next::Event(id, event) => {
                            return Some((Ok::<_, Infallible>(event), (events, changed, Some(id))));
                        }
                        Next::Expired => {
                            return Some((
                                Ok(Event::default().event("expired").data("{}")),
                                (events, changed, None),
                            ));
                        }
                        Next::Waiting => changed.changed().await.ok()?,
                    }
                }
            },
        );
        Sse::new(events)
            .keep_alive(KeepAlive::default())
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_cursor_signals_a_gap_before_replaying_retained_events() {
        use futures_util::StreamExt as _;

        let events = Arc::new(Events::default());
        for _ in 0..=MAX_EVENTS {
            events.done();
        }
        let mut body = events.subscribe(0).into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert!(
            std::str::from_utf8(&first)
                .unwrap()
                .contains("event: history_gap")
        );
        let second = body.next().await.unwrap().unwrap();
        assert!(
            std::str::from_utf8(&second)
                .unwrap()
                .contains("event: done")
        );
    }
}
