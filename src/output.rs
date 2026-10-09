use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Sarif,
}

#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub schema_version: u32,
    pub root: String,
    pub scanned_files: Vec<String>,
    /// Classified test files, a sorted subset of scanned_files.
    pub test_files: Vec<String>,
    pub findings: Vec<Finding>,
    pub failed_files: Vec<String>,
    pub skipped_files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    /// A project-relative path; "." denotes a project-level finding.
    pub path: String,
    pub rule_key: String,
    pub message: String,
    pub severity: String,
    #[serde(rename = "type")]
    pub issue_type: Option<String>,
    pub impacts: BTreeMap<String, String>,
    pub range: Option<TextRange>,
    pub flows: Vec<Flow>,
    /// Original engine payload, including quick fixes and rule context.
    pub details: Value,
}

/// Lines and columns are 1-based. End positions are exclusive; columns count UTF-16 code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TextRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Serialize)]
pub struct Flow {
    pub locations: Vec<FlowLocation>,
}

#[derive(Debug, Serialize)]
pub struct FlowLocation {
    pub path: Option<String>,
    pub message: Option<String>,
    pub range: Option<TextRange>,
}

pub fn normalize(root: &Path, raw: Vec<Value>) -> Result<Vec<Finding>> {
    let root = absolute_lexical(root)?;
    let mut findings = raw
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            normalize_one(&root, value).with_context(|| format!("invalid engine finding {index}"))
        })
        .collect::<Result<Vec<_>>>()?;
    findings.sort_by_cached_key(|f| {
        (
            f.path.clone(),
            f.range,
            f.rule_key.clone(),
            f.message.clone(),
            f.severity.clone(),
            f.details.to_string(),
        )
    });
    Ok(findings)
}

fn normalize_one(root: &Path, raw: Value) -> Result<Finding> {
    if !raw.is_object() {
        bail!("expected an issue object");
    }
    let path = match raw.get("fileUri") {
        Some(Value::Null) => ".".into(),
        Some(Value::String(uri)) => relative_path(root, uri)?,
        _ => bail!("missing or invalid fileUri"),
    };
    let rule_key = required_string(&raw, "ruleKey")?.to_owned();
    let message = required_string(&raw, "primaryMessage")?.to_owned();
    let severity = optional_string(&raw, "severity")?
        .unwrap_or("UNKNOWN")
        .to_owned();
    let issue_type = optional_string(&raw, "type")?.map(str::to_owned);
    let mut impacts = BTreeMap::new();
    match raw.get("impacts") {
        None | Some(Value::Null) => {}
        Some(Value::Object(values)) => {
            for (quality, severity) in values {
                impacts.insert(
                    quality.clone(),
                    severity
                        .as_str()
                        .ok_or_else(|| anyhow!("invalid impact severity for {quality}"))?
                        .to_owned(),
                );
            }
        }
        _ => bail!("invalid impacts: expected an object"),
    }
    let range = normalize_range(raw.get("textRange"))?;
    let mut flows = Vec::new();
    match raw.get("flows") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) => {
            for flow in values {
                let raw_locations = flow
                    .get("locations")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("invalid flow locations"))?;
                let mut locations = Vec::new();
                for location in raw_locations {
                    if !location.is_object() {
                        bail!("invalid flow location");
                    }
                    let path = optional_string(location, "fileUri")?
                        .map(|uri| relative_path(root, uri))
                        .transpose()?;
                    locations.push(FlowLocation {
                        path,
                        message: optional_string(location, "message")?.map(str::to_owned),
                        range: normalize_range(location.get("textRange"))?,
                    });
                }
                flows.push(Flow { locations });
            }
        }
        _ => bail!("invalid flows: expected an array"),
    }
    Ok(Finding {
        path,
        rule_key,
        message,
        severity,
        issue_type,
        impacts,
        range,
        flows,
        details: raw,
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    optional_string(value, field)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing or empty {field}"))
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => bail!("invalid {field}: expected a string"),
    }
}

fn normalize_range(raw: Option<&Value>) -> Result<Option<TextRange>> {
    let raw = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let number = |field: &str| -> Result<u32> {
        let n = raw
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("invalid textRange.{field}"))?;
        u32::try_from(n).with_context(|| format!("textRange.{field} is too large"))
    };
    let range = TextRange {
        start_line: number("startLine")?,
        start_column: number("startLineOffset")?
            .checked_add(1)
            .context("start column overflow")?,
        end_line: number("endLine")?,
        end_column: number("endLineOffset")?
            .checked_add(1)
            .context("end column overflow")?,
    };
    if range.start_line == 0
        || range.end_line == 0
        || (range.end_line, range.end_column) < (range.start_line, range.start_column)
    {
        bail!("invalid textRange: lines must be positive and end must follow start");
    }
    Ok(Some(range))
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn relative_path(root: &Path, uri: &str) -> Result<String> {
    let parsed = url::Url::parse(uri).with_context(|| format!("invalid file URI {uri}"))?;
    let path = parsed
        .to_file_path()
        .map_err(|_| anyhow!("expected a local file URI: {uri}"))?;
    let path = absolute_lexical(&path)?;
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "finding path {} is outside project {}",
            path.display(),
            root.display()
        )
    })?;
    let text = relative.to_str().context("finding path is not UTF-8")?;
    Ok(if text.is_empty() {
        ".".into()
    } else {
        text.replace(std::path::MAIN_SEPARATOR, "/")
    })
}

pub fn render(report: &ScanReport, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Json => Ok(format!("{}\n", serde_json::to_string_pretty(report)?)),
        OutputFormat::Sarif => Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&sarif(report))?
        )),
        OutputFormat::Text => {
            let mut out = String::new();
            for finding in &report.findings {
                let location = match finding.range {
                    Some(range) => format!(
                        "{}:{}:{}",
                        finding.path, range.start_line, range.start_column
                    ),
                    None => finding.path.clone(),
                };
                // Findings remain one per line, even for multiline engine messages.
                let message = finding.message.replace(['\r', '\n'], " ");
                out.push_str(&format!(
                    "{location}: {} [{}] {message}\n",
                    finding.severity, finding.rule_key
                ));
            }
            out.push_str(&format!(
                "{} finding(s) in {} scanned file(s) ({} production, {} test)\n",
                report.findings.len(),
                report.scanned_files.len(),
                report.scanned_files.len() - report.test_files.len(),
                report.test_files.len()
            ));
            for path in &report.failed_files {
                out.push_str(&format!("Analysis failed: {path}\n"));
            }
            for (path, reason) in &report.skipped_files {
                out.push_str(&format!("Skipped: {path}: {reason}\n"));
            }
            Ok(out)
        }
    }
}

fn sarif(report: &ScanReport) -> Value {
    let rules: Vec<&str> = report
        .findings
        .iter()
        .map(|f| f.rule_key.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let indices: BTreeMap<&str, usize> = rules
        .iter()
        .copied()
        .enumerate()
        .map(|(i, key)| (key, i))
        .collect();
    let results: Vec<Value> = report.findings.iter().map(|finding| {
        let mut result = json!({
            "ruleId": finding.rule_key,
            "ruleIndex": indices[finding.rule_key.as_str()],
            "level": sarif_level(finding),
            "message": {"text": finding.message},
            "properties": {"severity": finding.severity, "type": finding.issue_type, "impacts": finding.impacts},
        });
        if finding.path != "." {
            result["locations"] = json!([sarif_location(&finding.path, finding.range, None)]);
        }
        let flows: Vec<Value> = finding.flows.iter().filter(|flow| !flow.locations.is_empty()).map(|flow| {
            let locations: Vec<Value> = flow.locations.iter().enumerate().map(|(index, location)| {
                let location = match &location.path {
                    Some(path) => sarif_location(path, location.range, location.message.as_deref()),
                    None => json!({"message": {"text": location.message.as_deref().filter(|s| !s.is_empty()).unwrap_or("Secondary location")}}),
                };
                json!({"location": location, "executionOrder": index + 1})
            }).collect();
            json!({"threadFlows": [{"locations": locations}]})
        }).collect();
        if !flows.is_empty() {
            result["codeFlows"] = json!(flows);
        }
        result
    }).collect();
    let mut notifications: Vec<Value> = report.failed_files.iter().map(|path| {
        json!({"level": "error", "message": {"text": format!("Analysis failed: {path}")}})
    }).collect();
    notifications.extend(report.skipped_files.iter().map(|(path, reason)| {
        json!({"level": "warning", "message": {"text": format!("Skipped: {path}: {reason}")}})
    }));
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {"name": "rustmarine", "version": env!("CARGO_PKG_VERSION"),
                "rules": rules.iter().map(|rule| json!({"id": rule})).collect::<Vec<_>>() }},
            "columnKind": "utf16CodeUnits",
            "properties": {"testFiles": report.test_files},
            "originalUriBaseIds": {"PROJECTROOT": {"uri": root_uri(&report.root)}},
            "results": results,
            "invocations": [{"executionSuccessful": report.failed_files.is_empty() && !(report.scanned_files.is_empty() && !report.skipped_files.is_empty()), "toolExecutionNotifications": notifications}],
        }],
    })
}

fn root_uri(root: &str) -> String {
    url::Url::from_directory_path(root)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| root.to_owned())
}

fn sarif_location(path: &str, range: Option<TextRange>, message: Option<&str>) -> Value {
    // Encode each path segment without interpreting '#', '%' or platform drive letters.
    let mut encoded = url::Url::parse("https://paths.invalid/").expect("constant valid URL");
    encoded
        .path_segments_mut()
        .expect("hierarchical URL")
        .extend(path.split('/'));
    let uri = encoded.path().trim_start_matches('/');
    let mut location =
        json!({"physicalLocation": {"artifactLocation": {"uri": uri, "uriBaseId": "PROJECTROOT"}}});
    if let Some(r) = range {
        location["physicalLocation"]["region"] = json!({"startLine": r.start_line, "startColumn": r.start_column, "endLine": r.end_line, "endColumn": r.end_column});
    }
    if let Some(message) = message {
        location["message"] = json!({"text": message});
    }
    location
}

fn sarif_level(finding: &Finding) -> &'static str {
    match finding.severity.as_str() {
        "BLOCKER" | "CRITICAL" | "HIGH" => "error",
        "MAJOR" | "MEDIUM" => "warning",
        "MINOR" | "INFO" | "LOW" => "note",
        _ => {
            if finding
                .impacts
                .values()
                .any(|s| s == "HIGH" || s == "BLOCKER")
            {
                "error"
            } else if finding.impacts.values().any(|s| s == "MEDIUM") {
                "warning"
            } else {
                "note"
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn issue() -> Value {
        // RawIssueDto from analysis/analyzeFilesAndTrack, matching the plugin's TextRangeDto.
        json!({"ruleKey": "python:S1481", "primaryMessage": "Remove this unused local variable.",
            "fileUri": "file:///project/src/foo.py", "severity": "MINOR", "type": "CODE_SMELL",
            "textRange": {"startLine": 2, "startLineOffset": 4, "endLine": 2, "endLineOffset": 7},
            "impacts": {"MAINTAINABILITY": "LOW"}, "flows": [], "quickFixes": []})
    }

    fn report(findings: Vec<Finding>) -> ScanReport {
        ScanReport {
            schema_version: 1,
            root: "/project".into(),
            scanned_files: vec!["src/foo.py".into()],
            test_files: vec![],
            findings,
            failed_files: vec![],
            skipped_files: BTreeMap::new(),
        }
    }

    #[test]
    fn normalizes_engine_coordinates_and_preserves_details() {
        let raw = issue();
        let findings = normalize(Path::new("/project"), vec![raw.clone()]).unwrap();
        let f = &findings[0];
        assert_eq!(f.path, "src/foo.py");
        assert_eq!(
            f.range,
            Some(TextRange {
                start_line: 2,
                start_column: 5,
                end_line: 2,
                end_column: 8
            })
        );
        assert_eq!(f.impacts["MAINTAINABILITY"], "LOW");
        assert_eq!(f.details, raw);
        assert!(render(&report(findings), OutputFormat::Text)
            .unwrap()
            .starts_with("src/foo.py:2:5: MINOR [python:S1481]"));
    }

    #[test]
    fn exports_file_classification_in_json_text_and_sarif() {
        let mut report = report(vec![]);
        report.test_files = report.scanned_files.clone();
        let json: Value =
            serde_json::from_str(&render(&report, OutputFormat::Json).unwrap()).unwrap();
        assert_eq!(json["test_files"], json!(["src/foo.py"]));
        let sarif: Value =
            serde_json::from_str(&render(&report, OutputFormat::Sarif).unwrap()).unwrap();
        assert_eq!(
            sarif["runs"][0]["properties"]["testFiles"],
            json!(["src/foo.py"])
        );
        assert!(render(&report, OutputFormat::Text)
            .unwrap()
            .contains("(0 production, 1 test)"));
    }

    #[test]
    fn errors_on_malformed_findings_instead_of_reporting_clean_scan() {
        let mut missing = issue();
        missing.as_object_mut().unwrap().remove("ruleKey");
        assert!(normalize(Path::new("/project"), vec![missing]).is_err());
        let mut bad_range = issue();
        bad_range["textRange"]["startLine"] = json!(0);
        assert!(normalize(Path::new("/project"), vec![bad_range]).is_err());
        let mut outside = issue();
        outside["fileUri"] = json!("file:///project/../other/foo.py");
        assert!(normalize(Path::new("/project"), vec![outside]).is_err());
    }

    #[test]
    fn sorts_findings_and_decodes_file_uris() {
        let mut earlier = issue();
        earlier["fileUri"] = json!("file:///project/a%20b.py");
        let findings = normalize(Path::new("/project"), vec![issue(), earlier]).unwrap();
        assert_eq!(findings[0].path, "a b.py");
        let result: Value =
            serde_json::from_str(&render(&report(findings), OutputFormat::Sarif).unwrap()).unwrap();
        assert_eq!(
            result["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "a%20b.py"
        );
    }

    #[test]
    fn exports_flows_and_reports_incomplete_analysis_in_sarif() {
        let mut raw = issue();
        raw["flows"] = json!([{"locations": [{"fileUri": "file:///project/src/source.py", "message": "Value originates here", "textRange": {"startLine": 1, "startLineOffset": 0, "endLine": 1, "endLineOffset": 3}}]}]);
        let mut report = report(normalize(Path::new("/project"), vec![raw]).unwrap());
        report.failed_files.push("broken.py".into());
        report
            .skipped_files
            .insert("unknown.lang".into(), "LANGUAGE_NOT_SUPPORTED".into());
        let sarif: Value =
            serde_json::from_str(&render(&report, OutputFormat::Sarif).unwrap()).unwrap();
        let run = &sarif["runs"][0];
        assert_eq!(run["columnKind"], "utf16CodeUnits");
        assert_eq!(run["invocations"][0]["executionSuccessful"], false);
        assert_eq!(
            run["invocations"][0]["toolExecutionNotifications"][1]["level"],
            "warning"
        );
        assert_eq!(
            run["results"][0]["codeFlows"][0]["threadFlows"][0]["locations"][0]["location"]
                ["physicalLocation"]["region"]["startColumn"],
            1
        );
    }

    #[test]
    fn handles_project_level_and_file_level_findings() {
        let mut raw = issue();
        raw["fileUri"] = Value::Null;
        raw["textRange"] = Value::Null;
        let findings = normalize(Path::new("/project"), vec![raw]).unwrap();
        assert_eq!(findings[0].path, ".");
        let json: Value =
            serde_json::from_str(&render(&report(findings), OutputFormat::Json).unwrap()).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert!(json["findings"][0]["range"].is_null());
    }
}
