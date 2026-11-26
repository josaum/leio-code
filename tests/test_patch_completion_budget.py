import sys
import io
import json
import urllib.error
from unittest.mock import patch
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from benchmark_patch_completion import (
    action,
    apply_source_patch,
    parse_blocks,
    source_excerpts,
    solve,
    fit_action_history,
    daily_quota_block,
    provider_quota_block,
    inference_result,
    inference_request,
    parent_test_examples,
    PinnedEndpointRedirectHandler,
    view_files,
)


class PatchCompletionBudgetTests(unittest.TestCase):
    def test_parent_test_failure_reaches_next_action_and_can_be_repaired(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            source = root / "src/sample.py"
            source.write_text("value = 1\n")
            requests = [
                {"action": "replace", "path": "src/sample.py", "old": "value = 1", "new": "value = 2"},
                {"action": "replace", "path": "src/sample.py", "old": "value = 2", "new": "value = 3"},
                {"action": "done"},
            ]
            prompts = []

            def inference(messages, *_):
                prompts.append(messages[1]["content"])
                return json.dumps(requests[len(prompts) - 1]), {"tokens": 100}

            case = {"issue": "Change the value.", "test_file": "tests/test_sample.py"}
            config = {"inference": inference, "validate_parent_tests": True,
                      "max_seconds_per_task": 30, "max_tokens_per_task": 40000, "max_steps": 3}
            grades = [{"passed": False, "output": "Expected value 3"}, {"passed": True, "output": ""}]
            with patch("benchmark_patch_completion.run_tests", side_effect=grades):
                receipt = solve(root, case, config, "ordinary", None)

            self.assertIn("Expected value 3", prompts[1])
            self.assertNotIn("Expected value 3", prompts[2])
            self.assertEqual(receipt["stop"], "done")
            self.assertEqual(source.read_text(), "value = 3\n")

    def test_excerpt_exposes_aliased_helper_and_its_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "src/sample.py"
            (root / "src").mkdir()
            (root / path).write_text(
                "class Builder:\n"
                "    def __init__(self):\n"
                "        self.prepare = self.configure\n"
                "    def install(self):\n"
                "        self.callback = self.prepare(shared_callback)\n"
                + "\n" * 35
                + "    def configure(self, callback):\n"
                "        callback.__qualname__ = type(self).__name__\n"
                "        return callback\n"
            )
            log = [({"action": "read", "path": path}, "")]
            excerpt, _ = source_excerpts(view_files(root), log, {"shared_callback"})
            self.assertIn("self.prepare(shared_callback)", excerpt)
            self.assertIn("callback.__qualname__ =", excerpt)

    def test_drop_oldest_complete_turn_and_preserve_latest_read(self):
        messages = [
            {"role": role, "content": text}
            for role, text in [
                ("system", "policy"),
                ("user", "issue"),
                ("assistant", "old read"),
                ("user", "x" * 16000),
                ("assistant", "new read"),
                ("user", "source"),
            ]
        ]
        packet, budget = fit_action_history(messages, 10000)
        self.assertEqual(packet, messages[:2] + messages[-2:])
        self.assertEqual(budget, 1800)
        self.assertEqual(fit_action_history(messages, 40000)[0], messages)
        self.assertLess(fit_action_history(messages, 500)[1], 80)

    def test_requested_read_window_through_aliased_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "snapshot"
            root.mkdir()
            (root / "sample.py").write_text("first\nsecond\nthird\n")
            alias = Path(directory) / "alias"
            alias.symlink_to(root, target_is_directory=True)
            (root / "outside.py").symlink_to(Path(directory) / "outside.py")
            (Path(directory) / "outside.py").write_text("excluded")
            files = view_files(alias)
            self.assertEqual(list(files), ["sample.py"])
            request = {"action": "read", "path": "sample.py", "start": 2, "end": 2}
            self.assertEqual(action(request, files, set(), {}), "2: second")

    def test_patch_uses_current_inspected_lines_and_validates_every_edit_before_writing(
        self,
    ):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            (root / "src").mkdir()
            path = "src/sample.py"
            file = root / path
            before = (
                b"def first():\r\n    return 1\r\n\r\ndef last():\r\n    return 2\r\n"
            )
            file.write_bytes(before)
            files = view_files(root)
            req = {"action": "read", "path": path}
            log = [(req, action(req, files, set(), {}))]
            source, visible = source_excerpts(files, log, {"first", "last"})
            self.assertEqual(source.count("def first():"), 1)
            self.assertEqual(visible[path][2], "    return 1")
            edits = [
                {"old": "return 1", "new": "return 3"},
                {"old": "return 2", "new": "return 4"},
            ]
            request = {"action": "patch", "path": path, "edits": edits}
            original = {}
            with self.assertRaises(ValueError):
                apply_source_patch(
                    {**request, "edits": edits + [edits[0]]}, files, visible, original
                )
            with self.assertRaises(SyntaxError):
                apply_source_patch(
                    {**request, "edits": [dict(edits[0], new="return )")]},
                    files,
                    visible,
                    original,
                )
            self.assertEqual(file.read_bytes(), before)
            self.assertEqual(original, {})
            applied = apply_source_patch(request, files, visible, original)
            self.assertTrue(applied.startswith("Applied."))
            _, refreshed = source_excerpts(files, log, {"first", "last"})
            self.assertEqual(refreshed[path][2], "    return 3")
            self.assertEqual(refreshed[path][5], "    return 4")
            self.assertEqual(
                file.read_bytes(),
                before.replace(b"return 1", b"return 3").replace(
                    b"return 2", b"return 4"
                ),
            )
            self.assertEqual(original[path].encode(), before)
            with self.assertRaises(ValueError):
                apply_source_patch(request, files, visible, original)
            self.assertEqual(
                parse_blocks(
                    "<<< SEARCH src/sample.py\n    return 3\n===\n    return 5\n>>> END"
                )["edits"][0],
                {"old": "    return 3", "new": "    return 5"},
            )

    def test_parent_examples_follow_unusual_identifiers_and_existing_fixture_usage(
        self,
    ):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "test_known.py").write_text(
                "def test_io(runner):\n    assert runner.inspect_payload()\n"
                "def test_other():\n    assert True\n"
            )
            (root / "future.txt").write_text("future-only example")
            examples = parent_test_examples(
                view_files(root), "inspect_payload fails", []
            )
            self.assertIn("def test_io(runner)", examples)
            self.assertNotIn("test_other", examples)
            self.assertNotIn("future-only", examples)

    def test_direct_xai_request_and_response_use_the_pinned_model(self):
        config = {"transport": "xai", "model": "grok-4.7"}
        messages = [{"role": "user", "content": "return a JSON action"}]
        with patch.dict("os.environ", {"XAI_API_KEY": "test-key"}, clear=True):
            req = inference_request(config, messages, 800, json_action=True)
        self.assertEqual(req.full_url, "https://api.x.ai/v1/responses")
        body = json.loads(req.data)
        self.assertEqual(body["input"], messages)
        self.assertEqual(body["max_output_tokens"], 800)
        self.assertFalse(body["store"])
        self.assertNotIn("provider", body)
        response = {
            "model": "grok-4.7",
            "status": "completed",
            "usage": {
                "total_tokens": 42,
                "output_tokens": 21,
                "output_tokens_details": {"reasoning_tokens": 6},
            },
            "output": [
                {"type": "reasoning", "summary": []},
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "action"}],
                },
            ],
        }
        content, receipt = inference_result(response, config)
        self.assertEqual(
            (content, receipt["provider"], receipt["tokens"]), ("action", "xAI", 42)
        )
        with self.assertRaises(RuntimeError):
            inference_result({**response, "model": "another-model"}, config)

    def test_compatible_endpoint_is_pinned_and_requires_the_requested_model(self):
        base = "https://model.example/v1"
        config = {"transport": "openai-compatible", "model": "test-model",
                  "provider": "local", "endpoint": base + "/chat/completions"}
        with patch.dict("os.environ", {"BENCHMARK_API_BASE_URL": base, "BENCHMARK_API_KEY": "fixture"}, clear=True):
            request = inference_request(config, [], 250, json_action=True)
            body = json.loads(request.data)
            self.assertEqual(request.full_url, config["endpoint"])
            self.assertEqual(body["max_tokens"], 250)
            self.assertEqual(body["response_format"], {"type": "json_object"})
            self.assertNotIn("provider", body)
            with self.assertRaises(ValueError):
                inference_request({**config, "endpoint": "https://other.example"}, [], 250, json_action=False)
        with self.assertRaises(urllib.error.HTTPError):
            PinnedEndpointRedirectHandler().redirect_request(request, None, 302, "", {}, "https://other.example")
        response = {"model": "test-model", "choices": [{"message": {"content": "ok"}}],
                    "usage": {"total_tokens": 15, "completion_tokens": 1}}
        self.assertEqual(inference_result(response, config)[1]["tokens"], 15)
        with self.assertRaises(RuntimeError):
            inference_result({**response, "model": "fallback"}, config)

    def test_xai_billing_block_is_unscored_and_excludes_team_details(self):
        def denied(message):
            body = json.dumps({"code": "permission-denied", "error": message}).encode()
            return urllib.error.HTTPError("https://example.invalid", 403, "denied", {}, io.BytesIO(body))

        with denied("Your team abc has used all available credits.") as error:
            self.assertEqual(provider_quota_block(error, "xai"), {
                "http_status": 403, "reason": "team_credit_or_spending_limit"
            })
        with denied("Your team abc cannot access this model.") as error:
            self.assertIsNone(provider_quota_block(error, "xai"))

    def test_daily_quota_has_a_reset_and_transient_throttles_remain_retryable(self):
        def limited(message):
            return urllib.error.HTTPError(
                "https://example.invalid",
                429,
                "limited",
                {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": "1795651200000"},
                io.BytesIO(json.dumps({"error": {"message": message}}).encode()),
            )

        with limited("free-models-per-day-high-balance") as error:
            block = daily_quota_block(error)
        self.assertEqual(block["resets_at"], "2026-11-26T00:00:00+00:00")
        with limited("per-minute limit") as error:
            self.assertIsNone(daily_quota_block(error))
