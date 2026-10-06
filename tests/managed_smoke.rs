//! Optional network test: provisions only public upstream assets, never IDE/plugin files.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::Path,
    process::{Command, Output},
};

fn run(runtime: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustmarine"));
    command
        .env("RUSTMARINE_RUNTIME_DIR", runtime)
        .env_remove("JAVA_HOME")
        .env_remove("RUSTMARINE_BACKEND_DIR")
        .env_remove("RUSTMARINE_ANALYZER_DIR")
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH");
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    command.args(args).output().unwrap()
}

fn success(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "downloads a full standalone runtime; optional RUSTMARINE_TEST_RUNTIME_DIR reuses a test cache"]
fn standalone_init_discovery_offline_reuse_and_repair() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = env::var_os("RUSTMARINE_TEST_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("runtime"));
    let install = success(run(&runtime, &["init"]));
    let canonical = runtime.canonicalize().unwrap();
    for key in ["java", "node", "backend_dir", "analyzer_dir"] {
        assert!(Path::new(install[key].as_str().unwrap()).starts_with(&canonical));
    }
    let project = temporary.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("Check.java"),
        "class Check { boolean same(String a, String b) { return a == b; } }\n",
    )
    .unwrap();
    fs::write(
        project.join("check.py"),
        "def bad():\n    if True:\n        return 1\n    return 0\n",
    )
    .unwrap();
    fs::write(
        project.join("check.js"),
        "function compare(value) { return value == value; }\n",
    )
    .unwrap();
    let report = success(run(
        &runtime,
        &[
            "scan",
            "--root",
            project.to_str().unwrap(),
            "--all",
            "--fail-on",
            "none",
        ],
    ));
    assert!(report["failed_files"].as_array().unwrap().is_empty());
    assert!(report["skipped_files"].as_object().unwrap().is_empty());
    for prefix in ["java:", "python:", "javascript:"] {
        assert!(
            report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule_key"].as_str().unwrap().starts_with(prefix)),
            "Missing {prefix}: {report}"
        );
    }
    success(run(
        &runtime,
        &["doctor", "--root", project.to_str().unwrap()],
    ));
    let offline = run(&runtime, &["init", "--offline"]);
    assert!(!String::from_utf8_lossy(&offline.stderr).contains("Downloading"));
    assert_eq!(success(offline)["release_dir"], install["release_dir"]);

    let release = Path::new(install["release_dir"].as_str().unwrap());
    let receipt: Value =
        serde_json::from_slice(&fs::read(release.join("installation.json")).unwrap()).unwrap();
    let (relative, digest) = receipt["files"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(path, _)| Path::new(path).components().next().unwrap().as_os_str() == "analyzers")
        .unwrap();
    let damaged = release.join(relative);
    fs::write(&damaged, "corrupt analyzer").unwrap();
    let output = run(&runtime, &["init", "--offline"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("corrupt"));
    let repaired = run(&runtime, &["init", "--repair", "--offline"]);
    assert!(!String::from_utf8_lossy(&repaired.stderr).contains("Downloading"));
    assert_eq!(success(repaired)["release_dir"], install["release_dir"]);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(damaged).unwrap())),
        digest.as_str().unwrap()
    );
}
