//! Providers command: list installed session-log providers with roots,
//! availability, session counts, format families, and diagnostics.

use std::collections::BTreeMap;

use crate::cli::{Cli, OutputFormat};
use crate::error::Result;
use crate::provider::ArtifactForm;

/// Run the providers command.
pub fn run(cli: &Cli) -> Result<()> {
    let registry = super::helpers::provider_registry(cli);

    let mut reports = Vec::new();
    for entry in registry.entries() {
        let mut report = serde_json::json!({
            "provider": entry.id.to_string(),
            "root": entry.root.as_ref().map(|p| p.display().to_string()),
            "available": entry.provider.is_ok(),
        });
        match &entry.provider {
            Err(reason) => {
                report["diagnostic"] = serde_json::json!(reason);
            }
            Ok(provider) => match provider.sessions() {
                Err(e) => {
                    report["diagnostic"] = serde_json::json!(format!("session scan failed: {e}"));
                }
                Ok(descriptors) => {
                    let mut forms: BTreeMap<&str, usize> = BTreeMap::new();
                    let mut archived = 0usize;
                    for d in &descriptors {
                        for a in &d.artifacts {
                            let tag = match &a.form {
                                ArtifactForm::PlainFile => "plain",
                                ArtifactForm::CompressedFile => "compressed",
                                ArtifactForm::Database => "database",
                                ArtifactForm::Other(_) => "other",
                            };
                            *forms.entry(tag).or_default() += 1;
                            if a.archived {
                                archived += 1;
                            }
                        }
                    }
                    report["sessions"] = serde_json::json!(descriptors.len());
                    report["artifact_formats"] = serde_json::json!(forms);
                    report["archived_artifacts"] = serde_json::json!(archived);
                }
            },
        }
        reports.push(report);
    }

    if cli.effective_output() == OutputFormat::Json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }

    for report in &reports {
        println!("{}", report["provider"].as_str().unwrap_or("?"));
        if let Some(root) = report["root"].as_str() {
            println!("  root: {root}");
        }
        if let Some(diag) = report["diagnostic"].as_str() {
            println!("  status: unavailable — {diag}");
        } else {
            println!("  status: available");
            println!("  sessions: {}", report["sessions"]);
            let forms = report["artifact_formats"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| format!("{k} ×{v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if !forms.is_empty() {
                println!("  artifact formats: {forms}");
            }
            if report["archived_artifacts"].as_u64().unwrap_or(0) > 0 {
                println!("  archived artifacts: {}", report["archived_artifacts"]);
            }
        }
    }
    Ok(())
}
