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

    def test_home_can_enable_settings_without_expanding_history_collection(self):
        (self.origin / 'session.jsonl').write_text('history')
        self.collect()
        before = collect.collection_paths(self.source)
        self.source.update(home=str(self.base), collect_home=False)
        self.assertEqual(collect.collection_paths(self.source), before)
        self.collect()
        metadata = json.loads((self.root / 'laptop/source.json').read_text())
        self.assertEqual(metadata['origin']['home'], str(self.base))
        self.assertEqual(len(metadata['mounts']), len(before))
        self.source['collect_home'] = 'false'
        with self.assertRaises(ValueError):
            collect.collection_paths(self.source)

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
        with self.assertRaises((ValueError, subprocess.CalledProcessError)):
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


class ProviderCollectionTests(unittest.TestCase):
    setUp = CollectorTests.setUp
    execute = CollectorTests.execute
    collect = CollectorTests.collect

    def configure(self):
        self.source = {'id': 'laptop', 'label': 'Laptop', 'home': str(self.origin),
                       'project_roots': [{'path': str(self.origin / 'arbitrary-code-root'),
                                          'mirror_path': 'projects/team'}]}
        for path in ['.claude/projects/a/session.jsonl', '.codex/sessions/one.jsonl',
                     '.codex/archived_sessions/two.jsonl', '.local/share/opencode/storage/project/p.json',
                     'arbitrary-code-root/deep/team/repo/.aider.chat.history.md',
                     'arbitrary-code-root/deep/team/repo/.crush/crush.db',
                     'arbitrary-code-root/deep/team/repo/.crush/crush.db-wal',
                     'arbitrary-code-root/deep/team/repo/source.py', 'private.txt']:
            file = self.origin / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text(path)

    def test_automatic_providers_project_filters_and_inventory(self):
        self.configure()
        extra = self.base / 'extra.jsonl'
        extra.write_text('extra')
        self.source['paths'] = [{'path': str(extra), 'mirror_path': 'manual/extra.jsonl', 'kind': 'file'}]
        self.collect()
        current = self.root / 'laptop/current'
        for path in ['.claude/projects/a/session.jsonl', '.codex/sessions/one.jsonl',
                     '.codex/archived_sessions/two.jsonl', '.local/share/opencode/storage/project/p.json',
                     'projects/team/deep/team/repo/.aider.chat.history.md',
                     'projects/team/deep/team/repo/.crush/crush.db',
                     'projects/team/deep/team/repo/.crush/crush.db-wal', 'manual/extra.jsonl']:
            self.assertTrue((current / path).is_file(), path)
        self.assertFalse((current / 'private.txt').exists())
        self.assertFalse((current / 'projects/team/deep/team/repo/source.py').exists())
        metadata = json.loads((current.parent / 'source.json').read_text())
        self.assertEqual(metadata['origin']['home'], str(self.origin))
        self.assertTrue(metadata['last_collected_at'])
        mounts = {m['mirror_path']: m for m in metadata['mounts']}
        self.assertEqual(mounts['.codex']['providers'], ['codex'])
        self.assertEqual(mounts['projects/team']['providers'], ['aider', 'crush'])
        self.assertEqual(mounts['manual/extra.jsonl']['kind'], 'file')
        for command in self.commands:
            self.assertFalse(any(a.startswith(('--delete', '--remove-source', '--inplace', '--copy-links')) for a in command))

    def test_local_and_ssh_have_identical_layout_and_metadata(self):
        self.configure()
        self.collect()
        local = self.root / 'laptop/current'
        local_files = {str(p.relative_to(local)): p.read_bytes() for p in local.rglob('*') if p.is_file()}
        self.source.update(id='remote', ssh='fixture-host')
        # Execute real rsync with a fake SSH transport, leaving transfer options observable.
        def transfer(args):
            self.commands.append(args)
            if args[0] == 'restic':
                return
            args = list(args)
            if '-e' in args:
                i = args.index('-e')
                del args[i:i+2]
                args.remove('--protect-args')
                i = args.index('--') + 1
                args[i] = args[i].removeprefix('fixture-host:')
            subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
        with patch.object(collect, 'run', transfer), patch.object(collect, 'origin_exists',
                side_effect=lambda source, path, kind='directory': Path(path).exists()):
            collect.collect_source(self.root, self.source)
        remote = self.root / 'remote/current'
        self.assertEqual(local_files, {str(p.relative_to(remote)): p.read_bytes() for p in remote.rglob('*') if p.is_file()})
        self.assertTrue(any('--protect-args' in c for c in self.commands))
        self.assertEqual(json.loads((remote.parent / 'source.json').read_text())['origin']['ssh'], 'fixture-host')
        metadata = (remote.parent / 'source.json').read_bytes()
        with patch.object(collect, 'origin_exists', side_effect=subprocess.CalledProcessError(255, 'ssh')):
            with self.assertRaises(subprocess.CalledProcessError):
                collect.collect_source(self.root, self.source)
        self.assertEqual((remote.parent / 'source.json').read_bytes(), metadata)
        self.assertEqual(local_files, {str(p.relative_to(remote)): p.read_bytes() for p in remote.rglob('*') if p.is_file()})

    def test_offline_ssh_is_not_optional_absence(self):
        self.configure()
        self.source['ssh'] = 'fixture-host'
        with patch.object(collect.subprocess, 'run', return_value=subprocess.CompletedProcess([], 255)):
            with self.assertRaises(subprocess.CalledProcessError):
                collect.origin_exists(self.source, '/home/test/.codex')
        with patch.object(collect.subprocess, 'run', return_value=subprocess.CompletedProcess([], 44)):
            self.assertFalse(collect.origin_exists(self.source, '/home/test/.codex'))

    def test_ssh_probe_script_handles_quoted_paths_and_symlink_ancestors(self):
        self.configure()
        self.source['ssh'] = 'fixture-host'
        quoted = self.origin / "space and 'quote'"
        quoted.mkdir()
        actual_run = subprocess.run
        def shell_probe(args, **kwargs):
            self.assertEqual(args[0], 'ssh')
            return actual_run(['sh', '-c', args[-1]], **kwargs)
        with patch.object(collect.subprocess, 'run', shell_probe):
            self.assertTrue(collect.origin_exists(self.source, str(quoted)))
            self.assertFalse(collect.origin_exists(self.source, str(quoted / 'absent')))
            outside = self.base / 'outside-probe'
            outside.mkdir()
            (outside / 'child').mkdir()
            (self.origin / 'link').symlink_to(outside, target_is_directory=True)
            with self.assertRaises(subprocess.CalledProcessError):
                collect.origin_exists(self.source, str(self.origin / 'link/child'))

    def test_optional_deletion_retains_history_and_failure_retains_timestamp(self):
        self.configure()
        self.collect()
        current = self.root / 'laptop/current'
        shutil.rmtree(self.origin / '.codex')
        self.collect()
        self.assertTrue((current / '.codex/sessions/one.jsonl').exists())
        metadata = (current.parent / 'source.json').read_bytes()
        self.origin.rename(self.base / 'offline')
        with self.assertRaises(ValueError):
            self.collect()
        self.assertEqual((current.parent / 'source.json').read_bytes(), metadata)
        self.assertTrue((current / '.codex/sessions/one.jsonl').exists())

    def test_invalid_overlapping_and_home_roots_rejected(self):
        self.configure()
        config = {'mirror_root': str(self.root), 'sources': [self.source]}
        for bad in ['../escape', '/absolute', 'a/../escape', 'a/./b', 'a\\b', '']:
            self.source['project_roots'][0]['mirror_path'] = bad
            with self.assertRaises(ValueError, msg=bad):
                collect.validate(config)
        self.source['project_roots'] = [{'path': str(self.origin), 'mirror_path': 'all-home'}]
        with self.assertRaises(ValueError):
            collect.validate(config)
        self.source['project_roots'] = []
        self.source['paths'] = [{'path': '/other', 'mirror_path': '.codex/sessions'}]
        with self.assertRaises(ValueError):
            collect.validate(config)
        self.source['paths'] = [{'path': '/other/../escape', 'mirror_path': 'extra'}]
        with self.assertRaises(ValueError):
            collect.validate(config)

    def test_legacy_identity_upgrades_additively_but_cannot_move_home(self):
        (self.origin / 'session').write_text('old')
        self.collect()
        # This old mount mapped an unrelated origin to .claude/projects: adopting
        # a different .claude origin must be rejected, not silently relabeled.
        self.source['home'] = str(self.base)
        with self.assertRaises(ValueError):
            self.collect()

    def test_matching_legacy_mount_can_enable_provider_collection(self):
        self.configure()
        home_source = dict(self.source)
        self.source = {'id': 'laptop', 'label': 'Laptop', 'paths': [
            {'path': str(self.origin / '.claude'), 'mirror_path': '.claude'}]}
        self.collect()
        self.source.update(home_source)
        self.collect()
        current = self.root / 'laptop/current'
        self.assertTrue((current / '.codex/sessions/one.jsonl').is_file())
        self.assertTrue((current / '.claude/projects/a/session.jsonl').is_file())
        self.source['home'] = str(self.base / 'other-home')
        with self.assertRaises(ValueError):
            self.collect()

    def test_manual_overlaps_and_full_home_copy_are_rejected(self):
        self.configure()
        for paths in [
            [{'path': '/x', 'mirror_path': 'extra'}, {'path': '/x/y', 'mirror_path': 'extra/y'}],
            [{'path': str(self.origin), 'mirror_path': 'whole-home'}],
        ]:
            self.source['paths'] = paths
            with self.assertRaises(ValueError):
                self.collect()

    def test_post_transfer_backup_failure_does_not_promote(self):
        self.configure()
        self.collect()
        current = self.root / 'laptop/current'
        metadata = (current.parent / 'source.json').read_bytes()
        file = self.origin / '.codex/sessions/one.jsonl'
        old = file.read_text()
        file.write_text('new capture')
        stage_backups = 0
        def fail_final_backup(args):
            nonlocal stage_backups
            if args[0] == 'restic' and args[-1].endswith('/incoming'):
                stage_backups += 1
                if stage_backups == 2:
                    raise subprocess.CalledProcessError(3, args)
            self.execute(args)
        with patch.object(collect, 'run', fail_final_backup):
            with self.assertRaises(subprocess.CalledProcessError):
                collect.collect_source(self.root, self.source)
        self.assertEqual((current / '.codex/sessions/one.jsonl').read_text(), old)
        self.assertEqual((current.parent / 'source.json').read_bytes(), metadata)
        self.assertEqual((current.parent / 'incoming/.codex/sessions/one.jsonl').read_text(), 'new capture')

    def test_provider_symlink_escape_is_rejected(self):
        self.configure()
        shutil.rmtree(self.origin / '.codex')
        outside = self.base / 'outside'
        outside.mkdir()
        (self.origin / '.codex').symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.collect()


class ProviderSettingsCollectionTests(unittest.TestCase):
    setUp = CollectorTests.setUp
    execute = CollectorTests.execute
    collect = CollectorTests.collect

    def test_configuration_is_excluded_before_history_backup(self):
        (self.origin / 'settings.json').write_text('{"apiKey":"must-not-retain"}')
        (self.origin / 'config.toml').write_text('token = "must-not-retain"')
        (self.origin / '.settings.json.cchv-interrupted').write_text('temporary-secret')
        (self.origin / 'settings.json.bak').write_text('backup-secret')
        (self.origin / '.settings.json.swp').write_text('swap-secret')
        (self.origin / 'session.jsonl').write_text('history')
        self.collect()
        current = self.root / 'laptop/current/.claude/projects'
        self.assertTrue((current / 'session.jsonl').exists())
        self.assertFalse((current / 'settings.json').exists())
        self.assertFalse((current / 'config.toml').exists())
        self.assertFalse((current / '.settings.json.cchv-interrupted').exists())
        self.assertFalse((current / 'settings.json.bak').exists())
        self.assertFalse((current / '.settings.json.swp').exists())
        self.assertTrue(any('--exclude=settings.json' in args for args in self.commands))

    def test_preexisting_configuration_blocks_restic_without_deleting_history(self):
        current = self.root / 'laptop/current'
        current.mkdir(parents=True)
        (current / 'settings.json').write_text('{"password":"old-secret"}')
        with self.assertRaisesRegex(ValueError, 'Existing mirror contains provider configuration'):
            self.collect()
        self.assertTrue((current / 'settings.json').exists())
        self.assertFalse(any(args[0] == 'restic' for args in self.commands))

    def test_write_capability_defaults_false_and_can_be_revoked_offline(self):
        self.collect()
        metadata = self.root / 'laptop/source.json'
        self.assertFalse(json.loads(metadata.read_text())['allow_settings_write'])
        self.source['allow_settings_write'] = True
        self.collect()
        self.assertTrue(json.loads(metadata.read_text())['allow_settings_write'])
        timestamp = json.loads(metadata.read_text())['last_collected_at']
        self.source['allow_settings_write'] = False
        self.origin.rmdir()
        with self.assertRaises(ValueError):
            self.collect()
        self.assertFalse(json.loads(metadata.read_text())['allow_settings_write'])
        self.assertEqual(json.loads(metadata.read_text())['last_collected_at'], timestamp)

    def test_renamed_configuration_and_snapshot_mounts_are_rejected(self):
        for origin_name, kind in [('settings.json', 'file'), ('settings-snapshots', 'directory')]:
            target = self.origin / origin_name
            if kind == 'file':
                target.write_text('{"token":"do-not-copy"}')
            else:
                target.mkdir()
                (target / 'cached.json').write_text('{}')
            self.source['paths'] = [{'path': str(target), 'mirror_path': 'renamed-history', 'kind': kind}]
            with self.assertRaisesRegex(ValueError, 'must not be collected'):
                self.collect()
            self.assertFalse(any(args[0] == 'restic' for args in self.commands))

    def test_write_capability_requires_boolean(self):
        self.source['allow_settings_write'] = 'true'
        with self.assertRaises(ValueError):
            collect.validate({'mirror_root': str(self.root), 'sources': [self.source]})


if __name__ == '__main__':
    unittest.main()
