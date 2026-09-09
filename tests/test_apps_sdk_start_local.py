from pathlib import Path
import unittest


SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "apps-sdk"
    / "scripts"
    / "start-local.sh"
)


class AppsSdkStartLocalTests(unittest.TestCase):
    def test_start_local_prefers_cargo_bin_before_workspace_target(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")

        cargo_bin = '"${CARGO_HOME:-$HOME/.cargo}/bin/${BINARY_NAME}"'
        release_candidate = '"${REPO_ROOT}/target/release/${BINARY_NAME}"'
        debug_candidate = '"${REPO_ROOT}/target/debug/${BINARY_NAME}"'

        self.assertIn(cargo_bin, text)
        self.assertIn(release_candidate, text)
        self.assertIn(debug_candidate, text)
        self.assertIn("is_cargo_target_bin", text)
        self.assertLess(text.index(cargo_bin), text.index(release_candidate))
        self.assertLess(text.index(release_candidate), text.index(debug_candidate))
        self.assertNotIn("command -v leio-code", text)

    def test_start_local_exports_resolved_binary_to_apps_sdk(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")

        self.assertIn('export LEIO_CODE_BIN="${LEIO_CODE_BIN_RESOLVED}"', text)

    def test_start_local_detaches_background_server(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")

        self.assertIn('nohup node "${APPS_SDK_DIR}/server.js" </dev/null', text)
        self.assertIn('disown "${server_pid}"', text)

    def test_start_local_uses_launchctl_on_macos(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")

        self.assertIn("launchctl submit", text)
        self.assertIn("com.josaum.leio-code.apps-sdk", text)
        self.assertIn('"LEIO_CODE_BIN=${LEIO_CODE_BIN:-}"', text)

    def test_start_local_indexes_this_checkout_not_the_parent_tree(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")

        self.assertIn('LEIO_CODE_ROOT="$(cd "${APPS_SDK_DIR}/.." && pwd)"', text)
        self.assertIn(
            'REPO_ROOT="${LEIO_CODE_REPO_ROOT:-${LEIO_CODE_ROOT}}"',
            text,
        )
        self.assertNotIn('APPS_SDK_DIR}/../.."', text)
        self.assertIn(
            '"LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT=${LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT:-true}"',
            text,
        )


if __name__ == "__main__":
    unittest.main()
