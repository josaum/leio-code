import importlib.util
import unittest
from collections import Counter
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "doctor_warning_ledger.py"
SPEC = importlib.util.spec_from_file_location("doctor_warning_ledger", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
doctor_warning_ledger = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(doctor_warning_ledger)


class DoctorWarningLedgerTests(unittest.TestCase):
    def test_warning_family_extracts_doctor_prefix(self) -> None:
        self.assertEqual(
            doctor_warning_ledger.warning_family("[redis-key-hygiene] Redis SET has no TTL"),
            "redis-key-hygiene",
        )
        self.assertEqual(doctor_warning_ledger.warning_family("plain warning"), "unclassified")

    def test_validate_ledger_requires_current_families_and_fields(self) -> None:
        ledger = {
            "families": {
                "redis-key-hygiene": {
                    "owner": "api",
                    "severity": "low",
                    "status": "fix_now",
                    "block_later": True,
                    "next_action": "add TTL",
                    "last_seen_warning_count": 1,
                }
            }
        }
        result = doctor_warning_ledger.validate_ledger(
            ledger,
            Counter({"redis-key-hygiene": 2}),
        )

        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["count_drift"]["redis-key-hygiene"]["current_warning_count"], 2)

    def test_validate_ledger_rejects_missing_family(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "missing_current"):
            doctor_warning_ledger.validate_ledger(
                {"families": {}},
                Counter({"redis-key-hygiene": 1}),
            )

    def test_validate_ledger_rejects_stale_family(self) -> None:
        ledger = {
            "families": {
                "redis-key-hygiene": {
                    "owner": "api",
                    "severity": "low",
                    "status": "fixed",
                    "block_later": True,
                    "next_action": "remove stale entry",
                    "last_seen_warning_count": 1,
                }
            }
        }
        with self.assertRaisesRegex(RuntimeError, "stale families"):
            doctor_warning_ledger.validate_ledger(ledger, Counter())

    def _ledger(self, baseline: int) -> dict:
        return {
            "families": {
                "secret-set-parity": {
                    "owner": "platform",
                    "severity": "low",
                    "status": "known_env_gap",
                    "block_later": False,
                    "next_action": "provide .local secrets in deploy env",
                    "last_seen_warning_count": baseline,
                }
            }
        }

    def test_enforce_counts_allows_debt_at_baseline(self) -> None:
        # current == baseline: no regression.
        grown = doctor_warning_ledger.enforce_counts(
            self._ledger(8), Counter({"secret-set-parity": 8})
        )
        self.assertEqual(grown, {})

    def test_enforce_counts_allows_debt_to_shrink(self) -> None:
        # current < baseline: debt may shrink freely.
        grown = doctor_warning_ledger.enforce_counts(
            self._ledger(8), Counter({"secret-set-parity": 3})
        )
        self.assertEqual(grown, {})

    def test_enforce_counts_flags_growth(self) -> None:
        # current > baseline: the ratchet bites.
        grown = doctor_warning_ledger.enforce_counts(
            self._ledger(8), Counter({"secret-set-parity": 9})
        )
        self.assertEqual(grown, {"secret-set-parity": {"baseline": 8, "current": 9}})

    def test_enforce_counts_flags_unledgered_family_as_growth_from_zero(self) -> None:
        # a family absent from the ledger has baseline 0; any count is growth.
        grown = doctor_warning_ledger.enforce_counts(
            self._ledger(8), Counter({"secret-set-parity": 8, "brand-new": 1})
        )
        self.assertEqual(grown, {"brand-new": {"baseline": 0, "current": 1}})


if __name__ == "__main__":
    unittest.main()
