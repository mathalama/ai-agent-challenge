//! PDF-версии документа организаторов дают ту же нарезку требований, что и Markdown.
//! PDF собираются скриптом `scripts/gen_requirements_pdfs.py` (см. samples/README.md).

use std::path::Path;

use doc::ParserRegistry;
use review::{CheckRegistry, Pipeline, RunContext};

async fn run(name: &str, bytes: &[u8]) -> RunContext<'static> {
    let document = ParserRegistry::default()
        .parse(Path::new(name), bytes)
        .unwrap();
    // Реестр и провайдер нужны на всё время контекста, поэтому живут до конца теста.
    let checks: &'static CheckRegistry = Box::leak(Box::default());
    let mut ctx = RunContext::new(document, &llm::MockProvider, checks);
    Pipeline::default().run(&mut ctx).await.unwrap();
    ctx
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn pdf_versions_give_the_same_requirements_as_markdown() {
    let markdown = run(
        "requirements.md",
        include_bytes!("../../../samples/requirements.md"),
    )
    .await;
    let expected: Vec<_> = markdown
        .requirements
        .iter()
        .map(|req| (req.source_id.clone(), normalized(&req.text)))
        .collect();
    assert!(expected.len() >= 18);

    for (name, bytes) in [
        (
            "requirements.pdf",
            &include_bytes!("../../../samples/requirements.pdf")[..],
        ),
        (
            "requirements.pandoc.pdf",
            &include_bytes!("../../../samples/requirements.pandoc.pdf")[..],
        ),
    ] {
        let pdf = run(name, bytes).await;
        let actual: Vec<_> = pdf
            .requirements
            .iter()
            .map(|req| (req.source_id.clone(), normalized(&req.text)))
            .collect();
        assert_eq!(actual, expected, "{name}: нарезка требований отличается");
        let source_ids = |ctx: &RunContext| -> Vec<Option<String>> {
            ctx.requirements
                .iter()
                .map(|req| req.source_id.clone())
                .collect()
        };
        assert_eq!(source_ids(&pdf), source_ids(&markdown), "{name}");
    }
}

#[tokio::test]
async fn every_pdf_requirement_and_finding_points_to_a_page_and_a_findable_quote() {
    for (name, bytes) in [
        (
            "requirements.pdf",
            &include_bytes!("../../../samples/requirements.pdf")[..],
        ),
        (
            "requirements.pandoc.pdf",
            &include_bytes!("../../../samples/requirements.pandoc.pdf")[..],
        ),
    ] {
        let ctx = run(name, bytes).await;
        assert!(!ctx.findings.is_empty(), "{name}");
        for req in &ctx.requirements {
            assert!(req.location.page.is_some(), "{name}: {}", req.id);
            assert!(
                ctx.document.contains_quote(req.location, &req.quote),
                "{name}: цитата требования {} не найдена",
                req.id
            );
        }
        for finding in &ctx.findings {
            assert!(!finding.evidence.is_empty(), "{name}: {}", finding.id);
            for evidence in &finding.evidence {
                assert!(
                    evidence.location.page.is_some() && evidence.location.start_line >= 1,
                    "{name}: {} без страницы или строки",
                    finding.id
                );
                assert!(
                    ctx.document
                        .contains_quote(evidence.location, &evidence.quote),
                    "{name}: цитата находки {} не найдена на стр. {:?}",
                    finding.id,
                    evidence.location.page
                );
            }
        }
    }
}
