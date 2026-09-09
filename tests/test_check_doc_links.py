import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class CheckDocLinksTests(unittest.TestCase):
    def test_exit_ok_for_coherent_minimal_tree(self) -> None:
        script = Path(__file__).resolve().parent.parent / "scripts" / "check_doc_links.py"
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[link](docs/x.md)\n", encoding="utf-8")
            (root / "docs").mkdir()
            (root / "docs" / "x.md").write_text("# ok\n", encoding="utf-8")
            r = subprocess.run(
                [sys.executable, str(script), "--leio-root", str(root), "--workspace-root", str(root)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(r.returncode, 0, msg=r.stderr)

    def test_exit_error_when_target_missing(self) -> None:
        script = Path(__file__).resolve().parent.parent / "scripts" / "check_doc_links.py"
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text("[missing](docs/nope.md)\n", encoding="utf-8")
            r = subprocess.run(
                [sys.executable, str(script), "--leio-root", str(root), "--workspace-root", str(root)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(r.returncode, 0)


if __name__ == "__main__":
    unittest.main()
