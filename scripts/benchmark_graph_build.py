import argparse
from contextlib import nullcontext
import hashlib
import json
import math
from pathlib import Path
import resource
import subprocess
import tempfile
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_build(binary, repository, output, timeout):
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.perf_counter()
    command = [
        str(binary),
        "--json",
        "--repo",
        str(repository),
        "export",
        "code-graph",
        "--output-dir",
        str(output),
    ]
    try:
        completed = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout, check=False
        )
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "elapsed_seconds": time.perf_counter() - started}
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    result = {
        "status": "passed" if completed.returncode == 0 else "failed",
        "exit_code": completed.returncode,
        "elapsed_seconds": time.perf_counter() - started,
        "user_cpu_seconds": after.ru_utime - before.ru_utime,
        "system_cpu_seconds": after.ru_stime - before.ru_stime,
    }
    if completed.returncode:
        result["stderr"] = completed.stderr
        return result
    envelope = json.loads(completed.stdout)
    manifest = json.loads((output / "manifest.json").read_text())
    result["timing_ms"] = envelope["timing_ms"]
    result["counts"] = {
        key: manifest[key]
        for key in (
            "parsed_file_count",
            "symbol_count",
            "occurrence_count",
            "import_edge_count",
            "call_edge_count",
            "resolved_call_edge_count",
        )
    }
    result["source_fingerprint"] = manifest["source_fingerprint"]
    result["graph_sha256"] = digest(output / "graph.nq")
    result["cache_sha256"] = digest(output / "query-cache.json")
    result["warnings"] = envelope["warnings"]
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--retain-artifacts", action="store_true")
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    repository = arguments.repo.resolve(strict=True)
    if not binary.is_file() or not repository.is_dir():
        parser.error("binary must be a file and repo must be a directory")
    if arguments.repeats < 1 or not math.isfinite(arguments.timeout) or arguments.timeout <= 0:
        parser.error("repeats must be positive and timeout must be finite and positive")
    manager = (
        nullcontext(tempfile.mkdtemp(prefix="leio-cold-graph-"))
        if arguments.retain_artifacts
        else tempfile.TemporaryDirectory(prefix="leio-cold-graph-")
    )
    with manager as directory:
        runs = []
        for number in range(arguments.repeats):
            result = run_build(
                binary, repository, Path(directory) / str(number), arguments.timeout
            )
            runs.append(result)
            if result["status"] != "passed":
                break
    passed = len(runs) == arguments.repeats and all(
        result["status"] == "passed" for result in runs
    )
    consistent = passed and all(
        all(result[key] == runs[0][key] for key in (
            "counts", "source_fingerprint", "graph_sha256", "cache_sha256"
        ))
        for result in runs
    )
    print(json.dumps({
        "binary": str(binary),
        "binary_sha256": digest(binary),
        "repository": str(repository),
        "artifacts_directory": directory if arguments.retain_artifacts else None,
        "cold_builds": runs,
        "identical_graph_and_cache": consistent,
    }, indent=2))
    return 0 if consistent else 1


if __name__ == "__main__":
    raise SystemExit(main())
