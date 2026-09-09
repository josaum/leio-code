#!/usr/bin/env python3
"""Run a real local build -> HTTP delivery -> rollback, then expand JSON-LD with PyLD."""
import argparse
import json
import pathlib
import subprocess
import tempfile
import urllib.request

from pyld import jsonld


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/leio-harness")
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    root = pathlib.Path(tempfile.mkdtemp(prefix="leio-delivery-smoke-")).resolve()
    repo = root / "repo"
    repo.mkdir()
    run = root / "run"
    target = root / "delivery"

    def call(*argv):
        return json.loads(subprocess.check_output([binary, *map(str, argv)]))

    def workflow(action, *argv):
        return call("workflow", "--dir", run, "--action", action, *argv)

    def delivery(action, *argv):
        return call("delivery", "--dir", target, "--action", action, *argv)

    evidence = root / "evidence.json"
    evidence.write_text(json.dumps({"scope": "synthetic local delivery acceptance fixture",
                                   "limitation": "not repository architecture evidence"}))

    def build(label, initial=False):
        plan = {"request": f"Build static release {label}", "constraints": [], "questions": [],
                "acceptance": [f"HTTP entry point serves release {label}"], "steps": [{
                    "name": "build site", "kind": "build",
                    "argv": ["/bin/sh", "-c", f"mkdir -p site; printf '<!doctype html><link rel=stylesheet href=/style.css><h1>{label}</h1>' > site/index.html; printf 'h1 {{ color: navy; }}' > site/style.css"],
                    "timeout_ms": 5000, "retry_safe": True,
                    "artifacts": ["site/index.html", "site/style.css"]}]}
        path = root / "plan.json"
        path.write_text(json.dumps(plan))
        state = workflow("init" if initial else "revise", "--input", path,
                         *( ["--repo", repo] if initial else []))
        workflow("evidence", "--input", evidence)
        workflow("confirm", "--approval", state["digest"])
        workflow("approve", "--approval", state["digest"])
        state = workflow("execute", "--approval", state["digest"])
        assert state["state"] == "completed"
        return delivery("prepare", "--source", repo / "site")

    first = build("release-A", initial=True)
    server = subprocess.Popen([binary, "delivery", "--dir", str(target), "--action", "serve",
                               "--address", "127.0.0.1:0"], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True)
    try:
        address = json.loads(server.stdout.readline())["address"]

        def promote(prepared):
            return delivery("deploy", "--revision", prepared["revision"], "--approval",
                            prepared["authorization_digest"], "--address", address)

        assert promote(first)["revision"] == first["revision"]
        second = build("release-B")
        assert promote(second)["revision"] == second["revision"]
        with urllib.request.urlopen(f"http://{address}/") as response:
            assert b"release-B" in response.read()
        current = delivery("inspect")
        rolled_back = delivery("rollback", "--approval", current["rollback_authorization_digest"],
                               "--address", address)
        assert rolled_back["revision"] == first["revision"]
        with urllib.request.urlopen(f"http://{address}/") as response:
            assert b"release-A" in response.read()
        assert delivery("verify", "--address", address)["revision"] == first["revision"]
    finally:
        server.terminate()
        server.communicate(timeout=10)

    documents = []
    for path in sorted(root.rglob("*.jsonld")):
        value = json.loads(path.read_text())
        assert jsonld.expand(value)
        quads = jsonld.to_rdf(value, {"format": "application/n-quads"})
        assert value["@id"] in quads
        assert "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON" in quads
        documents.append({"path": str(path), "rdf_quads": len(quads.splitlines())})
    assert len(documents) == 4
    print(json.dumps({"result": "build, deploy A, deploy B, rollback A verified",
                      "root": str(root), "documents": documents}))


if __name__ == "__main__":
    main()
