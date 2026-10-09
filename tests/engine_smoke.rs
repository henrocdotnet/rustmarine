//! Run explicitly with a compatible, locally supplied Sonar distribution.
use serde_json::Value;
use std::{env, fs, process::Command};

#[test]
#[ignore = "requires RUSTMARINE_TEST_BACKEND_DIR, RUSTMARINE_TEST_ANALYZER_DIR and RUSTMARINE_TEST_JAVA"]
fn real_engine_reports_findings_and_honors_configuration() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join(".rustmarine")).unwrap();
    fs::write(
        project.path().join("Bad.java"),
        r#"class Bad {
  public boolean compare(String left, String right) {
    return left == right;
  }
}
"#,
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustmarine"));
    command
        .args(["scan", "--root"])
        .arg(project.path())
        .args(["Bad.java", "--backend-dir"])
        .arg(env::var_os("RUSTMARINE_TEST_BACKEND_DIR").expect("RUSTMARINE_TEST_BACKEND_DIR"))
        .arg("--analyzer-dir")
        .arg(env::var_os("RUSTMARINE_TEST_ANALYZER_DIR").expect("RUSTMARINE_TEST_ANALYZER_DIR"))
        .arg("--java")
        .arg(env::var_os("RUSTMARINE_TEST_JAVA").expect("RUSTMARINE_TEST_JAVA"))
        .args(["--timeout", "90"]);
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["failed_files"].as_array().unwrap().is_empty());
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["rule_key"] == "java:S4973"),
        "{report}"
    );

    // Exit threshold changes must preserve findings for agents.
    command.args(["--fail-on", "none"]);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["rule_key"] == "java:S4973"));

    let ignored = Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .args(["ignore", "java:S4973", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert_eq!(ignored.status.code(), Some(0), "{ignored:?}");
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["failed_files"].as_array().unwrap().is_empty());
    assert!(
        !report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["rule_key"] == "java:S4973"),
        "Rule deactivation failed: {report}"
    );
}

#[test]
#[ignore = "requires a Python analyzer and RUSTMARINE_TEST_BACKEND_DIR, RUSTMARINE_TEST_ANALYZER_DIR, RUSTMARINE_TEST_JAVA"]
fn real_engine_honors_test_classification_and_production_overrides() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join(".rustmarine")).unwrap();
    fs::create_dir(project.path().join("tests")).unwrap();
    fs::create_dir(project.path().join("checks")).unwrap();
    let source = "def run(prepare, execute, release):\n    prepare('/api/fixture')\n    execute('/api/fixture')\n    release('/api/fixture')\n";
    for path in ["checks/sample.py", "main.py", "tests/test_sample.py"] {
        fs::write(project.path().join(path), source).unwrap();
    }
    fs::write(
        project.path().join(".rustmarine/config.toml"),
        "[file_classification]\ntest_patterns = ['checks/**']\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustmarine"));
    command
        .args(["scan", "--root"])
        .arg(project.path())
        .args([
            "checks/sample.py",
            "main.py",
            "tests/test_sample.py",
            "--fail-on",
            "none",
            "--backend-dir",
        ])
        .arg(env::var_os("RUSTMARINE_TEST_BACKEND_DIR").expect("RUSTMARINE_TEST_BACKEND_DIR"))
        .arg("--analyzer-dir")
        .arg(env::var_os("RUSTMARINE_TEST_ANALYZER_DIR").expect("RUSTMARINE_TEST_ANALYZER_DIR"))
        .arg("--java")
        .arg(env::var_os("RUSTMARINE_TEST_JAVA").expect("RUSTMARINE_TEST_JAVA"))
        .args(["--timeout", "90"]);
    let scan = |command: &mut Command| {
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr: {}; stdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["failed_files"].as_array().unwrap().is_empty());
        report
    };
    let duplicate_files = |report: &Value| {
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|finding| finding["rule_key"] == "python:S1192")
            .map(|finding| finding["path"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let report = scan(&mut command);
    assert_eq!(
        report["scanned_files"],
        serde_json::json!(["checks/sample.py", "main.py", "tests/test_sample.py"])
    );
    assert_eq!(
        report["test_files"],
        serde_json::json!(["checks/sample.py", "tests/test_sample.py"])
    );
    assert_eq!(duplicate_files(&report), ["main.py"], "{report}");

    fs::write(
        project.path().join(".rustmarine/config.toml"),
        "[file_classification]\ntest_patterns = ['checks/**']\nmain_patterns = ['checks/**']\n",
    )
    .unwrap();
    let report = scan(&mut command);
    assert_eq!(
        report["test_files"],
        serde_json::json!(["tests/test_sample.py"])
    );
    assert_eq!(
        duplicate_files(&report),
        ["checks/sample.py", "main.py"],
        "{report}"
    );
}
