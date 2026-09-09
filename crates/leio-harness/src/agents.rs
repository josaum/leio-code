//! Agent adapters: declarative argv templates for real agent CLIs. `{task}`
//! and `{model}` placeholders are substituted per lane. No permission-bypass
//! flags are injected — safety flags stay with the operator.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Coordinator work-shape used to pick an OpenRouter permaslug.
///
/// Built-in CLI templates do not consume `{model}`. This table is only used
/// when a lane sets `model` or `workShape` and the template substitutes
/// `{model}`. Token-volume leaders are not treated as quality rankings.
/// The built-in snapshot below comes from OpenRouter
/// (openrouter.ai/rankings), usage through 2026-08-17, CC BY 4.0; it is the
/// offline fallback for [`openrouter_slug`] and the live refresh keeps it
/// current (see [`ModelTable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkShape {
    Explorer,
    Worker,
    Verifier,
    Reviewer,
    Security,
}

/// Flash 0731 leads OpenRouter token adoption; use only on volume lanes.
/// Source: OpenRouter rankings, usage through 2026-08-17. CC BY 4.0.
const SLUG_VOLUME: &str = "deepseek/deepseek-v4-flash-0731";
/// Luna is the cheap verifier analog (latency-sensitive, not review-grade).
const SLUG_VERIFIER: &str = "openai/gpt-5.6-luna";
/// Opus 5 is the quality pick for review and security, not token volume.
const SLUG_QUALITY: &str = "anthropic/claude-opus-5";

impl WorkShape {
    /// Parse a coordinator work-shape or Grok route alias.
    ///
    /// Accepts `explorer` / `terra-low`, `worker` / `terra-medium`,
    /// `verifier` / `luna-medium`, `reviewer` / `sol-high`, and
    /// `security` / `sol-xhigh`. Coordinator (`sol-medium`) is omitted on
    /// purpose so that lane stays on the Grok CLI default.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "explorer" | "terra-low" => Some(Self::Explorer),
            "worker" | "terra-medium" => Some(Self::Worker),
            "verifier" | "luna-medium" => Some(Self::Verifier),
            "reviewer" | "sol-high" => Some(Self::Reviewer),
            "security" | "sol-xhigh" => Some(Self::Security),
            _ => None,
        }
    }
}

/// Live-refreshed OpenRouter work-shape picks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTable {
    /// Unix epoch milliseconds of the fetch.
    pub fetched_at_ms: u64,
    /// Where the ranking came from (`openrouter-api` or `builtin`).
    pub source: String,
    pub volume: String,
    pub verifier: String,
    pub quality: String,
}

/// A refresh is considered stale after one week.
pub const MODEL_TABLE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Work-shape ranks only consider models with at least this context window.
const MIN_CONTEXT_LENGTH: u64 = 100_000;

/// `~/.config/leio-harness/openrouter-models.json` (HOME-dependent).
pub fn models_cache_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
        .map(|home| Path::new(&home).join(".config/leio-harness/openrouter-models.json"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Read the cache table, rejecting missing, stale, or incomplete entries.
pub fn read_model_table_at(path: &Path, now_ms: u64) -> Option<ModelTable> {
    let raw = std::fs::read(path).ok()?;
    let table: ModelTable = serde_json::from_slice(&raw).ok()?;
    if now_ms.saturating_sub(table.fetched_at_ms) > MODEL_TABLE_TTL_MS {
        return None;
    }
    if table.volume.is_empty() || table.verifier.is_empty() || table.quality.is_empty() {
        return None;
    }
    Some(table)
}

/// Persist the table to `path` (parent directories created).
///
/// # Errors
///
/// Returns an error when the parent directory cannot be created or the file
/// cannot be written.
pub fn write_model_table_at(path: &Path, table: &ModelTable) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// The model table a production lane should resolve against: the refreshed
/// cache when fresh, otherwise `None` (built-in snapshot).
pub fn cached_table() -> Option<ModelTable> {
    let path = models_cache_path()?;
    read_model_table_at(&path, now_ms())
}

/// Return the OpenRouter permaslug for a work-shape.
///
/// `table` (when present) is the live-refreshed pick set; `None` falls back
/// to the built-in ranking snapshot. Explorer and worker map to the volume
/// pick, verifier to the latency-tier pick, reviewer and security to the
/// quality pick.
pub fn openrouter_slug(shape: WorkShape, table: Option<&ModelTable>) -> String {
    if let Some(table) = table {
        match shape {
            WorkShape::Explorer | WorkShape::Worker => table.volume.clone(),
            WorkShape::Verifier => table.verifier.clone(),
            WorkShape::Reviewer | WorkShape::Security => table.quality.clone(),
        }
    } else {
        builtin_slug(shape).to_owned()
    }
}

/// Built-in ranking snapshot (OpenRouter adoption through 2026-08-17).
pub fn builtin_slug(shape: WorkShape) -> &'static str {
    match shape {
        WorkShape::Explorer | WorkShape::Worker => SLUG_VOLUME,
        WorkShape::Verifier => SLUG_VERIFIER,
        WorkShape::Reviewer | WorkShape::Security => SLUG_QUALITY,
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<OpenRouterModel>,
}

#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    pricing: OpenRouterPricing,
}

#[derive(Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Deserialize, Default)]
struct OpenRouterPricing {
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    completion: String,
}

/// Rank an OpenRouter `/models` response body into work-shape picks.
///
/// Heuristics (documented, deterministic): `:free` variants are skipped,
/// models under [`MIN_CONTEXT_LENGTH`] context are skipped, and each
/// remaining model is scored by combined USD-per-million (prompt +
/// completion). Volume takes the cheapest, quality the most expensive, and
/// verifier the candidate closest to the median price excluding both, with
/// ties breaking lexicographically by id.
///
/// # Errors
///
/// Fails on malformed JSON or when fewer than three candidates survive the
/// filters — callers keep the built-in snapshot in that case.
pub fn rank_models(raw_json: &str, now_ms: u64) -> Result<ModelTable> {
    let response: ModelsResponse = serde_json::from_str(raw_json)
        .with_context(|| format!("parse models response ({} bytes)", raw_json.len()))?;
    let mut candidates: Vec<(String, f64)> = Vec::new();
    for model in response.data {
        // Agent lanes consume text completions, so audio/image-side outputs
        // (music previews charge zero prompt tokens) are disqualified. Models
        // without modality metadata predate multi-modal listing: text-only.
        let output_modalities = model
            .architecture
            .map(|a| a.output_modalities)
            .unwrap_or_default();
        if !output_modalities.is_empty() && output_modalities != ["text"] {
            continue;
        }
        // Preview ids are transient; `:free` variants are rate-limited
        // loss leaders. Neither is a dependable lane workhorse.
        if model.id.contains("preview") || model.id.ends_with(":free") {
            continue;
        }
        if model.context_length.unwrap_or(0) < MIN_CONTEXT_LENGTH {
            continue;
        }
        let prompt: f64 = model.pricing.prompt.trim().parse().unwrap_or(-1.0);
        let completion: f64 = model.pricing.completion.trim().parse().unwrap_or(-1.0);
        if !prompt.is_finite() || !completion.is_finite() || prompt < 0.0 || completion < 0.0 {
            continue;
        }
        // A zero combined price is a promotion, not a lane workhorse.
        let combined = (prompt + completion) * 1_000_000.0;
        if combined <= 0.0 {
            continue;
        }
        candidates.push((model.id, combined));
    }
    candidates.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    if candidates.len() < 3 {
        bail!(
            "only {} rankable models after filters; keeping builtin table",
            candidates.len()
        );
    }
    let volume = candidates[0].0.clone();
    let quality = candidates[candidates.len() - 1].0.clone();
    let mid_price = candidates[candidates.len() / 2].1;
    let verifier = candidates
        .iter()
        .skip(1)
        .take(candidates.len().saturating_sub(2))
        .min_by_key(|(id, price)| ((*price - mid_price).abs().to_bits(), id.clone()))
        .map(|(id, _)| id.clone())
        .context("no verifier candidate")?;
    Ok(ModelTable {
        fetched_at_ms: now_ms,
        source: "openrouter-api".to_owned(),
        volume,
        verifier,
        quality,
    })
}

/// Fetch OpenRouter's public models list and persist a fresh table to
/// `cache_path`.
///
/// The endpoint is public — no API key — and this never blocks lane
/// execution: `resolve_model` only consults the cache file this writes.
///
/// # Errors
///
/// Network, HTTP, ranking, and cache-write failures all surface to the
/// caller.
pub async fn refresh_models_table(cache_path: &Path, base_url: Option<&str>) -> Result<ModelTable> {
    let url = format!(
        "{}/models",
        base_url
            .unwrap_or("https://openrouter.ai/api/v1")
            .trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("leio-harness")
        .build()
        .context("build HTTP client")?;
    let raw = client
        .get(url)
        .send()
        .await
        .context("fetch OpenRouter models")?
        .error_for_status()
        .context("OpenRouter models endpoint returned an error status")?
        .text()
        .await
        .context("read OpenRouter models body")?;
    let table = rank_models(&raw, now_ms())?;
    write_model_table_at(cache_path, &table)?;
    Ok(table)
}

/// Resolve the model string a lane should substitute into `{model}`.
///
/// An explicit `model` wins. Otherwise `work_shape` is looked up in `table`
/// (the live-refreshed cache) or the built-in snapshot. Missing both returns
/// `Ok(None)` so built-in CLIs keep their own defaults.
///
/// # Errors
///
/// Returns an error when `work_shape` is set but is not a known shape
/// (including coordinator aliases such as `sol-medium`).
pub fn resolve_model(
    explicit: Option<&str>,
    work_shape: Option<&str>,
    table: Option<&ModelTable>,
) -> Result<Option<String>> {
    if let Some(model) = explicit {
        let trimmed = model.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_owned()));
        }
    }
    match work_shape {
        None => Ok(None),
        Some(name) => {
            let shape =
                WorkShape::parse(name).with_context(|| format!("unknown work_shape: {name}"))?;
            Ok(Some(openrouter_slug(shape, table)))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Built-in templates for the agents in the fabric. `codex exec`, `claude -p`,
/// `gemini -p` are non-interactive one-shot invocations suitable for lanes.
pub fn builtin_templates() -> BTreeMap<String, AgentTemplate> {
    let mut map = BTreeMap::new();
    map.insert(
        "codex".to_owned(),
        AgentTemplate {
            argv: vec!["codex".into(), "exec".into(), "{task}".into()],
            env: BTreeMap::new(),
        },
    );
    map.insert(
        "claude".to_owned(),
        AgentTemplate {
            argv: vec!["claude".into(), "-p".into(), "{task}".into()],
            env: BTreeMap::new(),
        },
    );
    map.insert(
        "gemini".to_owned(),
        AgentTemplate {
            argv: vec!["gemini".into(), "-p".into(), "{task}".into()],
            env: BTreeMap::new(),
        },
    );
    map.insert(
        "kimi".to_owned(),
        AgentTemplate {
            argv: vec!["kimi".into(), "-p".into(), "{task}".into()],
            env: BTreeMap::new(),
        },
    );
    map.insert(
        "grok".to_owned(),
        AgentTemplate {
            argv: vec!["grok".into(), "-p".into(), "{task}".into()],
            env: BTreeMap::new(),
        },
    );
    map
}

/// Expand a template for a lane: `{task}` → task text, `{model}` → model name
/// (error if the template needs a model the lane doesn't provide).
pub fn expand_template(
    template: &AgentTemplate,
    task: &str,
    model: Option<&str>,
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    let mut argv = Vec::with_capacity(template.argv.len());
    for part in &template.argv {
        let expanded = part.replace("{task}", task);
        let expanded = if expanded.contains("{model}") {
            let model = model.context("template contains {model} but lane has no model")?;
            expanded.replace("{model}", model)
        } else {
            expanded
        };
        argv.push(expanded);
    }
    if argv.is_empty() {
        bail!("agent template expanded to empty argv");
    }
    // Env values expand the same placeholders ({task}/{model}) so templates
    // can pass lane context to the agent process through the environment.
    let env: BTreeMap<String, String> = template
        .env
        .iter()
        .map(|(key, value)| {
            let expanded = value
                .replace("{task}", task)
                .replace("{model}", model.unwrap_or(""));
            (key.clone(), expanded)
        })
        .collect();
    Ok((argv, env))
}

/// Resolve a lane argv: explicit `argv` wins; otherwise expand the named
/// builtin/custom template.
pub fn resolve_lane_invocation(
    explicit_argv: Option<&[String]>,
    agent: Option<&str>,
    task: &str,
    model: Option<&str>,
    custom: &BTreeMap<String, AgentTemplate>,
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    if let Some(argv) = explicit_argv
        && !argv.is_empty()
    {
        let template = AgentTemplate {
            argv: argv.to_vec(),
            env: BTreeMap::new(),
        };
        return expand_template(&template, task, model);
    }
    let agent = agent.context("lane requires either argv or agent")?;
    let builtins = builtin_templates();
    let template = custom
        .get(agent)
        .or_else(|| builtins.get(agent))
        .with_context(|| format!("unknown agent template: {agent}"))?;
    expand_template(template, task, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_templates_are_non_interactive_one_shot() {
        let t = builtin_templates();
        assert_eq!(t["codex"].argv, vec!["codex", "exec", "{task}"]);
        assert_eq!(t["claude"].argv, vec!["claude", "-p", "{task}"]);
        assert_eq!(t["gemini"].argv, vec!["gemini", "-p", "{task}"]);
        assert_eq!(t["kimi"].argv, vec!["kimi", "-p", "{task}"]);
        assert_eq!(t["grok"].argv, vec!["grok", "-p", "{task}"]);
    }

    #[test]
    fn expand_substitutes_task_and_model() {
        let template = AgentTemplate {
            argv: vec![
                "tool".into(),
                "--task".into(),
                "{task}".into(),
                "--model".into(),
                "{model}".into(),
            ],
            env: BTreeMap::new(),
        };
        let (argv, _) = expand_template(&template, "hello", Some("gpt-4")).unwrap();
        assert_eq!(argv, vec!["tool", "--task", "hello", "--model", "gpt-4"]);
    }

    #[test]
    fn missing_model_is_an_error() {
        let template = AgentTemplate {
            argv: vec!["{model}".into()],
            env: BTreeMap::new(),
        };
        assert!(expand_template(&template, "t", None).is_err());
    }

    #[test]
    fn explicit_argv_wins_and_unknown_agent_errors() {
        let (argv, _) = resolve_lane_invocation(
            Some(&["/bin/echo".into(), "x".into()]),
            None,
            "t",
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(argv, vec!["/bin/echo", "x"]);
        assert!(resolve_lane_invocation(None, Some("nope"), "t", None, &BTreeMap::new()).is_err());
    }

    #[test]
    fn custom_template_overrides_builtin() {
        let mut custom = BTreeMap::new();
        custom.insert(
            "codex".to_owned(),
            AgentTemplate {
                argv: vec!["custom".into(), "{task}".into()],
                env: BTreeMap::new(),
            },
        );
        let (argv, _) =
            resolve_lane_invocation(None, Some("codex"), "task", None, &custom).unwrap();
        assert_eq!(argv, vec!["custom", "task"]);
    }

    #[test]
    fn builtins_still_omit_model() {
        for template in builtin_templates().values() {
            assert!(
                template.argv.iter().all(|part| !part.contains("{model}")),
                "builtin templates must not consume {{model}}: {:?}",
                template.argv
            );
        }
    }

    #[test]
    fn work_shape_aliases_map_to_openrouter_slugs() {
        assert_eq!(
            resolve_model(None, Some("terra-low"), None)
                .unwrap()
                .as_deref(),
            Some("deepseek/deepseek-v4-flash-0731")
        );
        assert_eq!(
            resolve_model(None, Some("worker"), None)
                .unwrap()
                .as_deref(),
            Some("deepseek/deepseek-v4-flash-0731")
        );
        assert_eq!(
            resolve_model(None, Some("luna-medium"), None)
                .unwrap()
                .as_deref(),
            Some("openai/gpt-5.6-luna")
        );
        assert_eq!(
            resolve_model(None, Some("sol-high"), None)
                .unwrap()
                .as_deref(),
            Some("anthropic/claude-opus-5")
        );
        assert_eq!(
            resolve_model(None, Some("security"), None)
                .unwrap()
                .as_deref(),
            Some("anthropic/claude-opus-5")
        );
    }

    #[test]
    fn coordinator_and_unknown_work_shapes_are_errors() {
        assert!(resolve_model(None, Some("sol-medium"), None).is_err());
        assert!(resolve_model(None, Some("coordinator"), None).is_err());
        assert!(resolve_model(None, Some("not-a-shape"), None).is_err());
    }

    #[test]
    fn explicit_model_wins_over_work_shape() {
        let model = resolve_model(Some("z-ai/glm-5.2"), Some("explorer"), None).unwrap();
        assert_eq!(model.as_deref(), Some("z-ai/glm-5.2"));
    }

    #[test]
    fn work_shape_fills_model_for_opt_in_template() {
        let mut custom = BTreeMap::new();
        custom.insert(
            "codex-model".to_owned(),
            AgentTemplate {
                argv: vec![
                    "codex".into(),
                    "exec".into(),
                    "--model".into(),
                    "{model}".into(),
                    "{task}".into(),
                ],
                env: BTreeMap::new(),
            },
        );
        let model = resolve_model(None, Some("explorer"), None).unwrap();
        let (argv, _) = resolve_lane_invocation(
            None,
            Some("codex-model"),
            "map callers",
            model.as_deref(),
            &custom,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "codex",
                "exec",
                "--model",
                "deepseek/deepseek-v4-flash-0731",
                "map callers"
            ]
        );
    }

    #[test]
    fn rank_models_picks_volume_verifier_quality() {
        let fixture = json!({
            "data": [
                {"id": "a/expensive", "context_length": 200000, "pricing": {"prompt": "0.00003", "completion": "0.00006"}},
                {"id": "b/mid", "context_length": 150000, "pricing": {"prompt": "0.000003", "completion": "0.000006"}},
                {"id": "c/cheap", "context_length": 120000, "pricing": {"prompt": "0.0000002", "completion": "0.0000004"}},
                {"id": "d/tiny-ctx", "context_length": 8000, "pricing": {"prompt": "0", "completion": "0"}},
                {"id": "e/model:free", "context_length": 200000, "pricing": {"prompt": "0", "completion": "0"}},
                {"id": "f/music-preview", "architecture": {"output_modalities": ["text", "audio"]}, "context_length": 1000000, "pricing": {"prompt": "0", "completion": "0"}},
                {"id": "g/zero-price", "architecture": {"output_modalities": ["text"]}, "context_length": 1000000, "pricing": {"prompt": "0", "completion": "0"}}
            ]
        });
        let table = rank_models(&fixture.to_string(), 1_000).expect("rank");
        assert_eq!(table.volume, "c/cheap");
        assert_eq!(table.quality, "a/expensive");
        assert_eq!(table.verifier, "b/mid");
        assert_eq!(table.source, "openrouter-api");
        assert_eq!(table.fetched_at_ms, 1_000);
    }

    #[test]
    fn rank_models_requires_three_candidates() {
        let fixture = json!({"data": [
            {"id": "a/x", "context_length": 200000, "pricing": {"prompt": "0.001", "completion": "0.002"}},
            {"id": "b/y", "context_length": 200000, "pricing": {"prompt": "0.002", "completion": "0.003"}}
        ]});
        assert!(rank_models(&fixture.to_string(), 1_000).is_err());
    }

    #[test]
    fn model_table_cache_respects_ttl_and_completeness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        let table = ModelTable {
            fetched_at_ms: 1_000,
            source: "openrouter-api".into(),
            volume: "a/v".into(),
            verifier: "a/m".into(),
            quality: "a/q".into(),
        };
        write_model_table_at(&path, &table).unwrap();
        assert!(read_model_table_at(&path, 1_000 + MODEL_TABLE_TTL_MS - 1).is_some());
        assert!(read_model_table_at(&path, 1_000 + MODEL_TABLE_TTL_MS + 1).is_none());
        let empty = ModelTable {
            fetched_at_ms: 1_000,
            source: "openrouter-api".into(),
            volume: String::new(),
            verifier: "a/m".into(),
            quality: "a/q".into(),
        };
        write_model_table_at(&path, &empty).unwrap();
        assert!(read_model_table_at(&path, 1_001).is_none());
    }

    #[test]
    fn live_table_overrides_builtin_in_resolution() {
        let table = ModelTable {
            fetched_at_ms: 1_000,
            source: "openrouter-api".into(),
            volume: "live/volume".into(),
            verifier: "live/verifier".into(),
            quality: "live/quality".into(),
        };
        assert_eq!(
            resolve_model(None, Some("explorer"), Some(&table))
                .unwrap()
                .as_deref(),
            Some("live/volume")
        );
        assert_eq!(
            resolve_model(None, Some("security"), Some(&table))
                .unwrap()
                .as_deref(),
            Some("live/quality")
        );
        // Built-in snapshot when no cache is supplied.
        assert_eq!(
            resolve_model(None, Some("explorer"), None)
                .unwrap()
                .as_deref(),
            Some(SLUG_VOLUME)
        );
    }
}
