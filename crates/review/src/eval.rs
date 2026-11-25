use std::{
    collections::HashSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use doc::SourceSpan;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Criterion, Finding, Requirement, Scope, Severity};

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("Не удалось прочитать «{}»: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Некорректный формат эталонной разметки «{}»: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Ошибка в эталонной разметке «{}»: {message}", path.display())]
    Validation { path: PathBuf, message: String },
    #[error("В каталоге «{}» не найдено файлов эталонной разметки (*.golden.json)", .0.display())]
    EmptyDirectory(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenFile {
    pub document: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    pub defects: Vec<GoldenDefect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenDefect {
    pub id: String,
    pub requirement: String,
    pub criterion: Criterion,
    #[serde(default)]
    pub acceptable_criteria: Vec<Criterion>,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    pub lines: Vec<usize>,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct LoadedGolden {
    pub golden_path: PathBuf,
    pub document_path: PathBuf,
    pub title: String,
    pub defects: Vec<GoldenDefect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedDefect {
    pub defect_id: String,
    pub finding_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentEval {
    pub golden_path: PathBuf,
    pub document_path: PathBuf,
    pub title: String,
    pub total_defects: usize,
    pub total_findings: usize,
    pub verified_findings: usize,
    pub true_positives: usize,
    pub precision: f64,
    pub recall: f64,
    pub matched: Vec<MatchedDefect>,
    pub missed: Vec<GoldenDefect>,
}

pub fn discover_golden_files(path: &Path) -> Result<Vec<PathBuf>, EvalError> {
    if !path.exists() {
        return Err(EvalError::Validation {
            path: path.to_path_buf(),
            message: "указанный путь не существует".into(),
        });
    }
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let entries = fs::read_dir(path).map_err(|source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| EvalError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let candidate = entry.path();
        if candidate.is_file()
            && candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".golden.json"))
        {
            files.push(candidate);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(EvalError::EmptyDirectory(path.to_path_buf()));
    }
    Ok(files)
}

pub fn load_golden_file(path: &Path) -> Result<LoadedGolden, EvalError> {
    let raw = fs::read_to_string(path).map_err(|source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let golden: GoldenFile = serde_json::from_str(&raw).map_err(|source| EvalError::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    if golden.document.as_os_str().is_empty() {
        return Err(EvalError::Validation {
            path: path.to_path_buf(),
            message: "поле «document» не должно быть пустым".into(),
        });
    }
    let document_path =
        resolve_document_path(path, &golden.document).ok_or_else(|| EvalError::Validation {
            path: path.to_path_buf(),
            message: format!(
                "документ «{}» из поля «document» не найден",
                golden.document.display()
            ),
        })?;

    if golden.defects.is_empty() {
        return Err(EvalError::Validation {
            path: path.to_path_buf(),
            message: "список «defects» не должен быть пустым".into(),
        });
    }

    let is_pdf = is_pdf_document(&document_path);
    let text_line_count = text_document_line_count(&document_path);
    let mut seen_ids = HashSet::new();
    for (index, defect) in golden.defects.iter().enumerate() {
        let label = if defect.id.trim().is_empty() {
            format!("#{}", index + 1)
        } else {
            defect.id.clone()
        };
        if defect.id.trim().is_empty() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!("дефект {label}: поле «id» не должно быть пустым"),
            });
        }
        if !seen_ids.insert(defect.id.clone()) {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!("дублирующийся идентификатор дефекта «{}»", defect.id),
            });
        }
        if defect.requirement.trim().is_empty() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!("дефект {label}: поле «requirement» не должно быть пустым"),
            });
        }
        if defect.description.trim().is_empty() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!("дефект {label}: поле «description» не должно быть пустым"),
            });
        }
        if defect.page == Some(0) {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!("дефект {label}: номера страниц начинаются с 1 (указано 0)"),
            });
        }
        if is_pdf && defect.page.is_none() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!(
                    "дефект {label}: для PDF-документа необходимо указать номер страницы в поле «page»"
                ),
            });
        }
        if !is_pdf && defect.page.is_some() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!(
                    "дефект {label}: поле «page» задаётся только для страничных документов (PDF)"
                ),
            });
        }
        if defect.lines.is_empty() {
            return Err(EvalError::Validation {
                path: path.to_path_buf(),
                message: format!(
                    "дефект {label}: поле «lines» должно содержать хотя бы одну строку"
                ),
            });
        }
        for &line in &defect.lines {
            if line == 0 {
                return Err(EvalError::Validation {
                    path: path.to_path_buf(),
                    message: format!("дефект {label}: номера строк начинаются с 1 (указано 0)"),
                });
            }
            if let Some(max_lines) = text_line_count
                && line > max_lines
            {
                return Err(EvalError::Validation {
                    path: path.to_path_buf(),
                    message: format!(
                        "дефект {label}: строка {line} выходит за пределы документа ({max_lines} строк)"
                    ),
                });
            }
        }
    }

    let title = golden
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            document_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });

    Ok(LoadedGolden {
        golden_path: path.to_path_buf(),
        document_path,
        title,
        defects: golden.defects,
    })
}

fn resolve_document_path(golden_path: &Path, document: &Path) -> Option<PathBuf> {
    if document.is_absolute() {
        return document.is_file().then(|| document.to_path_buf());
    }
    if let Some(parent) = golden_path.parent() {
        let candidate = parent.join(document);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    document.is_file().then(|| document.to_path_buf())
}

fn is_pdf_document(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn text_document_line_count(path: &Path) -> Option<usize> {
    let ext = path.extension()?.to_str()?;
    if !matches!(
        ext.to_ascii_lowercase().as_str(),
        "md" | "markdown" | "txt" | "text"
    ) {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    Some(content.lines().count())
}

pub fn evaluate_document(
    golden: &LoadedGolden,
    requirements: &[Requirement],
    findings: &[Finding],
) -> DocumentEval {
    let verified_findings = findings.iter().filter(|finding| finding.verified).count();

    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(findings.len());
    for finding in findings {
        if !finding.verified {
            edges.push(Vec::new());
            continue;
        }
        let matching_defects = golden
            .defects
            .iter()
            .enumerate()
            .filter(|(_, defect)| finding_matches_defect(finding, defect, requirements))
            .map(|(index, _)| index)
            .collect();
        edges.push(matching_defects);
    }

    // Максимальное паросочетание между подтверждёнными находками и эталонными дефектами:
    // дубли находок не увеличивают recall и честно снижают precision.
    let mut defect_match: Vec<Option<usize>> = vec![None; golden.defects.len()];
    for finding_idx in 0..findings.len() {
        if edges[finding_idx].is_empty() {
            continue;
        }
        let mut visited = vec![false; golden.defects.len()];
        let _ = try_augment(finding_idx, &edges, &mut visited, &mut defect_match);
    }

    let mut matched = Vec::new();
    let mut missed = Vec::new();
    for (defect_idx, defect) in golden.defects.iter().enumerate() {
        if let Some(finding_idx) = defect_match[defect_idx] {
            matched.push(MatchedDefect {
                defect_id: defect.id.clone(),
                finding_id: findings[finding_idx].id.clone(),
            });
        } else {
            missed.push(defect.clone());
        }
    }

    let true_positives = matched.len();
    let total_defects = golden.defects.len();
    let total_findings = findings.len();
    let precision = if total_findings == 0 {
        0.0
    } else {
        true_positives as f64 / total_findings as f64
    };
    let recall = if total_defects == 0 {
        0.0
    } else {
        true_positives as f64 / total_defects as f64
    };

    DocumentEval {
        golden_path: golden.golden_path.clone(),
        document_path: golden.document_path.clone(),
        title: golden.title.clone(),
        total_defects,
        total_findings,
        verified_findings,
        true_positives,
        precision,
        recall,
        matched,
        missed,
    }
}

fn try_augment(
    finding_idx: usize,
    edges: &[Vec<usize>],
    visited: &mut [bool],
    defect_match: &mut [Option<usize>],
) -> bool {
    for &defect_idx in &edges[finding_idx] {
        if visited[defect_idx] {
            continue;
        }
        visited[defect_idx] = true;
        if defect_match[defect_idx].is_none()
            || try_augment(
                defect_match[defect_idx].expect("проверено выше"),
                edges,
                visited,
                defect_match,
            )
        {
            defect_match[defect_idx] = Some(finding_idx);
            return true;
        }
    }
    false
}

fn span_matches_defect(span: SourceSpan, defect: &GoldenDefect) -> bool {
    span.page == defect.page
        && defect
            .lines
            .iter()
            .any(|&line| span.start_line <= line && line <= span.end_line)
}

fn finding_matches_defect(
    finding: &Finding,
    defect: &GoldenDefect,
    requirements: &[Requirement],
) -> bool {
    let criterion_matches = finding.all_criteria().iter().any(|&criterion| {
        criterion == defect.criterion || defect.acceptable_criteria.contains(&criterion)
    });
    if !criterion_matches {
        return false;
    }

    if finding
        .evidence
        .iter()
        .any(|evidence| span_matches_defect(evidence.location, defect))
    {
        return true;
    }

    if let Scope::Requirement(req_id) = &finding.scope
        && let Some(requirement) = requirements.iter().find(|req| req.id == *req_id)
    {
        let page_matches = requirement.location.page == defect.page;
        let req_tokens = defect_requirement_tokens(&defect.requirement);
        let id_matches = page_matches
            && req_tokens.iter().any(|token| {
                token.eq_ignore_ascii_case(&requirement.id)
                    || requirement
                        .source_id
                        .as_deref()
                        .is_some_and(|source_id| equals_ignore_case(token, source_id))
            });
        if id_matches || span_matches_defect(requirement.location, defect) {
            return true;
        }
    }

    false
}

fn defect_requirement_tokens(requirement: &str) -> Vec<&str> {
    requirement
        .split([',', ';', '/'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect()
}

fn equals_ignore_case(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

pub fn format_eval_report(results: &[DocumentEval]) -> String {
    let mut out = String::new();
    for (index, item) in results.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "=== {} ({}) ===",
            item.title,
            item.document_path.display()
        );
        let _ = writeln!(
            out,
            "Найдено дефектов: {}/{} (recall: {:.1}%)",
            item.true_positives,
            item.total_defects,
            item.recall * 100.0
        );
        let _ = writeln!(
            out,
            "Находок агента: {} (подтверждённых: {}, precision: {:.1}%)",
            item.total_findings,
            item.verified_findings,
            item.precision * 100.0
        );
        if item.missed.is_empty() {
            out.push_str("Пропущенные дефекты: нет\n");
        } else {
            let _ = writeln!(out, "Пропущенные дефекты ({}):", item.missed.len());
            for defect in &item.missed {
                let lines = defect
                    .lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let location = match defect.page {
                    Some(page) => format!("страница {page}, строки {lines}"),
                    None => format!("строки {lines}"),
                };
                let _ = writeln!(
                    out,
                    "  - {} [{} · {}] {} ({}): {}",
                    defect.id,
                    defect.severity.russian_name(),
                    defect.criterion.russian_name(),
                    defect.requirement,
                    location,
                    defect.description
                );
            }
        }
    }

    if results.len() > 1 {
        let total_tp: usize = results.iter().map(|r| r.true_positives).sum();
        let total_defects: usize = results.iter().map(|r| r.total_defects).sum();
        let total_findings: usize = results.iter().map(|r| r.total_findings).sum();
        let precision = if total_findings == 0 {
            0.0
        } else {
            total_tp as f64 / total_findings as f64
        };
        let recall = if total_defects == 0 {
            0.0
        } else {
            total_tp as f64 / total_defects as f64
        };
        let _ = writeln!(
            out,
            "\n=== Итого по {} документам ===\nRecall: {:.1}% ({}/{}) · Precision: {:.1}% ({}/{})",
            results.len(),
            recall * 100.0,
            total_tp,
            total_defects,
            precision * 100.0,
            total_tp,
            total_findings,
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Detector, Evidence};

    fn sample_requirement(
        id: &str,
        source_id: Option<&str>,
        line: usize,
        page: Option<usize>,
    ) -> Requirement {
        Requirement {
            id: id.into(),
            source_id: source_id.map(str::to_owned),
            text: String::new(),
            quote: String::new(),
            location: SourceSpan {
                start_line: line,
                end_line: line,
                page,
            },
            part: None,
            section: None,
            table: None,
        }
    }

    fn sample_finding(
        id: &str,
        scope: Scope,
        criterion: Criterion,
        line: usize,
        page: Option<usize>,
        verified: bool,
    ) -> Finding {
        Finding {
            id: id.into(),
            scope,
            criterion,
            additional_criteria: Vec::new(),
            severity: Severity::High,
            evidence: vec![Evidence {
                quote: "цитата".into(),
                location: SourceSpan {
                    start_line: line,
                    end_line: line,
                    page,
                },
            }],
            explanation: "Описание дефекта".into(),
            suggestion: None,
            detector: Detector::Rule("test".into()),
            additional_detectors: Vec::new(),
            merged_ids: Vec::new(),
            fallback_evidence: false,
            verified,
        }
    }

    #[test]
    fn repository_golden_files_are_valid_and_in_bounds() {
        let eval_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
        let files = discover_golden_files(&eval_dir).unwrap();
        assert_eq!(files.len(), 2);
        for file in files {
            let loaded = load_golden_file(&file).unwrap();
            assert!(loaded.defects.len() >= 12);
        }
    }

    #[test]
    fn evaluation_computes_precision_recall_and_penalizes_duplicates() {
        let golden = LoadedGolden {
            golden_path: PathBuf::from("eval/test.golden.json"),
            document_path: PathBuf::from("doc.md"),
            title: "Тест".into(),
            defects: vec![
                GoldenDefect {
                    id: "D-01".into(),
                    requirement: "Ф-3, Ф-4".into(),
                    criterion: Criterion::Consistent,
                    acceptable_criteria: vec![],
                    severity: Severity::Critical,
                    page: None,
                    lines: vec![39, 40],
                    description: "Противоречие".into(),
                },
                GoldenDefect {
                    id: "D-02".into(),
                    requirement: "Требования к тестированию".into(),
                    criterion: Criterion::SetComplete,
                    acceptable_criteria: vec![],
                    severity: Severity::Medium,
                    page: None,
                    lines: vec![56, 57],
                    description: "Пустой раздел".into(),
                },
                GoldenDefect {
                    id: "D-03".into(),
                    requirement: "НФ-1".into(),
                    criterion: Criterion::Verifiable,
                    acceptable_criteria: vec![Criterion::Unambiguous],
                    severity: Severity::Medium,
                    page: None,
                    lines: vec![46],
                    description: "Без SLA".into(),
                },
            ],
        };
        let reqs = vec![
            sample_requirement("R-001", Some("Ф-3"), 39, None),
            sample_requirement("R-002", Some("НФ-1"), 46, None),
        ];
        let findings = vec![
            sample_finding(
                "f-1",
                Scope::Requirement("R-001".into()),
                Criterion::Consistent,
                39,
                None,
                true,
            ),
            sample_finding("f-2", Scope::Set, Criterion::Consistent, 40, None, true),
            sample_finding(
                "f-3",
                Scope::Requirement("R-002".into()),
                Criterion::Unambiguous,
                46,
                None,
                true,
            ),
            sample_finding("f-4", Scope::Set, Criterion::SetComplete, 57, None, false),
        ];

        let eval = evaluate_document(&golden, &reqs, &findings);
        assert_eq!(eval.true_positives, 2);
        assert_eq!(eval.total_defects, 3);
        assert_eq!(eval.total_findings, 4);
        assert_eq!(eval.verified_findings, 3);
        assert!((eval.precision - 0.5).abs() < 1e-9);
        assert!((eval.recall - (2.0 / 3.0)).abs() < 1e-9);
        assert_eq!(eval.missed.len(), 1);
        assert_eq!(eval.missed[0].id, "D-02");

        let formatted = format_eval_report(&[eval]);
        assert!(formatted.contains("Найдено дефектов: 2/3"));
        assert!(formatted.contains("precision: 50.0%"));
        assert!(formatted.contains("D-02"));
    }

    #[test]
    fn evaluation_distinguishes_pdf_pages_for_same_line_numbers() {
        let golden = LoadedGolden {
            golden_path: PathBuf::from("eval/spec.golden.json"),
            document_path: PathBuf::from("spec.pdf"),
            title: "PDF".into(),
            defects: vec![GoldenDefect {
                id: "P-01".into(),
                requirement: "Ф-1".into(),
                criterion: Criterion::Consistent,
                acceptable_criteria: vec![],
                severity: Severity::High,
                page: Some(1),
                lines: vec![5],
                description: "Дефект на странице 1".into(),
            }],
        };
        let reqs = vec![
            sample_requirement("R-001", Some("Ф-1"), 5, Some(1)),
            sample_requirement("R-002", Some("Ф-1"), 5, Some(2)),
        ];

        // Находка на странице 2 (строка 5) не должна давать ложное совпадение с дефектом на странице 1
        let wrong_page = vec![sample_finding(
            "f-wrong",
            Scope::Requirement("R-002".into()),
            Criterion::Consistent,
            5,
            Some(2),
            true,
        )];
        let eval_wrong = evaluate_document(&golden, &reqs, &wrong_page);
        assert_eq!(eval_wrong.true_positives, 0);
        assert_eq!(eval_wrong.missed.len(), 1);

        // Находка на странице 1 (строка 5) засчитывается
        let right_page = vec![sample_finding(
            "f-right",
            Scope::Requirement("R-001".into()),
            Criterion::Consistent,
            5,
            Some(1),
            true,
        )];
        let eval_right = evaluate_document(&golden, &reqs, &right_page);
        assert_eq!(eval_right.true_positives, 1);
        assert!(eval_right.missed.is_empty());
    }

    #[test]
    fn invalid_golden_markup_returns_clear_errors() {
        let dir = tempfile::tempdir().unwrap();
        let doc_path = dir.path().join("doc.md");
        fs::write(&doc_path, "# Заголовок\nСтрока 2\n").unwrap();
        let pdf_path = dir.path().join("doc.pdf");
        fs::write(&pdf_path, b"%PDF-1.4\n").unwrap();

        let write_and_load = |name: &str, body: &str| {
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            load_golden_file(&path).unwrap_err()
        };

        let err = write_and_load(
            "unknown.golden.json",
            r#"{"document":"doc.md","extra":1,"defects":[]}"#,
        );
        assert!(matches!(err, EvalError::Parse { .. }));

        let err = write_and_load(
            "bad_criterion.golden.json",
            r#"{"document":"doc.md","defects":[{"id":"D-1","requirement":"Ф-1","criterion":"unknown_crit","severity":"high","lines":[1],"description":"описание"}]}"#,
        );
        assert!(matches!(err, EvalError::Parse { .. }));

        let err = write_and_load(
            "oob_line.golden.json",
            r#"{"document":"doc.md","defects":[{"id":"D-1","requirement":"Ф-1","criterion":"consistent","severity":"high","lines":[99],"description":"описание"}]}"#,
        );
        assert!(
            matches!(err, EvalError::Validation { ref message, .. } if message.contains("выходит за пределы"))
        );

        let err = write_and_load(
            "dup_id.golden.json",
            r#"{"document":"doc.md","defects":[
                {"id":"D-1","requirement":"Ф-1","criterion":"consistent","severity":"high","lines":[1],"description":"описание 1"},
                {"id":"D-1","requirement":"Ф-2","criterion":"complete","severity":"low","lines":[2],"description":"описание 2"}
            ]}"#,
        );
        assert!(
            matches!(err, EvalError::Validation { ref message, .. } if message.contains("дублирующийся"))
        );

        // PDF без поля page — ошибка валидации
        let err = write_and_load(
            "pdf_no_page.golden.json",
            r#"{"document":"doc.pdf","defects":[{"id":"P-1","requirement":"Ф-1","criterion":"consistent","severity":"high","lines":[1],"description":"описание"}]}"#,
        );
        assert!(
            matches!(err, EvalError::Validation { ref message, .. } if message.contains("«page»"))
        );

        // page = 0 — ошибка валидации
        let err = write_and_load(
            "pdf_zero_page.golden.json",
            r#"{"document":"doc.pdf","defects":[{"id":"P-1","requirement":"Ф-1","criterion":"consistent","severity":"high","page":0,"lines":[1],"description":"описание"}]}"#,
        );
        assert!(
            matches!(err, EvalError::Validation { ref message, .. } if message.contains("начинаются с 1"))
        );
    }
}
