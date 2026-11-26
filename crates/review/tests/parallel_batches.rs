//! Пакеты стадии `requirement_checks` ждут модель одновременно, но результат не зависит от порядка ответов.

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use doc::ParserRegistry;
use llm::{LlmProvider, LlmRequest, LlmResponse, Usage};
use review::{CheckRegistry, Pipeline, RunContext};
use serde_json::json;

const REQUIREMENTS: usize = 60; // 5 пакетов по 12
const DELAY: Duration = Duration::from_millis(100);

#[derive(Default)]
struct SlowProvider {
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

#[async_trait]
impl LlmProvider for SlowProvider {
    fn model(&self) -> &str {
        "slow"
    }

    async fn generate(&self, request: &LlmRequest) -> llm::Result<LlmResponse> {
        let json = if request.stage == "requirement_checks" {
            let input: serde_json::Value = serde_json::from_str(&request.user_text).unwrap();
            let batch = input.as_array().unwrap();
            let first = &batch[0];
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            // Первый пакет отвечает дольше всех: порядок завершения не совпадает с порядком пакетов.
            let slow = first["id"] == "R-001";
            tokio::time::sleep(if slow { DELAY * 2 } else { DELAY }).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            json!([{
                "requirement_id": first["id"],
                "criterion": "unambiguous",
                "severity": "low",
                "evidence": [{"quote": first["quote"], "location": first["location"]}],
                "explanation": "Формулировка допускает разное толкование.",
                "suggestion": null
            }])
        } else {
            json!([])
        };
        Ok(LlmResponse {
            reasoning: None,
            json,
            usage: Usage::default(),
            model: self.model().into(),
        })
    }
}

#[tokio::test]
async fn requirement_batches_run_concurrently_and_keep_batch_order() {
    let mut source = String::from("## Функциональные требования\n\n");
    for number in 1..=REQUIREMENTS {
        source.push_str(&format!(
            "* **Ф-{number}.** Система обрабатывает событие номер {number} за разумное время.\n"
        ));
    }
    let document = ParserRegistry::default()
        .parse(Path::new("batches.md"), source.as_bytes())
        .unwrap();
    let provider = SlowProvider::default();
    let checks = CheckRegistry::default();
    let mut ctx = RunContext::new(document, &provider, &checks);

    let started = Instant::now();
    Pipeline::default().run(&mut ctx).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(ctx.requirements.len(), REQUIREMENTS);
    let found: Vec<_> = ctx
        .findings
        .iter()
        .filter(|finding| finding.id.starts_with("requirement_checks-"))
        .map(|finding| (finding.id.clone(), finding.scope.clone()))
        .collect();
    // Идентификаторы идут подряд, а находки — в порядке пакетов, несмотря на медленный первый ответ.
    let expected: Vec<_> = (0..5)
        .map(|batch| {
            (
                format!("requirement_checks-{:03}", batch + 1),
                review::Scope::Requirement(format!("R-{:03}", batch * 12 + 1)),
            )
        })
        .collect();
    assert_eq!(found, expected);

    let max = provider.max_in_flight.load(Ordering::SeqCst);
    assert!((2..=4).contains(&max), "одновременно запросов: {max}");
    // Последовательно пять пакетов заняли бы не меньше 600 мс (200 + 4 × 100).
    assert!(elapsed < Duration::from_millis(550), "{elapsed:?}");
}
