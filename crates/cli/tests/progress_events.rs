use std::{fs, path::Path, process::Command};

#[test]
fn check_streams_progress_to_console_and_json_lines() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = tempfile::tempdir().unwrap();
    let events_path = output_dir.path().join("reports/run.jsonl");

    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .arg("check")
        .arg(workspace.join("samples/requirements.md"))
        .args(["--provider", "mock", "--no-cache", "--events"])
        .arg(&events_path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("[1/9] segment…"), "{stderr}");
    assert!(stderr.contains("[9/9] clarify: готово"), "{stderr}");
    assert!(stderr.contains("Прогон завершён"), "{stderr}");

    let events: Vec<serde_json::Value> = fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.first().unwrap()["event"], "run_started");
    assert_eq!(events.last().unwrap()["event"], "run_finished");
    assert_eq!(events.last().unwrap()["succeeded"], true);
    let finished: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "stage_finished")
        .map(|event| event["stage"].as_str().unwrap())
        .collect();
    assert_eq!(finished.len(), 9);
    assert_eq!(finished[0], "segment");
    assert_eq!(finished[8], "clarify");
}

#[test]
fn events_path_must_differ_from_report() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = tempfile::tempdir().unwrap();
    let report = output_dir.path().join("report.md");

    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .arg("check")
        .arg(workspace.join("samples/requirements.md"))
        .arg("-o")
        .arg(&report)
        .args(["--provider", "mock", "--no-cache", "--events"])
        .arg(&report)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Для событий укажите путь"));
}
