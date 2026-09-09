from __future__ import annotations

import json
import struct
import tempfile
import unittest
import zlib
from pathlib import Path


LEIO_ROOT = Path(__file__).resolve().parents[1]
ASSETS = LEIO_ROOT / "assets"


class ChatGptIconAssetTests(unittest.TestCase):
    def assert_png_path_contract(
        self, path: Path, size: tuple[int, int]
    ) -> None:
        self.assertTrue(path.is_file(), f"missing asset: {path}")
        payload = path.read_bytes()
        self.assertEqual(payload[:8], b"\x89PNG\r\n\x1a\n")
        offset = 8
        chunks: list[bytes] = []

        while offset < len(payload):
            self.assertGreaterEqual(
                len(payload) - offset,
                12,
                "truncated PNG chunk header",
            )
            length = struct.unpack(">I", payload[offset : offset + 4])[0]
            chunk_end = offset + 12 + length
            self.assertLessEqual(chunk_end, len(payload), "truncated PNG chunk")
            chunk_type = payload[offset + 4 : offset + 8]
            chunk_data = payload[offset + 8 : offset + 8 + length]
            stored_crc = struct.unpack(">I", payload[offset + 8 + length : chunk_end])[0]
            computed_crc = zlib.crc32(chunk_type + chunk_data) & 0xFFFFFFFF
            self.assertEqual(stored_crc, computed_crc, f"bad {chunk_type!r} CRC")
            chunks.append(chunk_type)
            offset = chunk_end
            if chunk_type == b"IEND":
                self.assertEqual(length, 0)
                break

        self.assertTrue(chunks)
        self.assertEqual(chunks[0], b"IHDR")
        self.assertEqual(chunks[-1], b"IEND")
        self.assertEqual(offset, len(payload), "unexpected bytes after IEND")
        self.assertEqual(struct.unpack(">I", payload[8:12])[0], 13)
        self.assertEqual(struct.unpack(">II", payload[16:24]), size)
        self.assertEqual(payload[24], 8, "expected 8-bit PNG channels")
        self.assertEqual(payload[25], 2, "expected opaque RGB PNG")

    def assert_png_contract(self, filename: str, size: tuple[int, int]) -> None:
        self.assert_png_path_contract(ASSETS / filename, size)

    def test_directory_icon_contract(self) -> None:
        self.assert_png_contract("directory-icon.png", (1024, 1024))

    def test_composer_icon_contract(self) -> None:
        self.assert_png_contract("composer-icon.png", (256, 256))

    def test_manifest_uses_new_composer_icon(self) -> None:
        manifest = json.loads(
            (LEIO_ROOT / ".codex-plugin" / "plugin.json").read_text()
        )
        self.assertEqual(
            manifest["interface"]["composerIcon"],
            "./assets/composer-icon.png",
        )

    def test_png_contract_rejects_truncated_payload(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "truncated.png"
            path.write_bytes((ASSETS / "directory-icon.png").read_bytes()[:26])
            with self.assertRaises(AssertionError):
                self.assert_png_path_contract(path, (1024, 1024))

    def test_png_contract_rejects_bad_crc(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bad-crc.png"
            payload = bytearray((ASSETS / "directory-icon.png").read_bytes())
            payload[29] ^= 0x01
            path.write_bytes(payload)
            with self.assertRaises(AssertionError):
                self.assert_png_path_contract(path, (1024, 1024))


if __name__ == "__main__":
    unittest.main()
