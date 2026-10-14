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

`init` downloads SLOOP and language analyzer JARs directly from **Maven Central**, a full **Eclipse Temurin Java 21 JRE** from Adoptium's GitHub releases, and **Node.js 24 LTS** from nodejs.org. It does not use IntelliJ, a JetBrains Marketplace ZIP, Gradle, Artifactory credentials, preinstalled Java/Node, or files from this repository's IntelliJ plugin. The CLI's own `runtime-manifest.json` pins component versions, source URLs, and SHA-256 digests.

The current catalog (`2026-10-13.1`) provisions SLOOP `12.1.0.87730`, Temurin `21.0.12.1+1`, Node `24.21.0`, and eleven analyzers for Java, Python, JavaScript/TypeScript/CSS, HTML, Kotlin, Ruby, XML, Go, PHP, IaC/YAML, and secrets detection. Backend and analyzer releases are selected and verified together by this client; their version numbers are independent.

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

Put `config.toml` in the project's `.rustmarine/` directory, or use `--config /path/to/config.toml`. Unknown configuration keys are rejected. Directory/JAR paths are relative to the configuration file (the `.rustmarine/` directory by default). Bare Java/Node executable names use `PATH`; paths containing directories are resolved relative to the configuration file. CLI and environment runtime options take precedence over configuration, which takes precedence over managed defaults. Individual `--plugin` values are added to configured plugins.

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

To disable a rule using its full key from a finding:

```sh
rustmarine ignore typescript:S1135 --root /path/to/project
rustmarine ignore typescript:S1134 --root /path/to/project
```

`ignore` creates `.rustmarine/config.toml` when needed and sets `[rules."<rule-key>"].active = false`. It preserves existing comments, rule parameters, other settings, and file permissions, and saves changes atomically. Repeating an already disabled rule leaves the file untouched. Use `--config /path/to/config.toml` to update another configuration; relative `--config` paths are resolved from the current directory, as with scans. Malformed or unsupported configuration is rejected before writing. The command works without a Sonar runtime and validates the key's `repository:rule` format; available rule keys come from analyzer findings. TypeScript and JavaScript rule keys are separate.

Success returns JSON with `schema_version`, the absolute `config_path`, `rule_key`, `active: false`, and `changed`. Exit status is 0, including when the rule was already disabled; operational errors return the JSON `error.message` envelope and exit 2. Argument errors use Clap's stderr diagnostics. The setting takes effect on subsequent scans. From the checkout, `make ignore ARGS='typescript:S1135 --root /path/to/project'` runs the same command.

Manage saved rule overrides with `config rule`:

| Command | Behavior |
| --- | --- |
| `config rule list` | Show all saved overrides, sorted by rule key |
| `config rule list --language LANGUAGE` | Show saved overrides for one exact rule language/repository prefix |
| `config rule get RULE` | Show one saved override; `configured: false` and `rule: null` when absent |
| `config rule set RULE PARAMETER=VALUE ...` | Merge one or more parameters, preserving unspecified settings |
| `config rule set RULE --active true\|false` | Set activation, optionally together with parameters |
| `config rule delete RULE` | Remove the entire local override, restoring analyzer defaults |
| `config rule delete RULE PARAMETER ...` | Remove only the listed parameters, preserving activation and other parameters |

```sh
rustmarine config rule list --root /path/to/project
rustmarine config rule list --language go --root /path/to/project
rustmarine config rule list --language typescript --root /path/to/project
rustmarine config rule get go:S3776 --root /path/to/project
rustmarine config rule set go:S3776 threshold=20 --root /path/to/project
rustmarine config rule set java:S107 max=7 constructorMax=10 --root /path/to/project
rustmarine config rule set java:S100 'format=^[a-z][a-zA-Z0-9]*$' --root /path/to/project
rustmarine config rule set typescript:S1135 --active false --root /path/to/project
rustmarine config rule delete java:S107 constructorMax --root /path/to/project
rustmarine config rule delete java:S107 max constructorMax --root /path/to/project
rustmarine config rule delete go:S3776 --root /path/to/project
```

`set go:S3776 threshold=20` saves `parameters = { threshold = "20" }` under `[rules."go:S3776"]` in `.rustmarine/config.toml`. New overrides default to `active = true` unless `--active false` is supplied. Existing activation preferences remain unchanged unless `--active` is supplied. At least one parameter or `--active` is required. Parameters merge with existing values; duplicate parameter assignments in one command are rejected. Values are stored exactly as strings, including empty values, booleans, numbers, lists, and regular expressions. Quote assignments containing spaces or shell metacharacters. Parameter names are case-sensitive; for example, Java's S3776 uses `Threshold`, while Go's uses `threshold`.

All commands support `--root` and `--config`. Relative `--config` paths are resolved from the current directory. They work without an analysis runtime; `list` and `get` inspect local overrides, not available analyzer rules or effective defaults. `list --language` matches the exact, case-sensitive prefix before `:` in a rule key, such as `go`, `typescript`, `javascript`, or `java`. JavaScript and TypeScript are separate; use the prefix from a finding for other rule repositories. Omitting the filter lists every saved override; a language with no overrides returns an empty `rules` map. Missing configuration is treated as empty: reads and deletion of absent overrides create no files. `delete` leaves existing activation preferences intact when removing only parameters; deleting a disabled rule's entire override can enable it again through the analyzer's default activation. Deleted parameters return to analyzer defaults on the next scan.

Edits save atomically, preserving unrelated settings, comments, file permissions, and symlinks. Repeating identical settings or deleting an absent override/parameter leaves the file untouched. Invalid TOML and unknown config fields are rejected before writing. The commands validate rule/parameter name syntax, but do not look up analyzer metadata or validate supported parameters, value types, or ranges. Changes take effect on subsequent scans. The former `config threshold` command is replaced by `config rule set`.

Success returns JSON with `schema_version` and absolute `config_path`. `list` includes a `rules` map; `get`, `set`, and `delete` include `rule_key`, `configured`, and a `rule` object containing `active` and `parameters` (or `null` when absent). Edits also include `changed`. Exit status is 0 even when unchanged or unconfigured; operational errors return the JSON `error.message` envelope and exit 2, while argument errors use stderr. From the checkout, use `make config ARGS='rule set go:S3776 threshold=20 --root /path/to/project'`. `ignore RULE` remains a shortcut for setting `--active false`.

### Test and production files

Rustmarine submits both production and test files, setting Sonar's `isTest` flag explicitly. By default, tests include Go `*_test.go`, Python `test_*.py` and `*_test.py`, JavaScript/TypeScript `*.test.*` and `*.spec.*` (including JSX/TSX and module variants), and files inside directories named `test`, `tests`, `_tests_`, `__tests__`, or `e2e` at any depth. Other files are classified as production. This classification lets each analyzer apply its test/production rule scopes; it does not exclude test files or guarantee that every analyzer checks tests with every rule.

Add project-specific test helpers and production exceptions in `.rustmarine/config.toml`:

```toml
[file_classification]
# Custom tests extend the built-in conventions.
test_patterns = ["ui/src/testSession.ts", "integration/**"]
# Production overrides take precedence over all test patterns.
main_patterns = ["tests/production/**"]
# Set false to use only your custom patterns (or classify everything as production).
use_defaults = true
```

Patterns are case-sensitive globs against paths relative to the **scan root**, including when using `--config` elsewhere. Use `/` as the separator; `*` matches within one path component and `**` spans directories. To match a basename at any depth, use a pattern such as `**/*_test.go`. Invalid globs are configuration errors. Files matching neither test nor production patterns are production files; production patterns never remove files from the scan.

JSON reports include `test_files`, a sorted subset of `scanned_files`; the other submitted files are production files. The text summary includes production/test counts, SARIF stores `testFiles` in the run's `properties`, and saved logs record the submitted classification counts. Classification describes the client's inputs, not an analyzer coverage guarantee. Analyzers may also use their own test heuristics: for example, Python can still treat a conventional test filename as test code after a production override.

The standalone language set enabled by this client is: HTML, XML, JavaScript, TypeScript, CSS, Kotlin, PHP, Python, Ruby, YAML/IaC, Java, Go, and secrets detection. Actual rules depend on the analyzer JARs supplied and their requirements. C/C++ and C# need additional IDE/build context and are not enabled by this initial client. Rust language analysis is not enabled in this standalone language set; a Rust implementation of the client does not add Rust analysis rules.

`scanned_files` records files submitted for analysis, after known backend exclusions. It is not a per-language coverage guarantee: SLOOP does not expose which analyzer actually inspected each file in the one-shot result. An unsupported file can produce no language findings; secrets detection may still inspect it. Backend exclusions are reported separately in `skipped_files`, and failed analyses in `failed_files`. Analyzer loading failures cause an operational error.

## Output contract

Successful JSON reports use `schema_version: 1` with:

- `root`: canonical absolute project directory.
- `scanned_files`: sorted project-relative paths submitted for analysis.
- `test_files`: sorted subset of submitted files classified as tests; remaining files are production.
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

The tests cover Git selection, configuration, framing, finding normalization, output formats, and command behavior. Separate ignored smoke tests verify real Java findings, exit thresholds, rule deactivation, and Python test/production classification with an external runtime:

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

### Coverage reports

Coverage uses [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) with branch instrumentation. The Makefile pins the reporter to `0.9.1` and the coverage compiler to `nightly-2026-10-09`. Normal builds and checks continue to use your default Rust toolchain.

```sh
make coverage-setup  # Install the pinned compiler, LLVM tools, and cargo-llvm-cov
make coverage        # Run regular tests and generate reports
```

Reports are saved under `target/coverage/`, which is already ignored by Git:

- `summary.txt`: per-file and total line, region, function, and branch coverage.
- `html/index.html`: browsable source with uncovered lines and branch counts.
- `coverage.json`: full LLVM coverage data for agents and other tools.
- `lcov.info`: coverage data for compatible editors and CI integrations.

Coverage measures Rustmarine's production Rust code, including instrumented CLI subprocesses launched by integration tests. Inline unit-test modules are excluded with `coverage(off)` during coverage builds; integration-test source and dependencies are excluded by cargo-llvm-cov's defaults. The Java/Node analyzers and scanned projects are outside these totals. Branch coverage uses unstable compiler instrumentation and may not represent every language construct; it is a guide for finding missing tests, not proof of correctness.

`make coverage` skips the opt-in engine and managed-runtime tests. To include the Java and Python engine tests, provide the same `RUSTMARINE_TEST_BACKEND_DIR`, `RUSTMARINE_TEST_ANALYZER_DIR`, and `RUSTMARINE_TEST_JAVA` settings used by `make smoke`, then run `make coverage-smoke`. This runs regular tests and engine tests together and saves combined reports under `target/coverage-smoke/`, preserving the regular-test reports. The managed-runtime download/repair smoke test remains separate.

Use `make coverage-report` to regenerate formats from the most recently collected profiles without rerunning tests. After `coverage-smoke`, use `make coverage-report COVERAGE_DIR=target/coverage-smoke` to keep the regular-test reports intact. Profiles are shared between coverage targets and cleared at the start of each run, so run them sequentially. Pass `COVERAGE_DIR=/path/to/reports` to Make to choose an output directory or `COVERAGE_TOOLCHAIN=nightly-YYYY-MM-DD` to try a different nightly. Coverage tools are installed only by `coverage-setup`; the reporting targets do not install missing prerequisites.

The standalone Rustmarine repository's `.github/workflows/coverage.yml` runs regular-test branch coverage only when manually triggered through GitHub Actions or `gh workflow run coverage.yml`, then uploads the report directory as an artifact. Pushes and pull requests do not trigger it. This workflow belongs to the nested `rust-cli` repository; GitHub will discover it when that repository is checked out as the repository root, and manual dispatch requires the workflow to exist on the default branch. No coverage threshold is enforced yet.
