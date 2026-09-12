#!/usr/bin/env python3
"""Pull history into local mirrors; Restic owns retained versions, not CCHV."""
import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


def run(args):
    subprocess.run(args, check=True)


def safe_tree(root):
    if root.is_symlink():
        raise ValueError(f"Mirror root must not be a link: {root}")
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            path = Path(parent) / name
            if path.is_symlink() or not (path.is_dir() or path.is_file()):
                raise ValueError(f"Mirror contains a link or special file: {path}")


def validate(config):
    root = Path(config['mirror_root']).expanduser()
    if not root.is_absolute():
        raise ValueError('mirror_root must be absolute')
    seen = set()
    for source in config['sources']:
        sid = source['id']
        if not re.fullmatch(r'[a-z0-9][a-z0-9_-]{0,63}', sid) or sid in seen:
            raise ValueError(f'Invalid or duplicate source id: {sid}')
        seen.add(sid)
        if not source.get('label', '').strip():
            raise ValueError('Each source requires a label')
        if source.get('ssh') and not re.fullmatch(r'[a-zA-Z0-9_][a-zA-Z0-9_.@-]*', source['ssh']):
            raise ValueError('ssh must be an SSH config alias or user@host')
        mounts = set()
        for item in source['paths']:
            if item.get('kind', 'directory') not in ('directory', 'file'):
                raise ValueError('kind must be directory or file')
            relative = Path(item['mirror_path'])
            if relative.is_absolute() or '..' in relative.parts or not relative.parts:
                raise ValueError('mirror_path must be a relative path without traversal')
            if any(relative == p or relative in p.parents or p in relative.parents for p in mounts):
                raise ValueError('Overlapping mirror paths')
            mounts.add(relative)
            if not item['path'].startswith('/') or '\n' in item['path']:
                raise ValueError('Source path must be absolute')
            if not source.get('ssh'):
                origin = Path(item['path']).resolve()
                target = root.resolve()
                if origin == target or origin in target.parents or target in origin.parents:
                    raise ValueError('Source and mirror roots must not overlap')
    return root


def backup(path, sid):
    # A nonzero result, including Restic's incomplete-backup code 3, blocks writes.
    run(['restic', 'backup', '--tag', 'cchv-source:' + sid, '--', str(path)])


def collect_source(root, source):
    sid = source['id']
    directory = root / sid
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.is_symlink():
        raise ValueError('Source directory must not be a symlink')
    with (directory / '.collector.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        identity = directory / 'source.json'
        immutable = {'id': sid, 'ssh': source.get('ssh'), 'paths': source['paths']}
        if identity.exists():
            previous = json.loads(identity.read_text())
            if previous['origin'] != immutable:
                raise ValueError('Source identity changed; assign a new id for a different origin')
        # Pin origin before the first capture, including a failed initial pull.
        if not identity.exists():
            temporary = directory / 'source.json.tmp'
            temporary.write_text(json.dumps({'id': sid, 'label': source['label'], 'origin': immutable}, indent=2) + '\n')
            temporary.replace(identity)
        current = directory / 'current'
        stage = directory / 'incoming'
        for path in (current, stage):
            if path.exists():
                safe_tree(path)
                # Also retain interrupted pulls before reusing their staging directory.
                backup(path, sid)
        stage.mkdir(mode=0o700, exist_ok=True)
        if current.exists():
            run(['rsync', '-rt', '--checksum', '--', str(current) + '/', str(stage) + '/'])
        for item in source['paths']:
            target = stage / item['mirror_path']
            is_file = item.get('kind') == 'file'
            (target.parent if is_file else target).mkdir(parents=True, exist_ok=True)
            origin = item['path'] if is_file else item['path'].rstrip('/') + '/'
            args = ['rsync', '-rt', '--checksum', '--timeout=60']
            if source.get('ssh'):
                args += ['--protect-args', '-e', 'ssh -oBatchMode=yes -oConnectTimeout=15']
                origin = source['ssh'] + ':' + origin
            args += ['--', origin, str(target) if is_file else str(target) + '/']
            run(args)
        safe_tree(stage)
        backup(stage, sid)
        current.mkdir(mode=0o700, exist_ok=True)
        # No delete, inplace, or sender-removal options. File renames are atomic.
        run(['rsync', '-rt', '--checksum', '--delay-updates', '--', str(stage) + '/', str(current) + '/'])
        metadata = {'id': sid, 'label': source['label'], 'origin': immutable}
        temporary = directory / 'source.json.tmp'
        temporary.write_text(json.dumps(metadata, indent=2) + '\n')
        temporary.replace(identity)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('config', type=Path)
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    root = validate(config)
    for executable in ('rsync', 'restic', 'ssh'):
        if not shutil.which(executable):
            raise ValueError(f'Missing required executable: {executable}')
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink():
        raise ValueError('Mirror root must not be a symlink')
    failures = []
    for source in config['sources']:
        if not source.get('enabled', True):
            continue
        try:
            print(f"{datetime.now(timezone.utc).isoformat()} {source['id']}: collection started", flush=True)
            collect_source(root, source)
            print(f"{datetime.now(timezone.utc).isoformat()} {source['id']}: mirror updated successfully", flush=True)
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            failures.append(source['id'])
            print(f"{datetime.now(timezone.utc).isoformat()} {source['id']}: collection failed: {error}", file=sys.stderr, flush=True)
    return bool(failures)


if __name__ == '__main__':
    sys.exit(main())
