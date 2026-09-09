import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class CheckDocLinksTests(unittest.TestCase):
    @staticmethod
    def script() -> Path:
        return Path(__file__).resolve().parent.parent / "scripts" / "check_doc_links.py"

    def run_check(self, root: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(self.script()),
                "--leio-root",
                str(root),
                "--workspace-root",
                str(root),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_exit_ok_for_coherent_minimal_tree(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[link](docs/x.md#details)\n", encoding="utf-8")
            (root / "docs").mkdir()
            (root / "docs" / "x.md").write_text("# Details\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, msg=result.stderr)

    def test_exit_error_when_target_missing(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[missing](docs/nope.md)\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertNotEqual(result.returncode, 0)

    def test_exit_error_when_fragment_missing(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[missing](docs/x.md#absent)\n", encoding="utf-8")
            (root / "docs").mkdir()
            (root / "docs" / "x.md").write_text("# Present\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("#absent", result.stderr)

    def test_duplicate_headings_receive_github_numeric_suffixes(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[second](docs/x.md#details-1)\n", encoding="utf-8")
            (root / "docs").mkdir()
            (root / "docs" / "x.md").write_text("# Details\n\n## Details\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, msg=result.stderr)

    def test_inline_html_sources_are_checked(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                '<p><img src="assets/missing.png" alt="Missing"></p>\n',
                encoding="utf-8",
            )
            result = self.run_check(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("assets/missing.png", result.stderr)

    def test_code_fences_do_not_create_links(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "```python\nRESOLVERS[route.resolves](...)\n```\n",
                encoding="utf-8",
            )
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, msg=result.stderr)

    def test_nested_worktrees_are_not_scanned(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("# Root\n", encoding="utf-8")
            nested = root / ".claude" / "worktrees" / "agent-a"
            nested.mkdir(parents=True)
            (nested / "README.md").write_text("[broken](missing.md)\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, msg=result.stderr)
            self.assertIn("(1 markdown files)", result.stdout)


if __name__ == "__main__":
    unittest.main()
