use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn chat(directory: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn opens_chat_without_a_document_or_provider_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let output = chat(directory.path(), &["chat"], "Привет\n/check\n/exit\n");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Загрузите документ")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn bare_command_with_redirected_stdin_requires_a_subcommand() {
    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("rs chat")
    );
}

#[test]
fn mock_chat_exports_the_same_report_and_persists_answers() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("requirements sample.md"),
        include_bytes!("../../../samples/requirements.md"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(root)
        .args([
            "check",
            "requirements sample.md",
            "--provider",
            "mock",
            "--no-cache",
            "--format",
            "json",
            "-o",
            "expected.json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    let question = expected["questions"][0]["id"].as_str().unwrap().to_owned();
    let args = ["chat", "--provider", "mock", "--no-cache"];
    let output = chat(
        root,
        &args,
        &format!(
            "/load \"requirements sample.md\"\n/check\n/ask Сколько замечаний?\n/export json\n/answer {question} Ответ автора\n/exit\n"
        ),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("найдено замечаний"));
    assert!(stdout.contains("(строки "));
    let mut exported: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("reports/report.json")).unwrap()).unwrap();
    for report in [&mut exported, &mut expected] {
        report["passport"].as_object_mut().unwrap().remove("date");
    }
    assert_eq!(exported, expected);
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("requirements sample.answers.json")).unwrap())
            .unwrap();
    assert_eq!(saved[question.as_str()], "Ответ автора");

    chat(
        root,
        &args,
        "/load requirements sample.md\n/check\n/export json\n/exit\n",
    );
    let resumed: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("reports/report.json")).unwrap()).unwrap();
    assert!(
        resumed["questions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != question)
    );
    assert!(
        resumed["finding_answers"]
            .to_string()
            .contains("Ответ автора")
    );
}
