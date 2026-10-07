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
        .env("RUSTMARINE_RUNTIME_DIR", root.join(".test-runtime"))
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
    assert!(text.contains("--no-save"));
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

fn runs(root: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(root.join(".rustmarine/runs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

#[test]
fn scans_save_reports_without_selecting_previous_runs_or_overwriting_them() {
    let directory = clean_repository();
    for _ in 0..2 {
        let output = cli(directory.path(), &["scan"]);
        assert_eq!(output.status.code(), Some(0));
        assert!(json_stdout(&output)["scanned_files"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(output.stderr.is_empty());
        assert!(runs(directory.path())
            .iter()
            .any(|run| { fs::read(run.join("report.json")).unwrap() == output.stdout }));
    }
    let saved = runs(directory.path());
    assert_eq!(saved.len(), 2);
    for run in saved {
        let stamp = run.file_name().unwrap().to_str().unwrap();
        assert!(stamp.len() >= 15);
        assert_eq!(&stamp[8..9], "_");
        assert!(stamp[..8].chars().all(|c| c.is_ascii_digit()));
        assert!(stamp[9..15].chars().all(|c| c.is_ascii_digit()));
        assert!(fs::read_to_string(run.join("scan.log"))
            .unwrap()
            .contains("Scan completed"));
    }
    let output = cli(directory.path(), &["files", "--all"]);
    assert_eq!(json_stdout(&output)["files"], json!(["source.rs"]));
}

#[test]
fn no_save_creates_no_run_directory_including_on_errors() {
    let directory = clean_repository();
    let output = cli(directory.path(), &["scan", "--no-save"]);
    assert_eq!(output.status.code(), Some(0));
    let output = cli(directory.path(), &["scan", "--no-save", "missing.rs"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(json_stdout(&output)["error"].is_object());
    assert!(!directory.path().join(".rustmarine").exists());
}

#[test]
fn scans_save_selected_output_format_and_operational_errors() {
    for (format, filename) in [("text", "report.txt"), ("sarif", "report.sarif")] {
        let directory = clean_repository();
        let output = cli(directory.path(), &["scan", "--format", format]);
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            fs::read(runs(directory.path())[0].join(filename)).unwrap(),
            output.stdout
        );
    }
    let directory = clean_repository();
    let output = cli(directory.path(), &["scan", "missing.rs"]);
    assert_eq!(output.status.code(), Some(2));
    let run = &runs(directory.path())[0];
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(run.join("report.json")).unwrap()).unwrap(),
        json_stdout(&output)
    );
    assert!(fs::read_to_string(run.join("scan.log"))
        .unwrap()
        .contains("missing.rs"));
}

#[cfg(unix)]
#[test]
fn backend_stderr_is_saved_without_verbose_and_after_backend_exit() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir_all(root.join("backend/lib")).unwrap();
    fs::write(root.join("analyzer.jar"), "test placeholder").unwrap();
    fs::write(root.join("source.py"), "print('hello')\n").unwrap();
    let java = root.join("fake-java");
    fs::write(
        &java,
        "#!/bin/sh\necho 'test backend diagnostic' >&2\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();
    let output = cli(
        root,
        &[
            "scan",
            "--backend-dir",
            "backend",
            "--plugin",
            "analyzer.jar",
            "--java",
            java.to_str().unwrap(),
            "source.py",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let run = &runs(root)[0];
    assert!(fs::read_to_string(run.join("scan.log"))
        .unwrap()
        .contains("[sonar stderr] test backend diagnostic"));
    assert!(run.join("report.json").is_file());
}
