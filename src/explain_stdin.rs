//! `leio-code explain --stdin` plumbing.
//!
//! Closes the `find → jq → explain` loop. Users do:
//!
//! ```text
//! leio-code find env-var --format=jsonld \
//!   | jq '.entities[] | select(.is_secret == true)' \
//!   | leio-code explain --stdin
//! ```
//!
//! The handler in `main.rs` calls [`read_entities`] to pull a stream of
//! JSON-LD entities from stdin (whole envelope, JSON array, or a stream of
//! concatenated/JSONL objects — all three are accepted), then [`dispatch`]
//! to map each entity's `@type` to the appropriate existing `explain_*`
//! function in [`crate::query`].
//!
//! Design notes:
//! - This module only knows how to *dispatch*. It does not reimplement any
//!   explain logic. The mapping table is `@type` + a field-name lookup;
//!   no `@id` parser is added.
//! - `serde_json::Deserializer::from_reader(...).into_iter::<Value>()`
//!   handles the three input shapes uniformly:
//!     * single envelope object → 1 value out, expanded via `entities[]`
//!     * JSON array → 1 value out, iterated
//!     * concatenated/pretty-printed/JSONL stream (what
//!       `jq '.entities[]'` actually emits) → N values out
//!       A naive line split would not work for pretty-printed jq output.
//! - Unknown `@type` / missing required field → counted, warned to stderr,
//!   not fatal.

use std::io::Read;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::model::{QueryEnvelope, RepoIndex};
use crate::query::{
    explain_binary, explain_cartridge, explain_deploy_target, explain_env_var, explain_redis_key,
    explain_route,
};
use crate::value_resolution::ValueResolutionOpts;

/// Read a stream of JSON-LD entities from `reader`. Accepts:
/// - A single envelope object with an `entities` array — expanded.
/// - A JSON array of entities — iterated.
/// - A stream of concatenated JSON objects (pretty-printed or JSONL) — each
///   yielded as one entity.
///
/// Returns the entities in stream order. Returns an error only on JSON
/// parse failures or I/O errors. Unknown shapes are not rejected here —
/// the dispatcher decides what to skip.
pub fn read_entities<R: Read>(reader: R) -> Result<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();
    let stream = serde_json::Deserializer::from_reader(reader).into_iter::<Value>();
    let mut first = true;
    for item in stream {
        let value = item.context("failed to parse JSON from stdin")?;
        if first {
            first = false;
            // First value gets special treatment: if it's an envelope (object
            // with an `entities` array), expand. If it's an array, expand.
            // Otherwise treat it as a single entity and fall through.
            if let Value::Object(map) = &value
                && let Some(Value::Array(entities)) = map.get("entities")
            {
                out.extend(entities.iter().cloned());
                continue;
            }
            if let Value::Array(arr) = &value {
                out.extend(arr.iter().cloned());
                continue;
            }
            out.push(value);
        } else {
            // Subsequent values from a stream are always individual entities.
            // (We don't try to unwrap envelopes after the first.)
            out.push(value);
        }
    }
    Ok(out)
}

/// Outcome of dispatching a single entity.
pub enum DispatchOutcome {
    /// Dispatched successfully; carries the resulting envelope.
    Ok(Box<QueryEnvelope>),
    /// `@type` was not one of the recognized kinds. Carries the type label
    /// for the summary at the end.
    UnknownType(String),
    /// Required field (e.g. `name`, `key`, `route`) was missing or not a
    /// string. Carries a human-readable reason.
    MissingField(String),
}

/// Map an entity's `@type` to the appropriate `explain_*` call.
///
/// The field-name table mirrors `docs/output-schema.md` §2:
/// - `EnvVar` → `name`
/// - `Binary` → `name`
/// - `Route` → `route` (NOT `name`; see §2.8)
/// - `DeployTarget` → `name`
/// - `RedisKey` → `key` (with `name` fallback; see §2.3)
/// - `Cartridge` → `name`
///
/// Anything else is reported as `UnknownType`.
pub fn dispatch(
    entity: &Value,
    index: &RepoIndex,
    repo: &std::path::Path,
    opts: ValueResolutionOpts,
) -> DispatchOutcome {
    let entity_type = match entity.get("@type").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return DispatchOutcome::UnknownType("<missing @type>".to_string()),
    };

    match entity_type {
        "EnvVar" => match string_field(entity, &["name"]) {
            Some(name) => DispatchOutcome::Ok(Box::new(explain_env_var(index, &name, opts))),
            None => DispatchOutcome::MissingField(
                "EnvVar entity missing required `name` field".to_string(),
            ),
        },
        "Binary" => match string_field(entity, &["name"]) {
            Some(name) => DispatchOutcome::Ok(Box::new(explain_binary(index, &name))),
            None => DispatchOutcome::MissingField(
                "Binary entity missing required `name` field".to_string(),
            ),
        },
        "Route" => match string_field(entity, &["route"]) {
            Some(route) => DispatchOutcome::Ok(Box::new(explain_route(index, &route))),
            None => DispatchOutcome::MissingField(
                "Route entity missing required `route` field".to_string(),
            ),
        },
        "DeployTarget" => match string_field(entity, &["name"]) {
            Some(name) => {
                DispatchOutcome::Ok(Box::new(explain_deploy_target(index, &name, repo, opts)))
            }
            None => DispatchOutcome::MissingField(
                "DeployTarget entity missing required `name` field".to_string(),
            ),
        },
        "RedisKey" => match string_field(entity, &["key", "name"]) {
            Some(key) => DispatchOutcome::Ok(Box::new(explain_redis_key(index, &key))),
            None => DispatchOutcome::MissingField(
                "RedisKey entity missing required `key` (or `name`) field".to_string(),
            ),
        },
        "Cartridge" => match string_field(entity, &["name"]) {
            Some(name) => DispatchOutcome::Ok(Box::new(explain_cartridge(index, &name))),
            None => DispatchOutcome::MissingField(
                "Cartridge entity missing required `name` field".to_string(),
            ),
        },
        other => DispatchOutcome::UnknownType(other.to_string()),
    }
}

/// Return the first non-empty string-valued field from `entity` whose key is
/// in `candidates`.
fn string_field(entity: &Value, candidates: &[&str]) -> Option<String> {
    for key in candidates {
        if let Some(s) = entity.get(*key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Aggregate skip counts so the handler can print a one-line summary.
#[derive(Debug, Default)]
pub struct SkipSummary {
    pub unknown_type_counts: std::collections::BTreeMap<String, usize>,
    pub missing_field_count: usize,
}

impl SkipSummary {
    pub fn record_unknown(&mut self, ty: &str) {
        *self.unknown_type_counts.entry(ty.to_string()).or_insert(0) += 1;
    }

    pub fn record_missing(&mut self) {
        self.missing_field_count += 1;
    }

    pub fn total(&self) -> usize {
        self.unknown_type_counts.values().sum::<usize>() + self.missing_field_count
    }

    /// Format a one-line summary, or `None` if nothing was skipped.
    pub fn one_line(&self) -> Option<String> {
        if self.total() == 0 {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (ty, count) in &self.unknown_type_counts {
            parts.push(format!("{count} unknown_type({ty})"));
        }
        if self.missing_field_count > 0 {
            parts.push(format!("{} missing_field", self.missing_field_count));
        }
        Some(format!(
            "skipped {} entities: {}",
            self.total(),
            parts.join(", ")
        ))
    }
}

/// Ensure the `--stdin` invocation has no incompatible flags set.
/// Returns `Err` with a user-facing message on conflict.
pub fn validate_stdin_flags(where_expr: Option<&str>) -> Result<()> {
    if where_expr.is_some() {
        return Err(anyhow!(
            "--where is not supported with --stdin; filter upstream with jq before piping"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_whole_envelope() {
        let input = json!({
            "@context": "https://ontology.getjai.com/leio-code/v1#",
            "@type": "FindResult",
            "entities": [
                {"@type": "EnvVar", "name": "FOO"},
                {"@type": "EnvVar", "name": "BAR"},
            ]
        })
        .to_string();
        let entities = read_entities(input.as_bytes()).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0]["name"], "FOO");
        assert_eq!(entities[1]["name"], "BAR");
    }

    #[test]
    fn reads_json_array() {
        let input = json!([
            {"@type": "EnvVar", "name": "A"},
            {"@type": "EnvVar", "name": "B"},
        ])
        .to_string();
        let entities = read_entities(input.as_bytes()).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0]["name"], "A");
    }

    #[test]
    fn reads_jsonl_stream() {
        let input = r#"{"@type":"EnvVar","name":"A"}
{"@type":"EnvVar","name":"B"}
{"@type":"EnvVar","name":"C"}
"#;
        let entities = read_entities(input.as_bytes()).unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(entities[2]["name"], "C");
    }

    #[test]
    fn reads_pretty_concatenated() {
        // This is what `jq '.entities[]'` produces by default: pretty-printed
        // values concatenated with newlines (NOT line-delimited).
        let input = r#"{
  "@type": "EnvVar",
  "name": "A"
}
{
  "@type": "EnvVar",
  "name": "B"
}
"#;
        let entities = read_entities(input.as_bytes()).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0]["name"], "A");
        assert_eq!(entities[1]["name"], "B");
    }

    #[test]
    fn empty_input_is_ok() {
        let entities = read_entities("".as_bytes()).unwrap();
        assert!(entities.is_empty());
    }

    #[test]
    fn skip_summary_formats() {
        let mut s = SkipSummary::default();
        s.record_unknown("HttpCallSite");
        s.record_unknown("HttpCallSite");
        s.record_missing();
        let line = s.one_line().unwrap();
        assert!(line.contains("2 unknown_type(HttpCallSite)"));
        assert!(line.contains("1 missing_field"));
        assert!(line.starts_with("skipped 3 entities:"));
    }

    #[test]
    fn skip_summary_empty_is_none() {
        let s = SkipSummary::default();
        assert!(s.one_line().is_none());
    }

    #[test]
    fn validate_rejects_where() {
        assert!(validate_stdin_flags(Some(".entities[]")).is_err());
        assert!(validate_stdin_flags(None).is_ok());
    }
}
