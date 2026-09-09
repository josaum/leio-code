//! Human-readable view of the same bounded packet returned by `--json`.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::model::QueryEnvelope;

fn escape_controls(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

fn source_label(source: &Value) -> String {
    let path = escape_controls(source["path"].as_str().unwrap_or_default());
    match source["member"].as_str() {
        Some(member) => format!("{path} -> {}", escape_controls(member)),
        None => path,
    }
}

fn render_section(
    output: &mut String,
    title: &str,
    rows: &[&Value],
    source_labels: &BTreeMap<&str, String>,
) -> Result<()> {
    writeln!(output, "\n{title} ({})", rows.len())?;
    if rows.is_empty() {
        writeln!(output, "  None.")?;
        return Ok(());
    }
    let mut rows = rows.to_vec();
    rows.sort_by_key(|row| {
        (
            row["timestamp"].as_str().unwrap_or_default(),
            row["source_ordinal"].as_u64().unwrap_or_default(),
        )
    });
    for row in rows {
        writeln!(
            output,
            "  {} | {} | {}",
            escape_controls(row["timestamp"].as_str().unwrap_or("unknown time")),
            escape_controls(row["account"].as_str().unwrap_or("system")),
            escape_controls(row["kind"].as_str().unwrap_or("unknown kind")),
        )?;
        writeln!(
            output,
            "  ID: {}",
            escape_controls(row["id"].as_str().unwrap_or_default())
        )?;
        let digest = row["source_sha256"]
            .as_str()
            .context("conversation record source digest missing")?;
        let source = source_labels
            .get(digest)
            .context("conversation record references a missing source")?;
        let locator = row["locator"]
            .as_str()
            .context("conversation record locator missing")?;
        writeln!(output, "  Source: {source} | {}", escape_controls(locator))?;
        for line in row["text"].as_str().unwrap_or_default().split('\n') {
            writeln!(output, "    > {}", escape_controls(line))?;
        }
        if row["text_truncated"] == true {
            writeln!(
                output,
                "    [Excerpt truncated; full text remains in the source file.]"
            )?;
        }
    }
    Ok(())
}

/// Render a prepared conversation packet without modifying its evidence or writing events.
pub fn render_text(envelope: &QueryEnvelope) -> Result<String> {
    let packet = envelope
        .entities
        .first()
        .context("conversation packet missing")?;
    let sources = packet["sources"]
        .as_array()
        .context("conversation sources missing")?;
    let records = packet["evidence_records"]
        .as_object()
        .context("conversation records missing")?;
    let accounts = packet["inventory"]["accounts"]
        .as_object()
        .context("conversation accounts missing")?;
    let mut output = String::new();
    writeln!(output, "Conversation review")?;
    writeln!(
        output,
        "Messages: {} | Accounts: {} | Sources: {}",
        packet["inventory"]["messages"],
        accounts.len(),
        sources.len()
    )?;
    writeln!(
        output,
        "Times are export-local; timezone is not established."
    )?;
    writeln!(
        output,
        "Selection: {}",
        escape_controls(packet["selection"]["reason"].as_str().unwrap_or_default())
    )?;
    writeln!(
        output,
        "Context limit per section: {}; counts show selected records.",
        packet["context_limit_per_section"]
    )?;
    writeln!(output, "\nSources")?;
    let mut source_labels = BTreeMap::new();
    for source in sources {
        let label = source_label(source);
        let digest = source["content_sha256"]
            .as_str()
            .context("conversation source content digest missing")?;
        // Duplicate containers retain their provenance in the source list; records refer
        // to the first selected container whose transcript was parsed.
        source_labels.entry(digest).or_insert_with(|| label.clone());
        write!(output, "  {label}")?;
        if source["duplicate_content_skipped"] == true {
            write!(output, " (duplicate transcript skipped)")?;
        }
        writeln!(output)?;
    }
    for (key, title) in [
        ("target", "Target"),
        ("antecedents", "Antecedents (strictly prior)"),
        (
            "prior_account_history",
            "Prior account history (other sessions)",
        ),
        (
            "later_retrospective_only",
            "Later context (retrospective only)",
        ),
        ("same_timestamp_unordered", "Same timestamp (unordered)"),
    ] {
        let ids = packet["contexts"][key]
            .as_array()
            .with_context(|| format!("conversation context {key} missing"))?;
        let rows = ids
            .iter()
            .map(|id| {
                records
                    .get(id.as_str().context("conversation record ID must be text")?)
                    .context("conversation context references a missing record")
            })
            .collect::<Result<Vec<_>>>()?;
        render_section(&mut output, title, &rows, &source_labels)?;
    }
    let conflicts = packet["source_order_conflicts_retrospective_only"]
        .as_object()
        .context("conversation source-order conflicts missing")?;
    render_section(
        &mut output,
        "Source-order conflicts (retrospective only)",
        &conflicts.values().collect::<Vec<_>>(),
        &source_labels,
    )?;
    if !envelope.warnings.is_empty() {
        writeln!(output, "\nWarnings")?;
        for warning in &envelope.warnings {
            writeln!(output, "  - {}", escape_controls(warning))?;
        }
    }
    writeln!(
        output,
        "\nAnalysis remains pending; authorship is not established."
    )?;
    writeln!(
        output,
        "Use --json for the complete bounded evidence packet, hashes and method requirements."
    )?;
    Ok(output)
}
