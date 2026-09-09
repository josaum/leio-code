use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SisfronSimulationDurabilityDoctor;

impl Doctor for SisfronSimulationDurabilityDoctor {
    fn name(&self) -> &'static str {
        "sisfron-simulation-durability"
    }

    fn description(&self) -> &'static str {
        "Checks that SISFRON simulations persist durably, rehydrate outside the in-memory registry, reset in place, and expose named timeline snapshots."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_sisfron_simulation_durability(root)
    }
}

pub fn doctor_sisfron_simulation_durability(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let persistence_path = root.join("cartridges/sisfron/simulation/persistence.py");
    let api_path = root.join("cartridges/sisfron/simulation/simulation_api.py");
    let sim_path = root.join("cartridges/sisfron/simulation/smem_simulation.py");
    let clock_path = root.join("cartridges/sisfron/simulation/sim_clock.py");
    let tests_path = root.join("cartridges/sisfron/tests/test_simulation_api.py");

    let persistence_src = read_text(&persistence_path, &mut warnings);
    let api_src = read_text(&api_path, &mut warnings);
    let sim_src = read_text(&sim_path, &mut warnings);
    let clock_src = read_text(&clock_path, &mut warnings);
    let tests_src = read_text(&tests_path, &mut warnings);

    let persistence_contract = persistence_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "class SimulationPersistence:",
                "Path(config.state_dir) / \"simulations\"",
                "def load_timeline(",
                "redis.Redis.from_url(",
                "self._write_redis(record)",
                "record.get(\"timeline\", [])",
            ],
        )
    });
    let api_rehydration_contract = api_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _get_persistence() -> SimulationPersistence:",
                "def _rehydrate_simulation(record: dict[str, Any]) -> SmemSimulation:",
                "def _get_sim(sim_id: str) -> SmemSimulation:",
                "record = _get_persistence().load(sim_id)",
                "sim = _rehydrate_simulation(record)",
                "_SIMULATIONS[sim_id] = sim",
            ],
        )
    });
    let api_reset_contract = api_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "elif cmd.command == \"reset\":",
                "state = await sim.reset()",
                "result[\"status\"] = \"reset\"",
            ],
        )
    });
    let api_timeline_contract = api_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "timeline = _get_persistence().load_timeline(",
                "sim_id, from_tick=from_tick, to_tick=to_tick",
                "\"timeline\": timeline,",
                "\"named_graph_uris\": [snapshot.get(\"graph_uri\") for snapshot in timeline],",
            ],
        )
    });
    let simulation_record_contract = sim_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def set_state_change_hook(",
                "def export_record(self) -> dict[str, Any]:",
                "\"timeline\": list(self._timeline),",
                "\"named_graph_snapshots\": dict(self._named_graph_snapshots),",
                "def from_record(",
                "sim._timeline = list(record.get(\"timeline\", []))",
                "sim._named_graph_snapshots = dict(record.get(\"named_graph_snapshots\", {}))",
            ],
        )
    });
    let simulation_archive_contract = sim_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "async def reset(self) -> SimulationState:",
                "self._timeline.clear()",
                "self._named_graph_snapshots.clear()",
                "graph_uri = f\"urn:sisfron:sim:{self.sim_id}:tick:{self.state.tick}\"",
                "self._timeline.append(snapshot)",
                "self._state_change_hook(self)",
            ],
        )
    });
    let clock_contract = clock_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def restore_state(",
                "def reset(",
                "self._recorded_timeline = list(recorded_timeline or [])",
                "self._recorded_timeline.clear()",
            ],
        )
    });
    let regression_tests_present = tests_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "test_get_simulation_state_rehydrates_from_persistence",
                "test_control_reset_restores_initial_state_in_place",
                "test_timeline_returns_durable_named_snapshots",
            ],
        )
    });

    if !persistence_contract {
        warnings.push(
            "SISFRON simulation persistence no longer guarantees durable state_dir plus Redis-backed storage with timeline loading"
                .to_string(),
        );
    }
    if !api_rehydration_contract {
        warnings.push(
            "simulation_api.py no longer rehydrates SISFRON simulations lazily from durable persistence when the in-memory registry misses"
                .to_string(),
        );
    }
    if !api_reset_contract {
        warnings.push(
            "simulation_api.py no longer exposes in-place reset through /sisfron/simulations/{sim_id}/control"
                .to_string(),
        );
    }
    if !api_timeline_contract {
        warnings.push(
            "simulation_api.py no longer returns durable timeline snapshots and named graph URIs from persistence"
                .to_string(),
        );
    }
    if !simulation_record_contract {
        warnings.push(
            "smem_simulation.py no longer exports and rehydrates durable simulation records with timeline and named snapshot state"
                .to_string(),
        );
    }
    if !simulation_archive_contract {
        warnings.push(
            "smem_simulation.py no longer archives deterministic SISFRON named snapshots or invokes the persistence hook after state changes"
                .to_string(),
        );
    }
    if !clock_contract {
        warnings.push(
            "sim_clock.py no longer preserves replay/clock restoration and reset semantics needed for simulation durability"
                .to_string(),
        );
    }
    if !regression_tests_present {
        warnings.push(
            "test_simulation_api.py is missing the SISFRON durability regression tests for rehydration, reset, and named timeline snapshots"
                .to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &persistence_path,
            persistence_src.as_ref(),
            "class SimulationPersistence:",
            "SISFRON simulations use a dedicated persistence adapter",
        ),
        (
            &persistence_path,
            persistence_src.as_ref(),
            "Path(config.state_dir) / \"simulations\"",
            "simulation records are persisted under the cartridge state_dir",
        ),
        (
            &api_path,
            api_src.as_ref(),
            "record = _get_persistence().load(sim_id)",
            "simulation API lazily reloads simulations from durable storage",
        ),
        (
            &api_path,
            api_src.as_ref(),
            "elif cmd.command == \"reset\":",
            "simulation API exposes in-place reset",
        ),
        (
            &api_path,
            api_src.as_ref(),
            "\"named_graph_uris\": [snapshot.get(\"graph_uri\") for snapshot in timeline],",
            "timeline endpoint returns named graph lineage from durable snapshots",
        ),
        (
            &sim_path,
            sim_src.as_ref(),
            "def export_record(self) -> dict[str, Any]:",
            "simulation runtime exports a durable record",
        ),
        (
            &sim_path,
            sim_src.as_ref(),
            "graph_uri = f\"urn:sisfron:sim:{self.sim_id}:tick:{self.state.tick}\"",
            "simulation snapshots use deterministic SISFRON named graph URIs",
        ),
        (
            &clock_path,
            clock_src.as_ref(),
            "def restore_state(",
            "simulation clock can restore persisted state and replay lineage",
        ),
        (
            &tests_path,
            tests_src.as_ref(),
            "test_get_simulation_state_rehydrates_from_persistence",
            "regression tests cover durable rehydration",
        ),
    ] {
        push_evidence(&mut evidence, path, src, needle, detail);
    }

    entities.push(json!({
        "path": persistence_path.display().to_string(),
        "durable_state_dir_and_redis": persistence_contract,
    }));
    entities.push(json!({
        "path": api_path.display().to_string(),
        "lazy_rehydration": api_rehydration_contract,
        "reset_endpoint": api_reset_contract,
        "durable_timeline_endpoint": api_timeline_contract,
    }));
    entities.push(json!({
        "path": sim_path.display().to_string(),
        "record_export_rehydrate": simulation_record_contract,
        "named_snapshot_archives": simulation_archive_contract,
    }));
    entities.push(json!({
        "path": clock_path.display().to_string(),
        "clock_restore_reset": clock_contract,
    }));
    entities.push(json!({
        "path": tests_path.display().to_string(),
        "durability_regression_tests": regression_tests_present,
    }));

    let summary = if warnings.is_empty() {
        "SISFRON simulation durability invariants hold across persistence, lazy rehydration, reset, named timeline snapshots, and regression coverage".to_string()
    } else {
        format!(
            "SISFRON simulation durability doctor found {} warnings across persistence, API, runtime, clock, and test coverage",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_sisfron_simulation_durability"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.66 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "sisfron-simulation-durability",
            "contracts": {
                "persistence_contract": persistence_contract,
                "api_rehydration_contract": api_rehydration_contract,
                "api_reset_contract": api_reset_contract,
                "api_timeline_contract": api_timeline_contract,
                "simulation_record_contract": simulation_record_contract,
                "simulation_archive_contract": simulation_archive_contract,
                "clock_contract": clock_contract,
                "regression_tests_present": regression_tests_present,
            }
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn source_contains_all(src: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| src.contains(needle))
}

fn push_evidence(
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&String>,
    needle: &str,
    detail: &str,
) {
    if let Some(src) = src
        && let Some(line) = find_line(src, needle)
    {
        evidence.push(EvidenceItem {
            kind: "sisfron_simulation_durability".to_string(),
            path: path.display().to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::doctor_sisfron_simulation_durability;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-code-sisfron-sim-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cartridges/sisfron/simulation")).expect("create sim dir");
        fs::create_dir_all(root.join("cartridges/sisfron/tests")).expect("create tests dir");
        root
    }

    fn write_fixture(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn detects_happy_path_sisfron_simulation_durability_contract() {
        let root = temp_root("happy");
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/persistence.py",
            r#"
class SimulationPersistence:
    def __init__(self, config):
        self.root = Path(config.state_dir) / "simulations"
    def save(self, sim):
        self._write_redis(record)
    def load_timeline(self):
        return record.get("timeline", [])
    def connect(self):
        redis.Redis.from_url(self.config.redis_url)
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/simulation_api.py",
            r#"
def _get_persistence() -> SimulationPersistence:
    pass
def _rehydrate_simulation(record: dict[str, Any]) -> SmemSimulation:
    pass
def control():
    elif cmd.command == "reset":
        state = await sim.reset()
        result["status"] = "reset"
def timeline():
    timeline = _get_persistence().load_timeline(sim_id, from_tick=from_tick, to_tick=to_tick)
    payload = {
        "timeline": timeline,
        "named_graph_uris": [snapshot.get("graph_uri") for snapshot in timeline],
    }
def _get_sim(sim_id: str) -> SmemSimulation:
    record = _get_persistence().load(sim_id)
    sim = _rehydrate_simulation(record)
    _SIMULATIONS[sim_id] = sim
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/smem_simulation.py",
            r#"
async def reset(self) -> SimulationState:
    self._timeline.clear()
    self._named_graph_snapshots.clear()
def set_state_change_hook(self):
    pass
def export_record(self) -> dict[str, Any]:
    return {
        "timeline": list(self._timeline),
        "named_graph_snapshots": dict(self._named_graph_snapshots),
    }
def from_record():
    sim._timeline = list(record.get("timeline", []))
    sim._named_graph_snapshots = dict(record.get("named_graph_snapshots", {}))
def archive():
    graph_uri = f"urn:sisfron:sim:{self.sim_id}:tick:{self.state.tick}"
    self._timeline.append(snapshot)
    self._state_change_hook(self)
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/sim_clock.py",
            r#"
def restore_state(
):
    self._recorded_timeline = list(recorded_timeline or [])
def reset(
):
    self._recorded_timeline.clear()
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_simulation_api.py",
            r#"
def test_get_simulation_state_rehydrates_from_persistence():
    pass
def test_control_reset_restores_initial_state_in_place():
    pass
def test_timeline_returns_durable_named_snapshots():
    pass
"#,
        );

        let envelope = doctor_sisfron_simulation_durability(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn warns_when_rehydration_contract_disappears() {
        let root = temp_root("warn");
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/persistence.py",
            "class SimulationPersistence:\n    self.root = Path(config.state_dir) / \"simulations\"\n    def load_timeline(self):\n        return record.get(\"timeline\", [])\n    redis.Redis.from_url(self.config.redis_url)\n    self._write_redis(record)\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/simulation_api.py",
            "def _get_persistence() -> SimulationPersistence:\n    pass\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/smem_simulation.py",
            "def export_record(self) -> dict[str, Any]:\n    return {}\ndef from_record():\n    pass\nasync def reset(self) -> SimulationState:\n    self._timeline.clear()\n    self._named_graph_snapshots.clear()\ngraph_uri = f\"urn:sisfron:sim:{self.sim_id}:tick:{self.state.tick}\"\nself._timeline.append(snapshot)\nself._state_change_hook(self)\ndef set_state_change_hook(self):\n    pass\n\"timeline\": list(self._timeline)\n\"named_graph_snapshots\": dict(self._named_graph_snapshots)\nsim._timeline = list(record.get(\"timeline\", []))\nsim._named_graph_snapshots = dict(record.get(\"named_graph_snapshots\", {}))\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/simulation/sim_clock.py",
            "def restore_state(\n):\n    self._recorded_timeline = list(recorded_timeline or [])\ndef reset(\n):\n    self._recorded_timeline.clear()\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_simulation_api.py",
            "def test_get_simulation_state_rehydrates_from_persistence():\n    pass\ndef test_control_reset_restores_initial_state_in_place():\n    pass\ndef test_timeline_returns_durable_named_snapshots():\n    pass\n",
        );

        let envelope = doctor_sisfron_simulation_durability(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("rehydrates SISFRON simulations lazily")),
            "{:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(root);
    }
}
