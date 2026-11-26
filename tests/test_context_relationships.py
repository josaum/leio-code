"""CLI regression for evidence crossing package boundaries and Cargo binary ownership."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class ContextRelationships(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.binary = os.environ.get(
            "LEIO_CODE_BIN", str(Path.home() / ".cargo/bin/leio-code")
        )

    def write(self, name, source):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source)

    def run_cli(self, *args):
        out = subprocess.check_output(
            [self.binary, "--repo", str(self.root), "--json", *args],
            env=dict(os.environ, LEIO_REUSE_INDEX="1", LEIO_DISABLE_EVENTS="1"),
        )
        return json.loads(out)

    def query(self, text):
        return self.run_cli("context", text, "--limit", "2")["entities"][0][
            "files_to_read"
        ]

    def test_cross_package_consumer(self):
        for package in ("core", "client", "unrelated"):
            self.write(package + "/package.json", json.dumps({"name": package}))
        self.write("core/logic.ts", "export function parseSignal() { return 1; }\n")
        self.write(
            "client/receiver.ts",
            "import { parseSignal } from '../core/logic';\nexport function receive() { return parseSignal(); }\n",
        )
        self.write("unrelated/receiver.ts", "export function receive() { return 1; }\n")
        self.run_cli("index")
        self.run_cli("export", "code-graph")
        rows = self.query("Update receiver behavior for `core/logic.ts`.")
        self.assertEqual(
            [r["path"] for r in rows], ["core/logic.ts", "client/receiver.ts"]
        )
        self.assertTrue(
            any(
                "resolved import" in s or "indexed caller" in s
                for s in rows[1]["reasons"]
            ),
            rows,
        )

    def test_cargo_binary_integration(self):
        for pkg in ("toolbox", "another"):
            self.write(
                pkg + "/Cargo.toml",
                f'[package]\nname = "{pkg}"\nversion = "0.1.0"\nedition = "2021"\n',
            )
        self.write("toolbox/src/bin/relay.rs", "fn main() {}\n")
        for pkg in ("toolbox", "another"):
            self.write(
                pkg + "/tests/smoke.rs",
                '#[test]\nfn invoke() { let _cmd = env!("CARGO_BIN_EXE_relay"); }\n',
            )
        self.run_cli("index")
        rows = self.query("Verify tests after editing `toolbox/src/bin/relay.rs`.")
        self.assertEqual(
            [r["path"] for r in rows],
            ["toolbox/src/bin/relay.rs", "toolbox/tests/smoke.rs"],
        )
        self.assertTrue(any("Cargo binary" in s for s in rows[1]["reasons"]), rows)
