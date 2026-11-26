use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn rs(profile: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rs"))
        .args(args)
        .env("APPDATA", profile)
        .env("XDG_CONFIG_HOME", profile)
        .env_remove("RS_PROVIDER")
        .env_remove("PROVIDER")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .current_dir(profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn stores_keys_from_stdin_and_masks_config_show() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path();
    assert!(
        rs(profile, &["config", "set", "provider", "openai"], "")
            .status
            .success()
    );
    assert!(
        rs(profile, &["config", "set", "model", "test/model:free"], "")
            .status
            .success()
    );
    assert!(
        rs(
            profile,
            &["config", "set", "api-key"],
            "fake-private-key-1a2b\n"
        )
        .status
        .success()
    );
    let rejected = rs(
        profile,
        &["config", "set", "api-key", "fake-argv-secret"],
        "",
    );
    assert!(!rejected.status.success());
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains("fake-argv-secret"));
    let shown = rs(profile, &["config", "show"], "");
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("задан ••••1a2b"));
    assert!(shown.contains("test/model:free"));
    assert!(!shown.contains("fake-private-key"));
}

#[test]
fn check_uses_the_saved_mock_provider() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path();
    assert!(
        rs(profile, &["config", "set", "provider", "mock"], "")
            .status
            .success()
    );
    let document = profile.join("requirements.md");
    std::fs::write(&document, include_str!("../../../samples/requirements.md")).unwrap();
    let checked = rs(
        profile,
        &["check", "requirements.md", "--no-cache", "-o", "report.md"],
        "",
    );
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        std::fs::read_to_string(profile.join("report.md"))
            .unwrap()
            .contains("mock")
    );
    let chat = rs(profile, &["chat"], "/exit\n");
    assert!(chat.status.success());
    assert!(String::from_utf8(chat.stdout).unwrap().contains("> "));
}

#[test]
fn broken_config_does_not_block_mock_or_config_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path();
    let config_dir = profile.join("rs");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_file = config_dir.join("config.json");
    std::fs::write(&config_file, "{not valid json").unwrap();

    let shown = rs(profile, &["config", "show"], "");
    assert!(!shown.status.success());
    assert!(String::from_utf8_lossy(&shown.stderr).contains("config.json"));

    let document = profile.join("requirements.md");
    std::fs::write(&document, include_str!("../../../samples/requirements.md")).unwrap();
    let checked = rs(
        profile,
        &[
            "check",
            "requirements.md",
            "--provider",
            "mock",
            "--no-cache",
            "-o",
            "report.md",
        ],
        "",
    );
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let repaired = rs(profile, &["config", "set", "provider", "mock"], "");
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    assert!(rs(profile, &["config", "show"], "").status.success());
}
