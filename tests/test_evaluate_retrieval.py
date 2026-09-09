import importlib.util
from pathlib import Path
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
