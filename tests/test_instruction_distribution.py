"""The packaged hook must inject the canonical task-first instructions."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class InstructionDistributionTests(unittest.TestCase):
    def test_portable_payloads_include_hook_and_skill(self):
        payloads = []
        for name in ['package_codex_plugin', 'install_codex_plugin']:
            spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / f'{name}.py')
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            self.assertIn('hooks', module.INCLUDED_PATHS)
            self.assertIn('skills', module.INCLUDED_PATHS)
            payloads.append(set(module.INCLUDED_PATHS))
        self.assertEqual(payloads[0], payloads[1])

    def test_hook_from_foreign_cwd_emits_canonical_skill(self):
        env = {**os.environ, 'CLAUDE_PLUGIN_ROOT': str(ROOT)}
        env.pop('LEIO_CODE_SKILL', None)
        with tempfile.TemporaryDirectory() as cwd:
            result = subprocess.run(['bash', str(ROOT / 'hooks/session-context.sh')], cwd=cwd,
                                    env=env, capture_output=True, text=True, check=True)
        expected = (ROOT / 'skills/leio-code/SKILL.md').read_text()
        if expected.startswith('---'):
            expected = expected.split('---', 2)[2].lstrip('\n')
        self.assertEqual(result.stdout, expected)
        self.assertIn('Start with `leio_code_context(task, repo_root)`', result.stdout)
        self.assertNotIn('1. `status`', result.stdout)
