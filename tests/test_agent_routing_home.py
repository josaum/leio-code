from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL_REL = "skills/leio-code/SKILL.md"
POINTER_FILES = (
    "docs/AGENT-ROUTING.md",
    "GEMINI.md",
    "codex/AGENTS.md",
    "skills/leio-code-apps-sdk/SKILL.md",
    "apps-sdk/README.md",
    "apps-sdk/SUBMISSION.md",
    "docs/MCP-SURFACE-GAP.md",
    "docs/DEPLOY-GCP.md",
    ".agents/skills/run-leio-code/SKILL.md",
)
HEDGE_PHRASES = (
    "if it responds",
    "when those tools are connected and respond",
    "if MCP is up",
    "when it responds",
    "if MCP is missing, still connecting",
)


class AgentRoutingHomeTests(unittest.TestCase):
    def skill_text(self) -> str:
        return (ROOT / SKILL_REL).read_text()

    def test_skill_owns_the_first_pass(self) -> None:
        skill = self.skill_text()
        self.assertIn("One loop per repo_root", skill)
        self.assertIn("`status`", skill)
        self.assertIn("`capabilities`", skill)
        self.assertIn("`context", skill)
        self.assertNotIn("docs/AGENT-ROUTING.md", skill)

    def test_skill_calls_mcp_immediately(self) -> None:
        skill = self.skill_text()
        self.assertIn("Call MCP `leio_code_*` immediately", skill)
        for phrase in HEDGE_PHRASES:
            self.assertNotIn(phrase, skill)

    def test_cli_is_only_after_a_failed_mcp_call(self) -> None:
        skill = self.skill_text()
        self.assertIn("A failed MCP call is the only reason to run the same query on the CLI", skill)

    def test_tool_map_covers_every_cli_family(self) -> None:
        skill = self.skill_text()
        for verb in (
            "status",
            "capabilities",
            "context",
            "find",
            "explain",
            "graph",
            "doctor",
            "audit",
            "export",
            "knowledge",
            "nav",
            "init",
        ):
            self.assertIn(f"`{verb}`", skill, msg=verb)

    def test_agent_routing_doc_is_a_pointer(self) -> None:
        text = (ROOT / "docs/AGENT-ROUTING.md").read_text()
        self.assertIn(SKILL_REL, text)
        self.assertNotIn("Live kind inventory", text)
        self.assertNotIn("**112**", text)
        self.assertNotIn("One loop per repo_root", text)

    def test_in_repo_pointers_do_not_restate_the_loop(self) -> None:
        for rel in POINTER_FILES:
            text = (ROOT / rel).read_text()
            self.assertTrue(
                "skills/leio-code/SKILL.md" in text or "leio-code/SKILL.md" in text,
                msg=f"{rel} must point at the skill",
            )
            self.assertNotIn("One loop per repo_root", text, msg=rel)
            for phrase in HEDGE_PHRASES:
                self.assertNotIn(phrase, text, msg=f"{rel}: {phrase}")

    def test_mcp_and_apps_guide_routing_doc_is_the_skill(self) -> None:
        mcp = (ROOT / "mcp/index.js").read_text()
        guide = (ROOT / "mcp/guide.js").read_text()
        apps = (ROOT / "apps-sdk/server.js").read_text()
        for source, blob in (("mcp/index.js", mcp), ("mcp/guide.js", guide), ("apps-sdk/server.js", apps)):
            self.assertIn("skills/leio-code/SKILL.md", blob, msg=source)
            self.assertNotIn('routingDoc: "docs/AGENT-ROUTING.md"', blob, msg=source)

    def test_session_hook_emits_the_skill(self) -> None:
        script = ROOT / "hooks/session-context.sh"
        source = script.read_text()
        self.assertIn(SKILL_REL, source)
        completed = subprocess.run(
            ["bash", str(script)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("One loop per repo_root", completed.stdout)
        self.assertIn("Call MCP `leio_code_*` immediately", completed.stdout)
        self.assertNotIn("name: leio-code", completed.stdout)

    def test_session_hook_fail_open_when_skill_missing(self) -> None:
        script = ROOT / "hooks/session-context.sh"
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "missing.md"
            completed = subprocess.run(
                ["bash", str(script)],
                cwd=ROOT,
                env={**os.environ, "LEIO_CODE_SKILL": str(missing)},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("skill missing", completed.stdout.lower())

    def test_session_hook_honors_leio_code_skill_override(self) -> None:
        script = ROOT / "hooks/session-context.sh"
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / "SKILL.md"
            fake.write_text("---\nname: fake\n---\nOVERRIDE_LOOP\n", encoding="utf-8")
            completed = subprocess.run(
                ["bash", str(script)],
                cwd=ROOT,
                env={**os.environ, "LEIO_CODE_SKILL": str(fake)},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("OVERRIDE_LOOP", completed.stdout)
            self.assertNotIn("name: fake", completed.stdout)

    def test_session_hook_strips_incomplete_frontmatter(self) -> None:
        script = ROOT / "hooks/session-context.sh"
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / "SKILL.md"
            fake.write_text("---\nname: broken\nno closing fence\n", encoding="utf-8")
            completed = subprocess.run(
                ["bash", str(script)],
                cwd=ROOT,
                env={**os.environ, "LEIO_CODE_SKILL": str(fake)},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertTrue(completed.stdout.startswith("---"), completed.stdout)

    def test_apps_sdk_skill_forbids_stdio_tool_names(self) -> None:
        text = (ROOT / "skills/leio-code-apps-sdk/SKILL.md").read_text()
        self.assertIn("Never call `leio_code_*` on this host", text)
        self.assertIn("guide_repository_tools", text)


class AgentRoutingHomeJsonTests(unittest.TestCase):
    def test_pointer_list_is_json_serializable_contract(self) -> None:
        payload = {
            "home": SKILL_REL,
            "pointers": list(POINTER_FILES),
        }
        roundtrip = json.loads(json.dumps(payload))
        self.assertEqual(roundtrip["home"], SKILL_REL)
        self.assertEqual(len(roundtrip["pointers"]), len(POINTER_FILES))


if __name__ == "__main__":
    unittest.main()
