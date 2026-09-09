.PHONY: install verify coverage package-plugin package-mcpb harness-verify

CARGO_TARGET_DIR ?= target
DEST_DIR ?= $(HOME)/.cargo/bin

install:
	@set -euo pipefail; \
		code="$(CARGO_TARGET_DIR)/release/leio-code"; \
		harness="$(CARGO_TARGET_DIR)/release/leio-harness"; \
		echo "==> cargo build --release -p leio-code -p leio-harness"; \
		cargo build --release -p leio-code -p leio-harness; \
		mkdir -p "$(DEST_DIR)"; \
		install -m 0755 "$$code" "$(DEST_DIR)/leio-code"; \
		install -m 0755 "$$harness" "$(DEST_DIR)/leio-harness"; \
		echo "ok: installed $(DEST_DIR)/leio-code"; \
		echo "ok: installed $(DEST_DIR)/leio-harness"; \
		"$(DEST_DIR)/leio-code" --help >/dev/null; \
		"$(DEST_DIR)/leio-harness" --help >/dev/null

verify:
	cargo test --locked --lib
	cargo test --locked -p leio-harness
	node --test mcp/resolve-binary.test.js mcp/export-paths.test.js mcp/watch-state.test.js mcp/service-descriptor.test.js
	python3 tests/test_apps_sdk_start_local.py
	python3 tests/test_benchmark_models.py

coverage:
	python3 -m coverage run --include="*/scripts/benchmark_models.py" -m pytest tests/test_benchmark_models.py -q
	python3 -m coverage report -m

harness-verify:
	bash scripts/harness-verify.sh

package-plugin:
	python3 scripts/package_codex_plugin.py $(if $(CLEAN_OUTPUT),--clean,)

package-mcpb:
	bash scripts/package_desktop_mcpb.sh
