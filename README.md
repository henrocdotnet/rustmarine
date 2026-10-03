# Rustmarine

`rustmarine` is a Rust CLI for coding agents and humans. It selects source changes from Git, runs the same SonarLint out-of-process engine (SLOOP) used by this IntelliJ plugin, and returns findings with rule keys, severity, locations, and flows. JSON is the default; text and SARIF are also available.

This first version implements **standalone local scanning**. The Rust binary manages file selection, configuration, engine communication, and output. Sonar's existing Java analyzers perform the analysis. Connected SonarQube/SonarCloud features, issue synchronization, IDE quick-fix application, and IDE-specific compiler configuration are outside this version.

## Build and runtime setup

Build with Rust 1.88 or later:

```sh
cargo build --release --locked --manifest-path rust-cli/Cargo.toml
cargo install --locked --path rust-cli
```

Analysis also requires a full Java 21+ runtime and the Sonar runtime/analyzers. Java 21 is the tested choice; stripped IDE runtimes can lack facilities required by the backend. The CLI does not require IntelliJ to be running.

Use an installed SonarQube for IDE plugin or extract a plugin ZIP downloaded from the [JetBrains Marketplace](https://plugins.jetbrains.com/plugin/7973-sonarqube-for-ide). Keep its backend and analyzers from the same distribution:

```text
sonarlint-intellij/
  sloop/
    lib/*.jar            # backend and its dependencies
  plugins/
    sonar-*-plugin*.jar  # language analyzers
    eslint-bridge/       # when included by that distribution
```

The IntelliJ plugin's `lib/` directory is distinct from the analyzer `plugins/` directory. Pass the directories above explicitly:

```sh
export RUSTMARINE_BACKEND_DIR=/path/to/sonarlint-intellij/sloop
export RUSTMARINE_ANALYZER_DIR=/path/to/sonarlint-intellij/plugins
export JAVA_HOME=/path/to/full/jdk-21
rustmarine doctor --root /path/to/project
rustmarine scan --root /path/to/project
```

For example, installed plugins on macOS often live beneath `~/Library/Application Support/JetBrains/<product-version>/plugins/sonarlint-intellij/`. Paths containing spaces should be quoted. `--backend-dir`, `--analyzer-dir`, `--plugin` (repeatable), and `--java` can replace the environment settings. `doctor` validates paths and the Java version; a scan validates engine compatibility.

The protocol follows the API used by this repository's `BackendService.kt` (core version `12.1.0.87590`). Real scanning has been verified with an installed backend `10.47.0.84936` and its matching analyzers on Java `21.0.5`. Other distributions may change the protocol or language requirements. Backend binaries and analyzer JARs are not bundled in the Cargo package; their upstream licenses still apply.

## Agent workflow

```sh
# Preview changed file selection without starting Java
rustmarine files --root /path/to/project

# Staged + unstaged tracked changes compared to HEAD, plus untracked files
rustmarine scan --root /path/to/project > findings.json

# Changes since the merge base with main, including working-tree changes
rustmarine scan --root /path/to/project --base main

# Choose files explicitly; names are relative to --root
rustmarine scan --root /path/to/project src/example.py src/example.java
```

Selection uses NUL-delimited Git paths, so spaces and newlines in filenames work. Deleted files are omitted. Files are canonicalized, sorted, deduplicated, and restricted to the project root. Symlinks outside the root are skipped during discovery and rejected for explicit inputs.

`--staged` selects files changed in the index and **analyzes their current working-tree contents**, rather than an index snapshot. `--all` walks a project without requiring Git, honors `.gitignore`/`.ignore` and global Git ignores, and includes hidden files. Explicit files bypass ignore rules. `--base` compares the merge base to the current working tree. A repository with no commits is supported for the default and staged modes.

Analysis covers the whole selected file, so findings can include pre-existing issues on unchanged lines. This version does not compute a findings baseline or filter to changed lines.

## Human and CI usage

```sh
rustmarine scan --format text
rustmarine scan --all --format sarif > findings.sarif
rustmarine scan --staged --format text
rustmarine scan --timeout 120 --verbose
rustmarine scan --fail-on high
rustmarine scan --fail-on none --format text
```

`--fail-on high` fails for CRITICAL/BLOCKER severity or HIGH/BLOCKER impacts. It does not remove lower-severity findings from the report. `--fail-on none` suppresses the findings exit status, while analysis failures still exit 2. Engine diagnostics go to stderr. Standard output contains only the selected report.

| Exit | Meaning |
| --- | --- |
| `0` | Completed without findings meeting the threshold, or no selected files |
| `1` | Findings meet the configured threshold |
| `2` | Invalid arguments/configuration, engine failure, timeout, failed file analysis, or every selected file excluded |

A scan with no selected files succeeds without starting or configuring the engine. If an analysis returns failed files, its available findings are still printed and the exit code is 2. JSON-mode operational errors have an `error.message` envelope; Clap argument errors use its normal stderr diagnostics. The analysis deadline applies to startup, readiness, and analysis together. A short shutdown allowance follows completed analysis. The backend process is terminated and reaped on completion or error.

## Project configuration

Put `.rustmarine.toml` at the project root, or use `--config /path/to/config.toml`. Unknown configuration keys are rejected. Directory/JAR paths are relative to the configuration file. Bare Java/Node executable names use `PATH`; paths containing directories are resolved relative to the configuration file. CLI and environment runtime options take precedence over configuration. Individual `--plugin` values are added to configured plugins.

```toml
backend_dir = "/path/to/sonarlint-intellij/sloop"
analyzer_dir = "/path/to/sonarlint-intellij/plugins"
java = "/path/to/full/jdk-21/bin/java"
node = "/path/to/node"  # optional, for JavaScript/TypeScript

[properties]
"sonar.java.binaries" = "/path/to/project/build/classes/java/main"
"sonar.java.libraries" = "/path/to/project/lib/dependency.jar"

[rules."python:S5797"]
active = false

[rules."java:S100"]
active = true
parameters = { format = "^[a-z][a-zA-Z0-9]*$" }
```

A repeatable `--property KEY=VALUE` overrides configured analysis properties. Java, Kotlin, and other analyzers may need compiler outputs, dependencies, or language-specific properties to provide full semantic analysis. Properties such as `sonar.java.binaries` that refer to project files should use absolute paths when required by the analyzer; the backend work directory is separate from the project.

The standalone language set follows this IntelliJ client's standard languages plus Java and Go, independent of installed IDE language plugins: HTML, XML, JavaScript, TypeScript, CSS, Kotlin, PHP, Python, Ruby, YAML/IaC, Java, Go, and secrets detection. Actual rules depend on the analyzer JARs supplied and their requirements. C/C++ and C# need additional IDE/build context and are not enabled by this initial client. Rust language analysis is not enabled in this standalone language set; a Rust implementation of the client does not add Rust analysis rules.

`scanned_files` records files submitted for analysis, after known backend exclusions. It is not a per-language coverage guarantee: SLOOP does not expose which analyzer actually inspected each file in the one-shot result. An unsupported file can produce no language findings; secrets detection may still inspect it. Backend exclusions are reported separately in `skipped_files`, and failed analyses in `failed_files`. Analyzer loading failures cause an operational error.

## Output contract

Successful JSON reports use `schema_version: 1` with:

- `root`: canonical absolute project directory.
- `scanned_files`: sorted project-relative paths submitted for analysis.
- `findings`: sorted issues and hotspots, with `path`, `rule_key`, `message`, `severity`, `type`, `impacts`, `range`, `flows`, and `details`.
- `failed_files`: project-relative files that failed analysis.
- `skipped_files`: project-relative paths mapped to backend exclusion reasons.

Lines and columns in `range` are 1-based, columns count UTF-16 code units, and the end is exclusive. A null range denotes a file/project-level finding; path `.` denotes the project. `details` preserves the raw engine payload, including quick fixes and rule context when supplied. Engine metadata in `details` can change independently of the normalized fields. Malformed findings cause an error rather than being discarded.

SLOOP launches as a child process over bidirectional Content-Length-framed JSON-RPC. The client initializes standalone analyzers, registers one project scope, answers filesystem/property callbacks, waits for analysis readiness, then calls `analysis/analyzeFilesAndTrack`. Source files are read locally. No server connections are configured, and telemetry/monitoring are disabled. Runtime downloads or analyzer provisioning can still depend on the distribution and configured language requirements.

## Validation

```sh
cargo test --locked --manifest-path rust-cli/Cargo.toml
cargo fmt --check --manifest-path rust-cli/Cargo.toml
cargo clippy --locked --manifest-path rust-cli/Cargo.toml --all-targets -- -D warnings
```

The tests cover Git selection, configuration, framing, finding normalization, output formats, and command behavior. A separate ignored smoke test verifies real Java findings, exit thresholds, and rule deactivation:

```sh
RUSTMARINE_TEST_BACKEND_DIR="$RUSTMARINE_BACKEND_DIR" \
RUSTMARINE_TEST_ANALYZER_DIR="$RUSTMARINE_ANALYZER_DIR" \
RUSTMARINE_TEST_JAVA="$JAVA_HOME/bin/java" \
cargo test --locked --manifest-path rust-cli/Cargo.toml --test engine_smoke -- --ignored
```

Cargo dependencies and `Cargo.lock` are independent of the IntelliJ Gradle dependency locks.
