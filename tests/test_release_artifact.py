import importlib.util
import json
import os
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "verify_release_artifact.py"
SPEC = importlib.util.spec_from_file_location("verify_release_artifact", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
verify_release_artifact = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_release_artifact)

PACKAGE_MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "package_codex_plugin.py"
PACKAGE_SPEC = importlib.util.spec_from_file_location("package_codex_plugin", PACKAGE_MODULE_PATH)
assert PACKAGE_SPEC is not None
assert PACKAGE_SPEC.loader is not None
package_codex_plugin = importlib.util.module_from_spec(PACKAGE_SPEC)
PACKAGE_SPEC.loader.exec_module(package_codex_plugin)


def write_required_payload(root: Path) -> Path:
    payload = root / "leio-code"
    for rel in verify_release_artifact.REQUIRED_ARCHIVE_PATHS:
        path = payload / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rel, encoding="utf-8")
    return payload


class ReleaseArtifactTests(unittest.TestCase):
    def test_license_files_survive_packaging(self) -> None:
        repo = Path(__file__).resolve().parents[1]
        notices = ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY.md")
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "source"
            fixture.mkdir()
            for name in notices:
                (fixture / name).write_bytes((repo / name).read_bytes())
            packed = package_codex_plugin.copy_payload(fixture, Path(tmp) / "stage", "leio-code")
            archive = Path(tmp) / "notices.tar.gz"
            package_codex_plugin.build_tar_gz(packed.parent, archive, "leio-code")
            verify_release_artifact.verify_archive_payload(archive, "leio-code", notices)
            with tarfile.open(archive) as tar:
                for name in notices:
                    self.assertEqual(tar.extractfile("leio-code/" + name).read(), (repo / name).read_bytes())

    def test_packager_builds_deterministic_archives(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            staging_root = root / "staging"
            payload = staging_root / "leio-code"
            (payload / "skills").mkdir(parents=True)
            (payload / "README.md").write_text("readme\n", encoding="utf-8")
            (payload / "skills" / "SKILL.md").write_text("skill\n", encoding="utf-8")

            first_tar = root / "first.tar.gz"
            second_tar = root / "second.tar.gz"
            first_zip = root / "first.zip"
            second_zip = root / "second.zip"

            package_codex_plugin.build_tar_gz(staging_root, first_tar, "leio-code")
            package_codex_plugin.build_zip(staging_root, first_zip, "leio-code")

            for path in payload.rglob("*"):
                os.utime(path, (1_700_000_000, 1_700_000_000))

            package_codex_plugin.build_tar_gz(staging_root, second_tar, "leio-code")
            package_codex_plugin.build_zip(staging_root, second_zip, "leio-code")

            self.assertEqual(
                verify_release_artifact.sha256_hex(first_tar),
                verify_release_artifact.sha256_hex(second_tar),
            )
            self.assertEqual(
                verify_release_artifact.sha256_hex(first_zip),
                verify_release_artifact.sha256_hex(second_zip),
            )

    def test_verify_manifest_checks_archive_payloads_and_digests(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            payload = write_required_payload(root)
            tar_path = root / "leio-code-plugin-v2.0.0.tar.gz"
            zip_path = root / "leio-code-plugin-v2.0.0.zip"

            with tarfile.open(tar_path, "w:gz") as archive:
                archive.add(payload, arcname="leio-code")
            with zipfile.ZipFile(zip_path, "w") as archive:
                for path in payload.rglob("*"):
                    if path.is_file():
                        archive.write(path, arcname=str(path.relative_to(root)))

            manifest = {
                "plugin_name": "leio-code",
                "version": "2.0.0",
                "artifacts": [
                    {
                        "path": str(tar_path),
                        "format": "tar.gz",
                        "sha256": verify_release_artifact.sha256_hex(tar_path),
                        "size_bytes": tar_path.stat().st_size,
                    },
                    {
                        "path": str(zip_path),
                        "format": "zip",
                        "sha256": verify_release_artifact.sha256_hex(zip_path),
                        "size_bytes": zip_path.stat().st_size,
                    },
                ],
            }
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            result = verify_release_artifact.verify_manifest(manifest_path, install_smoke=False)

            self.assertEqual(result["status"], "ok")
            self.assertEqual(len(result["artifacts"]), 2)

    def test_verify_archive_payload_rejects_forbidden_build_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            payload = write_required_payload(root)
            forbidden = payload / "mcp" / "node_modules" / "left-pad" / "index.js"
            forbidden.parent.mkdir(parents=True, exist_ok=True)
            forbidden.write_text("module.exports = 1", encoding="utf-8")
            tar_path = root / "bad.tar.gz"
            with tarfile.open(tar_path, "w:gz") as archive:
                archive.add(payload, arcname="leio-code")

            with self.assertRaisesRegex(RuntimeError, "forbidden"):
                verify_release_artifact.verify_archive_payload(tar_path, "leio-code")


if __name__ == "__main__":
    unittest.main()
