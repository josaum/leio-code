"""Self-contained navigation examples; no external repository or framework."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

BINARY = Path(os.environ.get('LEIO_CODE_BIN', Path.home() / '.cargo/bin/leio-code'))


class NavigationExamplesTests(unittest.TestCase):
    def test_retry_and_revision_are_separate_navigation_targets(self):
        self.check_example(
            'src/workflow.rs',
            '''pub struct Workflow { pub input_version: u64, pub approved: bool }
impl Workflow {
    pub fn retry_execution(&self) -> u64 { self.input_version }
    pub fn revise_inputs(&mut self) { self.input_version += 1; self.approved = false; }
}
''',
            'retry execution versus revise inputs in workflow',
            ['retry_execution', 'revise_inputs'],
        )

    def test_verification_and_rollback_are_separate_navigation_targets(self):
        self.check_example(
            'src/delivery.rs',
            '''pub fn verify_revision(expected: &str, running: &str) -> bool { expected == running }
pub fn rollback_revision(previous: &str) -> String { previous.to_owned() }
''',
            'delivery verify revision and rollback revision',
            ['verify_revision', 'rollback_revision'],
        )

    def check_example(self, source_path, source, task, expected_symbols):
        self.assertTrue(BINARY.is_file(), f'Build LEIO or set LEIO_CODE_BIN: {BINARY}')
        with tempfile.TemporaryDirectory(prefix='leio-navigation-example-') as temporary:
            root = Path(temporary)
            (root / 'Cargo.toml').write_text('[package]\nname="navigation-example"\nversion="0.1.0"\nedition="2021"\n')
            (root / source_path).parent.mkdir(parents=True)
            (root / source_path).write_text(source)
            (root / 'src/unrelated.rs').write_text('pub fn format_receipt() {}\n')

            def run(*args):
                process = subprocess.run([str(BINARY), '--json', '--repo', str(root), *args],
                                         capture_output=True, text=True, timeout=60, check=True)
                return json.loads(process.stdout)

            run('index')
            bundle = run('context', task, '--limit', '3')['entities'][0]
            self.assertEqual(bundle['files_to_read'][0]['path'], source_path)
            graph = run('graph', 'symbols-in', source_path)
            names = {row.get('name') for row in graph['entities']}
            self.assertTrue(set(expected_symbols) <= names, names)
