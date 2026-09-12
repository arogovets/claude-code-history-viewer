import json
import os
import shutil
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import subprocess

import collect


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.origin = self.base / 'origin'
        self.origin.mkdir()
        self.root = self.base / 'mirrors'
        self.source = {'id': 'laptop', 'label': 'Laptop', 'paths': [
            {'path': str(self.origin), 'mirror_path': '.claude/projects'}]}
        self.commands = []

    def execute(self, args):
        self.commands.append(args)
        if args[0] != 'restic':
            subprocess.run(args, check=True, stdout=subprocess.DEVNULL)

    def collect(self):
        with patch.object(collect, 'run', self.execute):
            collect.collect_source(self.root, self.source)

    def test_growth_and_deletion_keep_mirror_and_backup_before_write(self):
        log = self.origin / 'session.jsonl'
        log.write_text('first\n')
        self.collect()
        log.write_text('first\nsecond\n')
        self.commands.clear()
        self.collect()
        current = self.root / 'laptop/current/.claude/projects/session.jsonl'
        self.assertEqual(current.read_text(), 'first\nsecond\n')
        self.assertEqual(self.commands[0][0:2], ['restic', 'backup'])
        log.unlink()
        self.collect()
        self.assertEqual(current.read_text(), 'first\nsecond\n')
        self.assertFalse(any('--delete' in a for cmd in self.commands for a in cmd))

    def test_backup_failure_blocks_promotion(self):
        (self.origin / 'session').write_text('old')
        self.collect()
        (self.origin / 'session').write_text('new')
        with patch.object(collect, 'run', side_effect=subprocess.CalledProcessError(3, 'restic')):
            with self.assertRaises(subprocess.CalledProcessError):
                collect.collect_source(self.root, self.source)
        self.assertEqual((self.root / 'laptop/current/.claude/projects/session').read_text(), 'old')

    def test_offline_pull_leaves_current(self):
        (self.origin / 'session').write_text('old')
        self.collect()
        (self.origin / 'session').unlink()
        self.origin.rmdir()
        with self.assertRaises(subprocess.CalledProcessError):
            self.collect()
        self.assertEqual((self.root / 'laptop/current/.claude/projects/session').read_text(), 'old')

    def test_individual_file_is_mirrored(self):
        log = self.origin / 'session.jsonl'
        log.write_text('captured')
        self.source['paths'] = [{'path': str(log), 'mirror_path': '.claude/projects/session.jsonl', 'kind': 'file'}]
        self.collect()
        self.assertEqual((self.root / 'laptop/current/.claude/projects/session.jsonl').read_text(), 'captured')

    def test_ids_and_overlap(self):
        config = {'mirror_root': str(self.root), 'sources': [self.source, self.source]}
        with self.assertRaises(ValueError):
            collect.validate(config)
        config['sources'] = [self.source]
        self.source['paths'][0]['mirror_path'] = '../escape'
        with self.assertRaises(ValueError):
            collect.validate(config)

    def test_symlink_rejected(self):
        self.root.mkdir()
        (self.root / 'laptop').symlink_to(self.origin, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.collect()


    @unittest.skipUnless(shutil.which('restic'), 'Restic integration dependency is unavailable')
    def test_real_restic_restores_both_versions(self):
        repository = self.base / 'restic'
        env = {'RESTIC_REPOSITORY': str(repository), 'RESTIC_PASSWORD': 'temporary-test-repository'}
        def real_run(args):
            subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
        with patch.dict(os.environ, env), patch.object(collect, 'run', real_run):
            real_run(['restic', 'init'])
            log = self.origin / 'session.jsonl'
            log.write_text('version-one\n')
            collect.collect_source(self.root, self.source)
            log.write_text('version-two\n')
            collect.collect_source(self.root, self.source)
            snapshots = json.loads(subprocess.check_output(['restic', 'snapshots', '--json']))
            restored = set()
            for snapshot in snapshots:
                path = snapshot['paths'][0] + '/.claude/projects/session.jsonl'
                restored.add(subprocess.check_output(['restic', 'dump', snapshot['id'], path]).decode())
            self.assertEqual(restored, {'version-one\n', 'version-two\n'})


if __name__ == '__main__':
    unittest.main()
