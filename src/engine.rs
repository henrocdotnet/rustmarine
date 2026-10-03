//! A one-shot client for the same SonarLint backend used by the IDE plugin.
use crate::rpc::Rpc;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use url::Url;

pub struct EngineOptions {
    pub backend_dir: PathBuf,
    pub plugins: Vec<PathBuf>,
    pub java: PathBuf,
    pub node: Option<PathBuf>,
    pub timeout: Duration,
    pub properties: BTreeMap<String, String>,
    pub rules: BTreeMap<String, Value>,
    pub verbose: bool,
}

pub struct EngineReport {
    pub findings: Vec<Value>,
    pub failed_files: Vec<String>,
    pub skipped_files: BTreeMap<String, String>,
}

const SCOPE: &str = "rustmarine-project";
// The standalone language set from EnabledLanguages.kt, including Java and Go
// independent of installed IntelliJ language plugins.
const LANGUAGES: &[&str] = &[
    "HTML",
    "XML",
    "JS",
    "CSS",
    "KOTLIN",
    "PHP",
    "PYTHON",
    "RUBY",
    "SECRETS",
    "TS",
    "YAML",
    "CLOUDFORMATION",
    "DOCKER",
    "KUBERNETES",
    "TERRAFORM",
    "JAVA",
    "GO",
];

fn file_uri(path: &Path) -> Result<String> {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|()| anyhow::anyhow!("Cannot encode file URI for {}", path.display()))
}

pub fn analyze(options: &EngineOptions, root: &Path, files: &[PathBuf]) -> Result<EngineReport> {
    if options.timeout.is_zero() {
        bail!("Backend timeout must be greater than zero");
    }
    let backend = options.backend_dir.canonicalize().with_context(|| {
        format!(
            "Backend directory not found: {}",
            options.backend_dir.display()
        )
    })?;
    if !backend.join("lib").is_dir() {
        bail!("Backend directory must contain lib/: {}. Use the sloop directory from a SonarQube for IDE distribution.", backend.display());
    }
    if options.plugins.is_empty() {
        bail!("No analyzer plugins configured; provide --analyzer-dir or --plugin");
    }
    let plugins: Vec<PathBuf> = options
        .plugins
        .iter()
        .map(|plugin| {
            let path = plugin
                .canonicalize()
                .with_context(|| format!("Analyzer plugin not found: {}", plugin.display()))?;
            if !path.is_file() {
                bail!("Analyzer plugin is not a file: {}", path.display());
            }
            Ok(path)
        })
        .collect::<Result<_>>()?;
    let root = root.canonicalize()?;
    let mut dtos = Vec::new();
    let mut uris = Vec::new();
    for selected in files {
        let path = if selected.is_absolute() {
            selected.clone()
        } else {
            root.join(selected)
        };
        let path = path
            .canonicalize()
            .with_context(|| format!("Selected source file is unavailable: {}", path.display()))?;
        let relative = path
            .strip_prefix(&root)
            .context("Selected source file is outside project root")?;
        let uri = file_uri(&path)?;
        uris.push(uri.clone());
        dtos.push(json!({
            "uri":uri,"ideRelativePath":relative,"configScopeId":SCOPE,
            "isTest":null,"charset":"UTF-8","fsPath":path,"content":null,
            "detectedLanguage":null,"isUserDefined":true
        }));
    }
    if files.is_empty() {
        return Ok(EngineReport {
            findings: Vec::new(),
            failed_files: Vec::new(),
            skipped_files: BTreeMap::new(),
        });
    }

    let temporary = tempfile::Builder::new().prefix("rustmarine-").tempdir()?;
    let storage = temporary.path().join("storage");
    let work = temporary.path().join("work");
    std::fs::create_dir_all(&storage)?;
    std::fs::create_dir_all(&work)?;
    let bridge = plugins
        .iter()
        .filter_map(|path| path.parent())
        .map(|parent| parent.join("eslint-bridge"))
        .find(|path| path.is_dir());
    let mut command = Command::new(&options.java);
    command
        .args([
            "-Djava.awt.headless=true",
            "-Dsonarlint.telemetry.disabled=true",
            "-Dsonarlint.monitoring.disabled=true",
            "-Xms128m",
            "-classpath",
        ])
        .arg(backend.join("lib").join("*"))
        .arg("org.sonarsource.sonarlint.core.backend.cli.SonarLintServerCli")
        .current_dir(&work);
    let mut rpc = Rpc::spawn(&mut command, options.verbose)?;
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .context("Backend timeout is too large")?;
    let mut callbacks = Callbacks {
        root: &root,
        files: dtos,
        properties: &options.properties,
        ready: false,
        skipped: Vec::new(),
        verbose: options.verbose,
    };
    rpc.request("initialize", json!({
        "clientConstantInfo":{"name":"Rustmarine","userAgent":concat!("rustmarine/",env!("CARGO_PKG_VERSION"))},
        "telemetryConstantAttributes":{"productKey":"rustmarine","productName":"Rustmarine","productVersion":env!("CARGO_PKG_VERSION"),"ideVersion":"CLI","additionalAttributes":{}},
        "httpConfiguration":{"sslConfiguration":{}},
        "alternativeSonarCloudEnvironment":null,"backendCapabilities":[],
        "storageRoot":storage,"workDir":work,"embeddedPluginPaths":plugins,
        "connectedModeEmbeddedPluginPathsByKey":{},"enabledLanguagesInStandaloneMode":LANGUAGES,
        "extraEnabledLanguagesInConnectedMode":[],"disabledPluginKeysForAnalysis":[],
        "sonarQubeConnections":[],"sonarCloudConnections":[],"sonarlintUserHome":temporary.path(),
        "standaloneRuleConfigByKey":options.rules,"isFocusOnNewCode":false,
        "languageSpecificRequirements":{"jsTsRequirements":{"clientNodeJsPath":options.node,"bundlePath":bridge},"omnisharpDownloadEnabled":false},
        "automaticAnalysisEnabled":false,"telemetryMigration":null,
        "logLevel":if options.verbose {"DEBUG"} else {"WARN"}
    }), deadline, &mut |method, params| callbacks.handle(method, params))?;
    rpc.notify("configuration/didAddConfigurationScopes", json!({"addedScopes":[{
        "id":SCOPE,"parentId":null,"bindable":false,
        "name":"CLI project","binding":{"connectionId":null,"sonarProjectKey":null,"bindingSuggestionDisabled":true}
    }]}))?;
    while !callbacks.ready {
        rpc.pump(deadline, &mut |method, params| {
            callbacks.handle(method, params)
        })?;
    }
    let statuses = rpc.request(
        "file/getFilesStatus",
        json!({"fileUrisByConfigScopeId":{SCOPE:&uris}}),
        deadline,
        &mut |method, params| callbacks.handle(method, params),
    )?;
    let statuses = statuses
        .get("fileStatuses")
        .and_then(Value::as_object)
        .context("Sonar backend file status response is missing fileStatuses")?;
    let mut skipped_files = BTreeMap::new();
    for uri in &uris {
        let excluded = statuses
            .get(uri)
            .and_then(|status| status.get("excluded"))
            .and_then(Value::as_bool)
            .with_context(|| format!("Sonar backend did not return file status for {uri}"))?;
        if excluded {
            skipped_files.insert(
                uri.clone(),
                "Excluded by the Sonar backend (for example Git ignore or size limit)".into(),
            );
        }
    }
    let result = rpc.request("analysis/analyzeFilesAndTrack", json!({
        "configurationScopeId":SCOPE,"analysisId":"b70c2000-6440-4000-8000-000000000001",
        "filesToAnalyze":uris,"extraProperties":options.properties,"shouldFetchServerIssues":false
    }), deadline, &mut |method, params| callbacks.handle(method, params))?;
    if !callbacks.skipped.is_empty() {
        bail!(
            "Sonar skipped an analyzer; scan is incomplete: {}",
            callbacks.skipped.join("; ")
        );
    }
    let findings = result
        .get("rawIssues")
        .and_then(Value::as_array)
        .context("Sonar backend analysis response is missing rawIssues")?
        .clone();
    let failed_files = result
        .get("failedAnalysisFiles")
        .and_then(Value::as_array)
        .context("Sonar backend analysis response is missing failedAnalysisFiles")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .context("Invalid failedAnalysisFiles entry")
        })
        .collect::<Result<_>>()?;
    // A shutdown failure must not erase completed findings. Drop still reaps the process.
    let shutdown_deadline = Instant::now() + Duration::from_secs(3);
    let _ = rpc.request(
        "shutdown",
        Value::Null,
        shutdown_deadline,
        &mut |method, params| callbacks.handle(method, params),
    );
    Ok(EngineReport {
        findings,
        failed_files,
        skipped_files,
    })
}

struct Callbacks<'a> {
    root: &'a Path,
    files: Vec<Value>,
    properties: &'a BTreeMap<String, String>,
    ready: bool,
    skipped: Vec<String>,
    verbose: bool,
}

impl Callbacks<'_> {
    fn handle(&mut self, method: &str, params: &Value) -> Result<Value> {
        if self.verbose && method != "log" {
            eprintln!("[sonar rpc] {method}");
        }
        match method {
            "listFiles" => Ok(json!({"files":self.files})),
            "getBaseDir" => Ok(json!({"baseDir":self.root})),
            "getInferredAnalysisProperties" => Ok(json!({"properties":self.properties})),
            "getFileExclusions" => Ok(json!({"fileExclusionPatterns":[]})),
            "getClientLiveInfo" => Ok(json!({"description":"Rustmarine"})),
            "startProgress" => Ok(Value::Null),
            "selectProxies" => Ok(json!({"proxies":[]})),
            "getTelemetryLiveAttributes" => Ok(json!({"additionalAttributes":{}})),
            "showMessageRequest" => Ok(json!({"selectedAction":null})),
            "didChangeAnalysisReadiness" => {
                if params
                    .get("configurationScopeIds")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(SCOPE)))
                {
                    self.ready = params
                        .get("areReadyForAnalysis")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                Ok(Value::Null)
            }
            "didSkipLoadingPlugin" => {
                self.skipped.push(params.to_string());
                Ok(Value::Null)
            }
            "log" => {
                if self.verbose || params.get("level").and_then(Value::as_str) == Some("ERROR") {
                    if let Some(message) = params.get("message").and_then(Value::as_str) {
                        eprintln!("[sonar] {message}");
                    }
                }
                if params
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| {
                        message.contains("Unable to create instance of class")
                            && message.contains("InitializeParams")
                    })
                {
                    bail!("The Java runtime cannot deserialize the Sonar protocol. Use a full Java 21+ runtime with jdk.unsupported (Java 21 is recommended).");
                }
                Ok(Value::Null)
            }
            "raiseIssues"
            | "raiseHotspots"
            | "reportProgress"
            | "didChangePluginStatuses"
            | "showMessage"
            | "showSoonUnsupportedMessage"
            | "didDetectSecret"
            | "promoteExtraEnabledLanguagesInConnectedMode"
            | "noBindingSuggestionFound"
            | "suggestBinding"
            | "suggestConnection"
            | "didSynchronizeConfigurationScopes" => Ok(Value::Null),
            other => bail!("Unsupported Sonar backend callback: {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_the_requested_scope_and_true_state() {
        let properties = BTreeMap::new();
        let mut callbacks = Callbacks {
            root: Path::new("/project"),
            files: vec![],
            properties: &properties,
            ready: false,
            skipped: vec![],
            verbose: false,
        };
        callbacks
            .handle(
                "didChangeAnalysisReadiness",
                &json!({"configurationScopeIds":["other"],"areReadyForAnalysis":true}),
            )
            .unwrap();
        assert!(!callbacks.ready);
        callbacks
            .handle(
                "didChangeAnalysisReadiness",
                &json!({"configurationScopeIds":[SCOPE],"areReadyForAnalysis":true}),
            )
            .unwrap();
        assert!(callbacks.ready);
        callbacks
            .handle(
                "didChangeAnalysisReadiness",
                &json!({"configurationScopeIds":[SCOPE],"areReadyForAnalysis":false}),
            )
            .unwrap();
        assert!(!callbacks.ready);
    }

    #[test]
    fn plugin_failures_and_incompatible_java_are_not_clean_scans() {
        let properties = BTreeMap::new();
        let mut callbacks = Callbacks {
            root: Path::new("/project"),
            files: vec![],
            properties: &properties,
            ready: false,
            skipped: vec![],
            verbose: false,
        };
        callbacks
            .handle(
                "didSkipLoadingPlugin",
                &json!({"language":"PYTHON","reason":"UNSUPPORTED_VERSION"}),
            )
            .unwrap();
        assert_eq!(callbacks.skipped.len(), 1);
        assert!(callbacks
            .handle(
                "log",
                &json!({"message":"Unable to create instance of class InitializeParams"})
            )
            .is_err());
    }
}
