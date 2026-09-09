import importlib.util
import json
import os
import tempfile
import unittest
from unittest import mock
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "install_codex_plugin.py"
SPEC = importlib.util.spec_from_file_location("install_codex_plugin", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
install_codex_plugin = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(install_codex_plugin)


def create_plugin_source(root: Path) -> Path:
    plugin_root = root / "leio-code-source"
    (plugin_root / ".codex-plugin").mkdir(parents=True)
    (plugin_root / ".codex-plugin" / "plugin.json").write_text(
        json.dumps({"name": "leio-code"}),
        encoding="utf-8",
    )
    (plugin_root / "skills").mkdir()
    (plugin_root / "skills" / "README.md").write_text("skill", encoding="utf-8")
    (plugin_root / "target").mkdir()
    (plugin_root / "target" / "ignored.txt").write_text("ignored", encoding="utf-8")
    return plugin_root


def assert_compatibility_link(test_case: unittest.TestCase, repo_root: Path, installed_path: Path) -> None:
    compat_path = repo_root / ".agents" / "plugins" / "plugins" / install_codex_plugin.PLUGIN_NAME
    test_case.assertTrue(compat_path.is_symlink())
    test_case.assertEqual(compat_path.resolve(), installed_path.resolve())


class InstallCodexPluginTests(unittest.TestCase):
    def test_ensure_marketplace_creates_repo_root_plugins_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo_root = Path(tmp)

            marketplace_path = install_codex_plugin.ensure_marketplace(repo_root)

            self.assertEqual(
                marketplace_path,
                repo_root / ".agents" / "plugins" / "marketplace.json",
            )
            self.assertTrue((repo_root / "plugins").is_dir())
            self.assertFalse((repo_root / ".agents" / "plugins" / "plugins").exists())

            marketplace = json.loads(marketplace_path.read_text(encoding="utf-8"))
            self.assertEqual(marketplace["name"], "local-plugin-marketplace")
            self.assertEqual(marketplace["interface"]["displayName"], "Local Plugins")
            self.assertEqual(marketplace["plugins"], [])

    def test_install_plugin_copy_uses_repo_root_plugins_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo_root = root / "repo"
            repo_root.mkdir()
            source_root = create_plugin_source(root)

            install_codex_plugin.ensure_marketplace(repo_root)
            installed_path = install_codex_plugin.install_plugin(source_root, repo_root, "copy", force=False)

            self.assertEqual(installed_path, repo_root / "plugins" / install_codex_plugin.PLUGIN_NAME)
            self.assertTrue((installed_path / ".codex-plugin" / "plugin.json").is_file())
            self.assertTrue((installed_path / "skills" / "README.md").is_file())
            self.assertFalse(installed_path.is_symlink())
            self.assertFalse((installed_path / "target").exists())
            assert_compatibility_link(self, repo_root, installed_path)

    def test_install_plugin_symlink_uses_repo_root_plugins_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo_root = root / "repo"
            repo_root.mkdir()
            source_root = create_plugin_source(root)

            install_codex_plugin.ensure_marketplace(repo_root)
            installed_path = install_codex_plugin.install_plugin(source_root, repo_root, "symlink", force=False)

            self.assertEqual(installed_path, repo_root / "plugins" / install_codex_plugin.PLUGIN_NAME)
            self.assertTrue(installed_path.is_symlink())
            self.assertEqual(installed_path.resolve(), source_root.resolve())
            self.assertEqual(
                Path(os.readlink(installed_path)),
                Path(os.path.relpath(source_root, installed_path.parent)),
            )
            assert_compatibility_link(self, repo_root, installed_path)

    def test_marketplace_entry_resolves_to_installed_plugin(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo_root = root / "repo"
            repo_root.mkdir()
            source_root = create_plugin_source(root)

            marketplace_path = install_codex_plugin.ensure_marketplace(repo_root)
            install_codex_plugin.install_plugin(source_root, repo_root, "symlink", force=False)
            install_codex_plugin.update_marketplace(marketplace_path)

            marketplace = json.loads(marketplace_path.read_text(encoding="utf-8"))
            source_path = marketplace["plugins"][0]["source"]["path"]
            repo_root_source = repo_root / Path(source_path)
            marketplace_root_source = marketplace_path.parent / Path(source_path)

            self.assertTrue(repo_root_source.exists())
            self.assertTrue((repo_root_source / ".codex-plugin" / "plugin.json").is_file())
            self.assertTrue(marketplace_root_source.exists())
            self.assertTrue((marketplace_root_source / ".codex-plugin" / "plugin.json").is_file())

    def test_update_marketplace_appends_plugin_entry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo_root = Path(tmp)
            marketplace_path = install_codex_plugin.ensure_marketplace(repo_root)

            install_codex_plugin.update_marketplace(marketplace_path)

            marketplace = json.loads(marketplace_path.read_text(encoding="utf-8"))
            self.assertEqual(len(marketplace["plugins"]), 1)
            self.assertEqual(
                marketplace["plugins"][0],
                {
                    "name": "leio-code",
                    "source": {"source": "local", "path": "./plugins/leio-code"},
                    "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                    "category": "Developer Tools",
                },
            )

    def test_update_marketplace_normalizes_existing_plugin_entry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo_root = Path(tmp)
            marketplace_path = install_codex_plugin.ensure_marketplace(repo_root)
            marketplace_path.write_text(
                json.dumps(
                    {
                        "name": "local-plugin-marketplace",
                        "interface": {"displayName": "Local Plugins"},
                        "plugins": [
                            {
                                "name": "leio-code",
                                "source": {"source": "local", "path": "./plugins/old-location"},
                                "policy": {"installation": "NOT_AVAILABLE", "authentication": "ON_USE"},
                                "category": "Old Category",
                            }
                        ],
                    },
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )

            install_codex_plugin.update_marketplace(marketplace_path)

            marketplace = json.loads(marketplace_path.read_text(encoding="utf-8"))
            self.assertEqual(len(marketplace["plugins"]), 1)
            self.assertEqual(marketplace["plugins"][0]["source"]["path"], "./plugins/leio-code")
            self.assertEqual(
                marketplace["plugins"][0]["policy"],
                {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
            )
            self.assertEqual(marketplace["plugins"][0]["category"], "Developer Tools")

    def test_prepare_mcp_dependencies_noops_without_package_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            plugin_root = Path(tmp)
            with mock.patch.object(install_codex_plugin.subprocess, "run") as run_mock:
                install_codex_plugin.prepare_mcp_dependencies(plugin_root)
            run_mock.assert_not_called()

    def test_prepare_mcp_dependencies_uses_npm_ci_when_lockfile_exists(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            plugin_root = Path(tmp)
            mcp_dir = plugin_root / "mcp"
            mcp_dir.mkdir()
            (mcp_dir / "package.json").write_text("{}", encoding="utf-8")
            (mcp_dir / "package-lock.json").write_text("{}", encoding="utf-8")

            with mock.patch.object(install_codex_plugin.subprocess, "run") as run_mock:
                install_codex_plugin.prepare_mcp_dependencies(plugin_root)

            run_mock.assert_called_once()
            args, kwargs = run_mock.call_args
            self.assertEqual(args[0], ["npm", "ci", "--omit=dev"])
            self.assertEqual(kwargs["cwd"], mcp_dir)
            self.assertTrue(kwargs["check"])

    def test_installer_payload_includes_apps_sdk_surface(self) -> None:
        self.assertIn(".leio-code/config.toml", install_codex_plugin.INCLUDED_PATHS)
        self.assertIn("apps-sdk", install_codex_plugin.INCLUDED_PATHS)
        self.assertIn("benchmarks", install_codex_plugin.INCLUDED_PATHS)
        self.assertIn("docs", install_codex_plugin.INCLUDED_PATHS)
        self.assertIn("hooks", install_codex_plugin.INCLUDED_PATHS)
        self.assertIn("crates/leio-knowledge-core", install_codex_plugin.INCLUDED_PATHS)


if __name__ == "__main__":
    unittest.main()
