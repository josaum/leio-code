import importlib.util
import json
from pathlib import Path
import stat
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('evaluate', Path(__file__).resolve().parents[1] / 'scripts/evaluate_retrieval.py')
evaluate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluate)


class RetrievalMetricsTests(unittest.TestCase):
    def test_rank_uses_first_relevant_and_counts_misses_as_zero(self):
        self.assertEqual(evaluate.metrics(['noise','right','also'], ['right','also'])['reciprocal_rank'], .5)
        self.assertEqual(evaluate.metrics(['noise'], ['right'])['reciprocal_rank'], 0)
        self.assertFalse(evaluate.metrics([], ['right'])['hit_at_3'])
        self.assertTrue(evaluate.metrics(['right'], ['right'])['hit_at_1'])


class StrictContextTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()

    def fake_binary(self, stdout, *, stderr='', exit_code=0, delay=0):
        binary = self.root / 'fake-leio-code'
        binary.write_text(
            '#!/usr/bin/env python3\n'
            'import sys, time\n'
            f'sys.stdout.write({stdout!r})\n'
            'sys.stdout.flush()\n'
            f'sys.stderr.write({stderr!r})\n'
            'sys.stderr.flush()\n'
            f'time.sleep({delay!r})\n'
            f'raise SystemExit({exit_code})\n'
        )
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return binary

    def test_nonzero_context_exit_reports_bounded_stdout_and_stderr(self):
        binary = self.fake_binary('x' * 1500, stderr='backend exploded', exit_code=7)
        with self.assertRaises(RuntimeError) as raised:
            evaluate.run_context(binary, self.repo, 'task')
        message = str(raised.exception)
        self.assertIn('exited 7', message)
        self.assertIn("stdout='", message)
        self.assertIn('backend exploded', message)
        self.assertIn('…', message)
        self.assertLess(len(message), 2400)

    def test_context_timeout_reports_partial_diagnostics(self):
        binary = self.fake_binary('partial stdout', stderr='partial stderr', delay=2)
        with self.assertRaisesRegex(RuntimeError, 'timed out'):
            evaluate.run_context(binary, self.repo, 'task', timeout=0.05)
        # Python's TimeoutExpired may carry None output on fast timeouts; the
        # runner must then emit the empty diagnostic rather than crash.
        slow = self.fake_binary('partial stdout', stderr='partial stderr', delay=1)
        with self.assertRaisesRegex(RuntimeError, "stdout=''"):
            evaluate.run_context(slow, self.repo, 'task', timeout=0.02)

    def test_invalid_context_json_reports_stdout_and_stderr(self):
        binary = self.fake_binary('not-json', stderr='decoder context')
        with self.assertRaisesRegex(
            RuntimeError,
            'invalid JSON.*not-json.*decoder context',
        ):
            evaluate.run_context(binary, self.repo, 'task')

    def test_context_requires_first_bundle_files_to_read(self):
        for payload in [
            {'entities': []},
            {'entities': [{}]},
            {'entities': [{'files_to_read': None}]},
        ]:
            binary = self.fake_binary(json.dumps(payload))
            with self.assertRaisesRegex(RuntimeError, r'entities\[0\].*files_to_read'):
                evaluate.run_context(binary, self.repo, 'task')

    def test_context_rejects_malformed_file_rows(self):
        for rows in [[None], [{}], [{'path': 3}]]:
            payload = {'entities': [{'files_to_read': rows}], 'meta': {}}
            binary = self.fake_binary(json.dumps(payload))
            with self.assertRaisesRegex(RuntimeError, 'malformed.*files_to_read'):
                evaluate.run_context(binary, self.repo, 'task')

    def test_context_returns_paths_and_semantic_source(self):
        payload = {
            'entities': [{'files_to_read': [{'path': 'src/right.rs'}]}],
            'meta': {'semantic_source': 'on_demand'},
        }
        binary = self.fake_binary(json.dumps(payload))
        self.assertEqual(
            evaluate.run_context(binary, self.repo, 'task'),
            (['src/right.rs'], 'on_demand'),
        )

    def test_report_adds_per_task_semantic_source_and_aggregate_counts(self):
        relevant = self.repo / 'src' / 'right.rs'
        relevant.parent.mkdir()
        relevant.write_text('fn right() {}\n')
        payload = {
            'entities': [{'files_to_read': [{'path': 'src/right.rs'}]}],
            'meta': {'semantic_source': 'precomputed'},
        }
        binary = self.root / 'versioned-leio-code'
        binary.write_text(
            '#!/usr/bin/env python3\n'
            'import json, sys\n'
            'if "--version" in sys.argv:\n'
            '    print("leio-code test")\n'
            'else:\n'
            f'    print(json.dumps({payload!r}))\n'
        )
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)

        report = evaluate.evaluate(binary, self.repo, [{
            'task': 'private task',
            'relevant_paths': ['src/right.rs'],
        }])

        self.assertEqual(report['results'][0]['semantic_source'], 'precomputed')
        self.assertEqual(report['semantic_source_counts'], {'precomputed': 1})


if __name__ == '__main__':
    unittest.main()
