#!/usr/bin/env python3
"""Install automatic pull collection for the current macOS login user."""
import argparse
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--repository', type=Path, required=True)
    parser.add_argument('--password-file', type=Path, required=True)
    parser.add_argument('--interval', type=int, default=60)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('This installer requires macOS')
    if args.interval < 10:
        parser.error('interval must be at least 10 seconds')
    for path in (args.config, args.password_file):
        if not path.expanduser().is_file():
            parser.error(f'Missing file: {path}')
    if not args.repository.expanduser().is_dir():
        parser.error('Initialize the Restic repository before installing')
    executable_dirs = []
    for name in ('restic', 'rsync', 'ssh'):
        executable = shutil.which(name)
        if not executable:
            parser.error(f'Missing executable: {name}')
        executable_dirs.append(str(Path(executable).parent))
    home = Path.home()
    logs = home / 'Library/Logs/cchv-collector'
    logs.mkdir(parents=True, exist_ok=True, mode=0o700)
    label = 'com.cchv.collector'
    plist = home / 'Library/LaunchAgents' / (label + '.plist')
    plist.parent.mkdir(parents=True, exist_ok=True)
    definition = {
        'Label': label,
        'ProgramArguments': [sys.executable, '-u', str(Path(__file__).resolve().with_name('collect.py')),
                             str(args.config.expanduser().resolve())],
        'EnvironmentVariables': {
            'PATH': ':'.join(dict.fromkeys(executable_dirs + ['/usr/bin', '/bin', '/usr/sbin', '/sbin'])),
            'RESTIC_REPOSITORY': str(args.repository.expanduser().resolve()),
            'RESTIC_PASSWORD_FILE': str(args.password_file.expanduser().resolve()),
        },
        'RunAtLoad': True,
        'StartInterval': args.interval,
        'ProcessType': 'Standard',
        'StandardOutPath': str(logs / 'collector.log'),
        'StandardErrorPath': str(logs / 'collector-error.log'),
    }
    target = f'gui/{os.getuid()}'
    # Updating this job never changes the separate CCHV WebUI service.
    subprocess.run(['launchctl', 'bootout', target + '/' + label],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    temporary = plist.with_suffix('.plist.tmp')
    temporary.write_bytes(plistlib.dumps(definition))
    temporary.chmod(0o600)
    temporary.replace(plist)
    subprocess.run(['launchctl', 'bootstrap', target, str(plist)], check=True)
    print(f'Installed {label}: at login and every {args.interval} seconds; logs: {logs}')


if __name__ == '__main__':
    main()
