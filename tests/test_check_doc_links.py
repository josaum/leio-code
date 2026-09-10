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

    def test_snake_case_fragments_keep_their_underscores(self) -> None:
        # GitHub only treats `_` as emphasis at a word boundary, so an identifier
        # heading slugs with its underscores intact.
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "[a](docs/x.md#query_dead_code)\n[b](docs/x.md#italic)\n", encoding="utf-8"
            )
            (root / "docs").mkdir()
            (root / "docs" / "x.md").write_text(
                "# query_dead_code\n\n# _italic_\n", encoding="utf-8"
            )
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, msg=result.stdout + result.stderr)

    def test_root_named_like_a_skipped_directory_still_scans(self) -> None:
        # Skip names must match below the root. Matching the absolute path made a
        # checkout under any `target/` or `vendor/` ancestor report a green run
        # having checked nothing at all.
        with tempfile.TemporaryDirectory() as td:
            root = Path(td) / "target" / "repo"
            (root / "docs").mkdir(parents=True)
            (root / "README.md").write_text("[missing](docs/nope.md)\n", encoding="utf-8")
            result = self.run_check(root)
            self.assertNotEqual(
                result.returncode, 0, msg="a broken link under such a root must still fail"
            )

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
