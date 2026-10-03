use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .current_dir(root)
        .env_remove("RUSTMARINE_BACKEND_DIR")
        .env_remove("RUSTMARINE_ANALYZER_DIR")
        .args(args)
        .output()
        .expect("run CLI")
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "Invalid JSON output ({error}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run Git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn clean_repository() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    fs::write(directory.path().join("source.rs"), "fn main() {}\n").unwrap();
    git(directory.path(), &["add", "source.rs"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=CLI Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=.git/no-hooks",
            "commit",
            "-qm",
            "initial",
        ],
    );
    directory
}

#[test]
fn help_exposes_commands_and_scan_contract() {
    let directory = tempfile::tempdir().unwrap();
    let output = cli(directory.path(), &["--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("scan") && text.contains("files") && text.contains("doctor"));
    assert!(text.contains("Findings exit 1") && text.contains("operational errors exit 2"));

    let output = cli(directory.path(), &["scan", "--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("--staged") && text.contains("--base") && text.contains("--format"));
    assert!(text.contains("current working-tree contents"));
}

#[test]
fn conflicting_selection_modes_are_usage_errors() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["scan", "--all", "--staged"],
        vec!["files", "--staged", "--base", "HEAD"],
        vec!["files", "--all", "source.rs"],
    ] {
        let output = cli(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    }
}

#[test]
fn files_reports_root_relative_json_paths_and_deduplicates() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("nested")).unwrap();
    fs::write(directory.path().join("nested/source file.rs"), "source").unwrap();
    let output = cli(
        directory.path(),
        &["files", "nested/source file.rs", "./nested/source file.rs"],
    );
    assert!(output.status.success());
    assert_eq!(
        json_stdout(&output),
        json!({
            "schema_version": 1,
            "root": directory.path().canonicalize().unwrap(),
            "files": ["nested/source file.rs"],
        })
    );
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn files_preserves_literal_backslashes_and_newlines_in_unix_names() {
    let directory = tempfile::tempdir().unwrap();
    let filename = "source\\with\nnewline.rs";
    fs::write(directory.path().join(filename), "source").unwrap();
    let output = cli(directory.path(), &["files", filename]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json_stdout(&output)["files"], json!([filename]));
}

#[test]
fn clean_scan_succeeds_without_configuring_a_runtime() {
    let directory = clean_repository();
    let output = cli(directory.path(), &["scan"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        json_stdout(&output),
        json!({
            "schema_version": 1,
            "root": directory.path().canonicalize().unwrap(),
            "scanned_files": [],
            "findings": [],
            "failed_files": [],
            "skipped_files": {},
        })
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn nonempty_scan_without_runtime_returns_machine_readable_error() {
    let directory = clean_repository();
    fs::write(directory.path().join("source.rs"), "fn changed() {}\n").unwrap();
    let output = cli(directory.path(), &["scan"]);
    assert_eq!(output.status.code(), Some(2));
    let report = json_stdout(&output);
    assert_eq!(report["schema_version"], 1);
    let message = report["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("SLOOP runtime is not configured"),
        "{message}"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn all_selects_non_git_projects_and_obeys_ignore_files() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(directory.path().join("source.rs"), "source").unwrap();
    fs::write(directory.path().join("ignored.rs"), "ignored").unwrap();
    let output = cli(directory.path(), &["files", "--all"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        json_stdout(&output)["files"],
        json!([".gitignore", "source.rs"])
    );

    // A non-Git scan must reach runtime validation, rather than failing selection.
    let output = cli(directory.path(), &["scan", "--all"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(json_stdout(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("SLOOP runtime is not configured"));
}

#[test]
fn missing_explicit_files_are_operational_errors() {
    let directory = tempfile::tempdir().unwrap();
    let output = cli(directory.path(), &["files", "missing.rs"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot resolve input file"));

    let output = cli(directory.path(), &["scan", "missing.rs"]);
    assert_eq!(output.status.code(), Some(2));
    let report = json_stdout(&output);
    assert_eq!(report["schema_version"], 1);
    assert!(report["error"]["message"]
        .as_str()
        .unwrap()
        .contains("missing.rs"));
}
