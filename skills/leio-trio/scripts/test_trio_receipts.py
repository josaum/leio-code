import unittest

import trio_receipts


class ReceiptParsingTests(unittest.TestCase):
    def test_query_ids_keep_dotted_and_hyphenated_kinds(self):
        dotted = "doctor.flight-server-zero-copy-1788477534952084000"
        underscored = "find_symbol-1788478894304497000"
        self.assertEqual(
            trio_receipts.QUERY_ID_RE.findall(f"`{dotted}`, `{underscored}`"),
            [dotted, underscored],
        )

    def test_non_hex_reference_id_is_malformed(self):
        full, abbreviated, malformed = trio_receipts.split_reference_ids(
            "receipt res:not-a-real-id",
            trio_receipts.RES_RE,
            64,
        )
        self.assertEqual(full, set())
        self.assertEqual(abbreviated, [])
        self.assertEqual(malformed, ["res:not-a-real-id"])

    def test_hex_ellipsis_is_an_informational_abbreviation(self):
        full, abbreviated, malformed = trio_receipts.split_reference_ids(
            "receipt res:abc…",
            trio_receipts.RES_RE,
            64,
        )
        self.assertEqual(full, set())
        self.assertEqual(abbreviated, ["res:abc"])
        self.assertEqual(malformed, [])


if __name__ == "__main__":
    unittest.main()
