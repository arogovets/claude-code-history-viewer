#!/usr/bin/env python3
"""Pull history into local mirrors; Restic owns retained versions, not CCHV."""
import argparse
from datetime import datetime, timezone
import fcntl
import fnmatch
import json
import os
from pathlib import Path
import re
import shutil
import shlex
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


SPEC_FILE = Path(__file__).with_name('provider-specs.json')


def provider_specs():
    return json.loads(SPEC_FILE.read_text())['providers']


def settings_exclusions():
    """Control-plane files never enter history or Restic, including custom mounts."""
    paths = {'.claude/.mcp.json', '.claude/.credentials.json', '.codex/auth.json',
             '.local/share/opencode/auth.json'}
    for spec in provider_specs():
        for scope in (spec.get('settings') or {}).get('scopes', []):
            if scope['base'] != 'absolute':
                paths.add(scope['path'])
            else:
                paths.add(Path(scope['path']).name)
    # Basename exclusions cover mounts rooted inside a provider directory too.
    names = {Path(path).name for path in paths}
    # Provider/editor backups and swap files can contain the same credentials.
    backups = {pattern for name in names for pattern in (name + '.*', name + '~', '.' + name + '.*')}
    return sorted(names | backups | {".*.cchv-*", ".*.cchv.lock", "settings-snapshots"})


def is_provider_settings_path(path):
    return any(fnmatch.fnmatch(Path(path).name, pattern) for pattern in settings_exclusions())


def reject_retained_settings(root):
    for _, directories, files in os.walk(root):
        if any(is_provider_settings_path(name) for name in directories + files):
            raise ValueError('Existing mirror contains provider configuration; remove control-plane files from the mirror before collecting. Existing Restic versions require a separate retention review.')


def relative_path(value):
    path = Path(value)
    if (not value or path.is_absolute() or any(p in ('', '.', '..') for p in value.split('/'))
            or '\\' in value or any(ord(c) < 32 for c in value)):
        raise ValueError('mirror_path must be a relative path without traversal')
    return path


def absolute_path(value):
    if (not value.startswith('/') or '..' in value.split('/') or '\\' in value
            or any(ord(c) < 32 for c in value)):
        raise ValueError('Source path must be absolute without traversal')
    if Path(value) == Path('/'):
        raise ValueError('Collecting the filesystem root is forbidden')
    return Path(value)


def collection_paths(source):
    """Resolve one manifest for either transport. No origin filesystem discovery."""
    specs = provider_specs()
    paths = []
    if not isinstance(source.get('collect_home', True), bool):
        raise ValueError('collect_home must be a boolean')
    if source.get('home'):
        absolute_path(source['home'])
    if source.get('home') and source.get('collect_home', True):
        home = absolute_path(source['home'])
        for spec in specs:
            for item in spec['home_paths']:
                relative_path(item['path'])
                paths.append(dict(path=str(home / item['path']), mirror_path=item['path'],
                                  kind=item['kind'], providers=[spec['provider']], optional=True,
                                  role='provider'))
    for item in source.get('project_roots', []):
        root = absolute_path(item['path'])
        if source.get('home') and (root == Path(source['home']) or root in Path(source['home']).parents):
            raise ValueError('Project roots must not be HOME')
        paths.append(dict(item, kind='directory', role='project_root',
                          providers=[s['provider'] for s in specs if s['project_paths']]))
    paths.extend(dict(item, kind=item.get('kind', 'directory'), providers=item.get('providers', []), role='extra')
                 for item in source.get('paths', []))
    # Shared stores (Gemini/Antigravity, editor extensions) are copied once.
    # A manual submount is redundant only if its origin mapping is identical.
    merged = []
    for item in sorted(paths, key=lambda p: len(relative_path(p['mirror_path']).parts)):
        absolute_path(item['path'])
        relative = relative_path(item['mirror_path'])
        for parent in merged:
            base = Path(parent['mirror_path'])
            if relative == base or base in relative.parents:
                suffix = relative.relative_to(base)
                if (Path(parent['path']) / suffix != Path(item['path'])
                        or parent['kind'] == 'file' or parent['role'] == 'project_root'
                        or item['role'] == 'project_root'):
                    raise ValueError('Overlapping mirror paths')
                parent['providers'] = sorted(set(parent['providers'] + item['providers']))
                break
        else:
            merged.append(item)
    return merged


def origin_exists(source, path, kind='directory'):
    """Absence is optional; connection, permission and other failures are not."""
    if not source.get('ssh'):
        if Path(path).is_symlink():
            raise ValueError('Source mount must not be a symlink')
        try:
            stat = Path(path).stat()
        except FileNotFoundError:
            return False
        import stat as stat_module
        matches = stat_module.S_ISDIR(stat.st_mode) if kind == 'directory' else stat_module.S_ISREG(stat.st_mode)
        if not matches:
            raise ValueError('Source mount has unexpected kind')
        return True
    # Check every existing component beneath the configured home. A symlink or
    # inaccessible ancestor is a failed pull, never an 'optional store absent'.
    candidate = Path(path)
    home = Path(source.get('home', path))
    components = [candidate]
    if candidate.is_relative_to(home):
        components = [home]
        for part in candidate.relative_to(home).parts:
            components.append(components[-1] / part)
    checks = ''
    for component in components:
        quoted_component = shlex.quote(str(component))
        checks += f'if test -L {quoted_component}; then exit 45; fi; '
        checks += f'if test -d {quoted_component} && ! test -x {quoted_component}; then exit 45; fi; '
    quoted = shlex.quote(path)
    flag = '-d' if kind == 'directory' else '-f'
    result = subprocess.run(['ssh', '-oBatchMode=yes', '-oConnectTimeout=15', source['ssh'],
                             checks + f'if test -L {quoted}; then exit 45; elif test {flag} {quoted}; then exit 0; elif test ! -e {quoted}; then exit 44; else exit 45; fi'],
                            check=False)
    if result.returncode not in (0, 44):
        raise subprocess.CalledProcessError(result.returncode, 'ssh source probe')
    return result.returncode == 0


def validate(config):
    root = Path(config['mirror_root']).expanduser()
    if not root.is_absolute():
        raise ValueError('mirror_root must be absolute')
    seen = set()
    for source in config['sources']:
        sid = source['id']
        if not isinstance(source.get('allow_settings_write', False), bool):
            raise ValueError('allow_settings_write must be a boolean')
        if not re.fullmatch(r'[a-z0-9][a-z0-9_-]{0,63}', sid) or sid in seen:
            raise ValueError(f'Invalid or duplicate source id: {sid}')
        seen.add(sid)
        if not source.get('label', '').strip():
            raise ValueError('Each source requires a label')
        if source.get('ssh') and not re.fullmatch(r'[a-zA-Z0-9_][a-zA-Z0-9_.@-]*', source['ssh']):
            raise ValueError('ssh must be an SSH config alias or user@host')
        mounts = set()
        for item in source.get('paths', []) + source.get('project_roots', []):
            if item.get('kind', 'directory') not in ('directory', 'file'):
                raise ValueError('kind must be directory or file')
            relative = relative_path(item['mirror_path'])
            if relative.is_absolute() or '..' in relative.parts or not relative.parts:
                raise ValueError('mirror_path must be a relative path without traversal')
            if any(relative == p or relative in p.parents or p in relative.parents for p in mounts):
                raise ValueError('Overlapping mirror paths')
            mounts.add(relative)
            origin_path = absolute_path(item['path'])
            if source.get('home') and (origin_path == Path(source['home']) or origin_path in Path(source['home']).parents):
                raise ValueError('Extra paths must not copy HOME or its ancestors')
            if not source.get('ssh'):
                origin = Path(item['path']).resolve()
                target = root.resolve()
                if origin == target or origin in target.parents or target in origin.parents:
                    raise ValueError('Source and mirror roots must not overlap')
        for item in collection_paths(source):
            origin = absolute_path(item['path'])
            if not source.get('ssh'):
                if item['role'] == 'provider' and not origin.resolve().is_relative_to(Path(source['home']).resolve()):
                    raise ValueError('Provider path escapes source HOME')
                origin, target = origin.resolve(), root.resolve()
                if origin == target or origin in target.parents or target in origin.parents:
                    raise ValueError('Source and mirror roots must not overlap')
    return root


def backup(path, sid):
    # A nonzero result, including Restic's incomplete-backup code 3, blocks writes.
    run(['restic', 'backup', '--tag', 'cchv-source:' + sid, '--', str(path)])


def compatible_origin(previous, proposed, mounts):
    if previous.get('ssh') != proposed.get('ssh'):
        return False
    if previous.get('home') and previous['home'] != proposed.get('home'):
        return False
    for item in previous.get('project_roots', []):
        if not any(item['path'] == p['path'] and item['mirror_path'] == p['mirror_path']
                   for p in proposed.get('project_roots', [])):
            return False
    # Additive configuration upgrades are safe; existing origin mappings stay pinned.
    for item in previous.get('paths', []) + previous.get('project_roots', []):
        relative = Path(item['mirror_path'])
        if not any(
            relative.is_relative_to(Path(m['mirror_path']))
            and Path(m['path']) / relative.relative_to(Path(m['mirror_path'])) == Path(item['path'])
            for m in mounts
        ):
            return False
    return True


def collect_source(root, source):
    validate({'mirror_root': str(root), 'sources': [source]})
    mounts = collection_paths(source)
    for item in mounts:
        if ('settings-snapshots' in Path(item['path']).parts
                or 'settings-snapshots' in Path(item['mirror_path']).parts
                or (item['kind'] == 'file' and
                    (is_provider_settings_path(item['path']) or is_provider_settings_path(item['mirror_path'])))):
            raise ValueError('Provider configuration and settings snapshots must not be collected as history')
    if root.is_symlink():
        raise ValueError('Mirror root must not be a symlink')
    sid = source['id']
    directory = root / sid
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.is_symlink():
        raise ValueError('Source directory must not be a symlink')
    if any((directory / name).is_symlink() for name in ('.collector.lock', 'source.json', 'source.json.tmp')):
        raise ValueError('Collector metadata must not be a symlink')
    with (directory / '.collector.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        identity = directory / 'source.json'
        immutable = {'id': sid, 'ssh': source.get('ssh'), 'paths': source.get('paths', [])}
        for key in ('home', 'project_roots'):
            if key in source:
                immutable[key] = source[key]
        if identity.exists():
            previous = json.loads(identity.read_text())
            if not compatible_origin(previous['origin'], immutable, mounts):
                raise ValueError('Source identity changed; assign a new id for a different origin')
        # Capability is control-plane metadata; update it even on failed captures,
        # preserving the previous successful inventory and collection timestamp.
        if not isinstance(source.get('allow_settings_write', False), bool):
            raise ValueError('allow_settings_write must be a boolean')
        metadata = previous.copy() if identity.exists() else {
            'id': sid, 'label': source['label'], 'origin': immutable, 'mounts': mounts}
        capability = source.get('allow_settings_write', False)
        if not identity.exists() or metadata.get('allow_settings_write') != capability:
            metadata['allow_settings_write'] = capability
            temporary = directory / 'source.json.tmp'
            temporary.write_text(json.dumps(metadata, indent=2) + '\n')
            temporary.replace(identity)
        if source.get('home') and not origin_exists(source, source['home']):
            raise ValueError('Source HOME is unavailable')
        current = directory / 'current'
        stage = directory / 'incoming'
        for path in (current, stage):
            if path.exists():
                safe_tree(path)
                reject_retained_settings(path)
                # Also retain interrupted pulls before reusing their staging directory.
                backup(path, sid)
        stage.mkdir(mode=0o700, exist_ok=True)
        if current.exists():
            run(['rsync', '-rt', '--checksum', '--', str(current) + '/', str(stage) + '/'])
        for item in mounts:
            if item.get('optional') and not origin_exists(source, item['path'], item['kind']):
                item['available'] = False
                continue
            if not item.get('optional') and not origin_exists(source, item['path'], item['kind']):
                raise ValueError('Required source mount is unavailable')
            item['available'] = True
            target = stage / item['mirror_path']
            is_file = item.get('kind') == 'file'
            (target.parent if is_file else target).mkdir(parents=True, exist_ok=True)
            origin = item['path'] if is_file else item['path'].rstrip('/') + '/'
            args = ['rsync', '-rt', '--checksum', '--timeout=60']
            for pattern in settings_exclusions():
                args += ['--exclude=' + pattern]
            if item['role'] == 'project_root':
                for spec in provider_specs():
                    for pattern in spec['project_paths']:
                        args += ['--include=**/' + pattern]
                args += ['--include=*/', '--exclude=*', '--prune-empty-dirs']
            if source.get('ssh'):
                args += ['--protect-args', '-e', 'ssh -oBatchMode=yes -oConnectTimeout=15']
                origin = source['ssh'] + ':' + origin
            args += ['--', origin, str(target) if is_file else str(target) + '/']
            run(args)
        safe_tree(stage)
        reject_retained_settings(stage)
        backup(stage, sid)
        current.mkdir(mode=0o700, exist_ok=True)
        # No delete, inplace, or sender-removal options. File renames are atomic.
        run(['rsync', '-rt', '--checksum', '--delay-updates', '--', str(stage) + '/', str(current) + '/'])
        metadata = {'id': sid, 'label': source['label'], 'origin': immutable,
                    'mounts': mounts, 'allow_settings_write': source.get('allow_settings_write', False), 'last_collected_at': datetime.now(timezone.utc).isoformat()}
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
