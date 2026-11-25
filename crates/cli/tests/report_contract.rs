use std::{fs, path::Path, process::Command};

use serde_json::Value;
use sha2::{Digest, Sha256};

const MOCK_FIXTURE: &str = include_str!("../../../docs/api/fixtures/report.json");
const PUBLISHED_SCHEMA: &str = include_str!("../../../docs/api/report.schema.json");

#[test]
fn report_schema_is_current_and_validates_mock_fixture() {
    let output_dir = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .arg("schema")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let generated: Value = serde_json::from_slice(&result.stdout).unwrap();
    let published: Value = serde_json::from_str(PUBLISHED_SCHEMA).unwrap();
    assert_eq!(
        generated, published,
        "Обновите схему командой cargo run -p cli -- schema -o docs/api/report.schema.json"
    );

    let validator = jsonschema::validator_for(&published).unwrap();
    let report: Value = serde_json::from_str(MOCK_FIXTURE).unwrap();
    validator.validate(&report).unwrap();
}

#[test]
fn mock_fixture_matches_current_report() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("report.json");
    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .arg("check")
        .arg(workspace.join("samples/requirements.md"))
        .args(["--provider", "mock", "--format", "json", "--no-cache", "-o"])
        .arg(&report_path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let mut actual: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    let mut fixture: Value = serde_json::from_str(MOCK_FIXTURE).unwrap();
    for report in [&mut actual, &mut fixture] {
        let passport = report["passport"].as_object_mut().unwrap();
        assert!(passport.remove("date").is_some());
        assert!(passport.remove("agent_version").is_some());
    }
    assert_eq!(
        actual, fixture,
        "Обновите mock-фикстуру командой из docs/api/README.md"
    );
}

#[test]
fn published_schema_matches_approved_version() {
    let mut schema: Value = serde_json::from_str(PUBLISHED_SCHEMA).unwrap();
    let report: Value = serde_json::from_str(MOCK_FIXTURE).unwrap();
    let version = report["schema_version"].as_str().unwrap();
    let approved = match version {
        "1" => "1bd17f5d278ce6d7ed54a315ca2d6bdb50034bd8395a0aeb0c36ceaa58fc7983",
        _ => panic!("Добавьте контрольную сумму для новой версии схемы {version}"),
    };

    schema.sort_all_objects();
    let digest = Sha256::digest(serde_json::to_vec(&schema).unwrap());
    let checksum: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        checksum, approved,
        "Схема версии {version} изменилась. Для несовместимого изменения поднимите версию"
    );
}
