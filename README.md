# Rustmarine

`rustmarine` is a Rust CLI for coding agents and humans. It selects source changes from Git, runs Sonar's out-of-process engine (SLOOP), and returns findings with rule keys, severity, locations, and flows. JSON is the default; text and SARIF are also available.

This first version implements **standalone local scanning**. The Rust binary manages file selection, configuration, engine communication, and output. Sonar's existing Java analyzers perform the analysis. Connected SonarQube/SonarCloud features, issue synchronization, IDE quick-fix application, and IDE-specific compiler configuration are outside this version.

## Build and runtime setup

Build with Rust 1.88 or later. From the Rustmarine checkout containing `Cargo.toml`:

```sh
make build      # Default target; produces target/release/rustmarine
make install    # Builds and installs rustmarine into Cargo's binary directory
make help       # Lists all targets
```

Make wraps Cargo commands. Without Make, use `cargo build --release --locked` and `cargo install --locked --path .`. From the enclosing IntelliJ repository, use `make -C rust-cli <target>`.

Provision the entire analysis environment with one command:

```sh
rustmarine init
rustmarine doctor
rustmarine scan --root /path/to/project
```

`init` downloads SLOOP and language analyzer JARs directly from **Maven Central**, a full **Eclipse Temurin Java 21 JRE** from Adoptium's GitHub releases, and **Node.js 22** from nodejs.org. It does not use IntelliJ, a JetBrains Marketplace ZIP, Gradle, Artifactory credentials, preinstalled Java/Node, or files from this repository's IntelliJ plugin. The CLI's own `runtime-manifest.json` pins component versions, source URLs, and SHA-256 digests.

The current catalog provisions SLOOP `11.10.0.86299`, Temurin `21.0.12.1+1`, Node `22.23.3`, and eleven analyzers for Java, Python, JavaScript/TypeScript/CSS, HTML, Kotlin, Ruby, XML, Go, PHP, IaC/YAML, and secrets detection. Backend and analyzer releases are selected and verified together by this client; their version numbers are independent.

Every download is verified before extraction. Setup stages the installation, executes the managed Java/Node binaries, and requires real Java, Python, and JavaScript findings before activating it. A successful `init` returns JSON with the installed versions and executable paths. Progress and errors go to stderr; an unsuccessful setup exits 2. Setup does not modify the project or its configuration.

Storage defaults to the current user's application data directory:

- macOS: `~/Library/Application Support/rustmarine`
- Linux: `$XDG_DATA_HOME/rustmarine`, normally `~/.local/share/rustmarine`
- Windows: `%LOCALAPPDATA%/rustmarine`

The directory contains `downloads/` (verified archives) and `releases/<catalog-id>-<platform>/` (the installed backend, analyzers, Java, Node, and integrity receipt). Scans and `doctor` discover the installed catalog automatically. To use another directory consistently:

```sh
export RUSTMARINE_RUNTIME_DIR=/path/to/rustmarine-runtime
rustmarine init
rustmarine scan --root /path/to/project
```

Alternatively, pass `--runtime-dir` to each command or put `runtime_dir` in project configuration for scans. Relative CLI paths are resolved from the current directory. Managed runtime support covers macOS, Windows, and glibc Linux on x86-64 and ARM64. Other platforms can supply external runtime paths. Only macOS ARM64 has been exercised end to end so far.

Repeating `init` checks the existing installation's file hashes and reruns analysis checks. Downloads are reused when their digests match. A damaged installation is diagnosed rather than silently used by `init`:

```sh
rustmarine init --repair         # Rebuild using verified cached or fresh downloads
rustmarine init --offline        # Check installed runtime, or install from a complete cache
rustmarine init --repair --offline
```

Offline setup makes no HTTP requests. It fails if a required archive is missing or corrupt. Failed setup leaves any previously installed runtime intact. Downloads and staged files are written atomically, installation commands are serialized with a file lock, and archives cannot write outside their extraction directory. Scans check installed file presence; the more expensive hash and analysis checks are performed by `init`.

Updates are explicit and reproducible: a new CLI release ships a reviewed runtime catalog with a new catalog ID, and running `init` installs that catalog alongside older releases. Setup never selects arbitrary latest components at runtime. For development, change versions and SHA-256 digests in `runtime-manifest.json`, bump its `id`, and run the standalone setup/analysis checks before releasing the CLI. Java/Node security updates also require a catalog revision. Automatic update discovery is not implemented.

External installations remain optional: `--backend-dir`, `--analyzer-dir`, repeatable `--plugin`, `--java`, and `--node` override managed defaults. `RUSTMARINE_BACKEND_DIR` and `RUSTMARINE_ANALYZER_DIR` are still supported. Providing an external backend opts out of managed component discovery, so the caller supplies its analyzers and Java. Backend binaries and analyzers are downloaded assets, not bundled in the Rust executable; their upstream licenses and notices are preserved in the distributions.

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
rustmarine scan --no-save
```

`--fail-on high` fails for CRITICAL/BLOCKER severity or HIGH/BLOCKER impacts. It does not remove lower-severity findings from the report. `--fail-on none` suppresses the findings exit status, while analysis failures still exit 2. Engine diagnostics go to stderr. Standard output contains only the selected report.

By default, every scan saves its report and engine log under **`<root>/.rustmarine/runs/YYYYMMDD_HHMMSS/`**, using local time at scan startup. The report matches standard output: `report.json`, `report.txt`, or `report.sarif`, depending on `--format`. `scan.log` records run status and engine diagnostics even without `--verbose`; `--verbose` adds debug diagnostics and echoes them to stderr. Operational failures save `report.json` with the error envelope and record the error in the log. Invalid command-line arguments and an inaccessible project/storage directory can fail before a run is created.

Use **`--no-save`** to keep output on stdout/stderr without creating run files. Timestamp collisions create directories with `_1`, `_2`, and so on rather than overwriting previous runs. Saved runs are excluded from automatic file selection, including Git changes and `--all`; explicit paths still bypass this exclusion. Runs are retained until you delete them. Add `.rustmarine/runs/` to your project's `.gitignore` if you want Git itself to ignore them. Reports and logs remain local and can contain sensitive findings.

| Exit | Meaning |
| --- | --- |
| `0` | Completed without findings meeting the threshold, or no selected files |
| `1` | Findings meet the configured threshold |
| `2` | Invalid arguments/configuration, engine failure, timeout, failed file analysis, or every selected file excluded |

A scan with no selected files succeeds without starting or configuring the engine. If an analysis returns failed files, its available findings are still printed and the exit code is 2. JSON-mode operational errors have an `error.message` envelope; Clap argument errors use its normal stderr diagnostics. The analysis deadline applies to startup, readiness, and analysis together. A short shutdown allowance follows completed analysis. The backend process is terminated and reaped on completion or error.

## Project configuration

Put `.rustmarine.toml` at the project root, or use `--config /path/to/config.toml`. Unknown configuration keys are rejected. Directory/JAR paths are relative to the configuration file. Bare Java/Node executable names use `PATH`; paths containing directories are resolved relative to the configuration file. CLI and environment runtime options take precedence over configuration, which takes precedence over managed defaults. Individual `--plugin` values are added to configured plugins.

```toml
# Optional: omit runtime paths to use the environment created by init.
# runtime_dir = "/path/to/rustmarine-runtime"

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

The standalone language set enabled by this client is: HTML, XML, JavaScript, TypeScript, CSS, Kotlin, PHP, Python, Ruby, YAML/IaC, Java, Go, and secrets detection. Actual rules depend on the analyzer JARs supplied and their requirements. C/C++ and C# need additional IDE/build context and are not enabled by this initial client. Rust language analysis is not enabled in this standalone language set; a Rust implementation of the client does not add Rust analysis rules.

`scanned_files` records files submitted for analysis, after known backend exclusions. It is not a per-language coverage guarantee: SLOOP does not expose which analyzer actually inspected each file in the one-shot result. An unsupported file can produce no language findings; secrets detection may still inspect it. Backend exclusions are reported separately in `skipped_files`, and failed analyses in `failed_files`. Analyzer loading failures cause an operational error.

## Output contract

Successful JSON reports use `schema_version: 1` with:

- `root`: canonical absolute project directory.
- `scanned_files`: sorted project-relative paths submitted for analysis.
- `findings`: sorted issues and hotspots, with `path`, `rule_key`, `message`, `severity`, `type`, `impacts`, `range`, `flows`, and `details`.
- `failed_files`: project-relative files that failed analysis.
- `skipped_files`: project-relative paths mapped to backend exclusion reasons.

Lines and columns in `range` are 1-based, columns count UTF-16 code units, and the end is exclusive. A null range denotes a file/project-level finding; path `.` denotes the project. `details` preserves the raw engine payload, including quick fixes and rule context when supplied. Engine metadata in `details` can change independently of the normalized fields. Malformed findings cause an error rather than being discarded.

SLOOP launches as a child process over bidirectional Content-Length-framed JSON-RPC. The client initializes standalone analyzers, registers one project scope, answers filesystem/property callbacks, waits for analysis readiness, then calls `analysis/analyzeFilesAndTrack`. Source files are read locally. No server connections are configured, and telemetry/monitoring are disabled. The managed Java/Node binaries and analyzer assets are provisioned by init; ordinary scans do not download the managed runtime.

## Validation

```sh
make check      # Formatting check, Clippy, and unit/integration tests
make test       # Unit/integration tests only
make fmt        # Apply Rust formatting
```

`make check` leaves source formatting unchanged. Use `make run ARGS='--help'` to run the CLI, or `make scan ARGS='--root /path/to/project --format text'` to scan. `make init` and `make doctor` run their corresponding commands; each accepts `ARGS`. Set `CARGO=/path/to/cargo` to override the Cargo executable.

The tests cover Git selection, configuration, framing, finding normalization, output formats, and command behavior. A separate ignored smoke test verifies real Java findings, exit thresholds, and rule deactivation with an external runtime:

```sh
RUSTMARINE_TEST_BACKEND_DIR="$RUSTMARINE_BACKEND_DIR" \
RUSTMARINE_TEST_ANALYZER_DIR="$RUSTMARINE_ANALYZER_DIR" \
RUSTMARINE_TEST_JAVA="$JAVA_HOME/bin/java" \
make smoke
```

Cargo dependencies and `Cargo.lock` are independent of the IntelliJ Gradle dependency locks.

A standalone managed-runtime smoke test can download a complete environment and verify discovery, offline reuse, repair, and real Java/Python/JavaScript findings without any IDE installation:

```sh
make smoke-managed
```
