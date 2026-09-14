# Pull-only history collector

This standalone Python 3.9+ program invokes rsync and Restic. CCHV does not run it.
Source machines need only readable history directories and, for SSH pulls, SSH
and rsync. Use SSH configuration aliases and keys; no CCHV server or agent runs
on a source machine.

Initialize an external Restic repository and set `RESTIC_REPOSITORY` and
`RESTIC_PASSWORD_FILE` in the collector's environment. Keep the repository
outside the mirror root. Never run retention/prune policies that remove captured
versions. CCHV neither implements nor invokes retention, restore, or deletion.
See the [Restic backup documentation](https://restic.readthedocs.io/en/stable/040_backup.html).
A backup must exit successfully; incomplete backups block mirror advancement.

## Provider-driven configuration

[`provider-specs.json`](provider-specs.json) is the canonical collection contract
for every Rust `ProviderId`. The collector reads this file and Rust embeds the
same file; the frontend maintains no provider-path list. Adding a provider
without exactly one nonempty specification fails the backend contract test.
Keep the manifest beside `collect.py` when installing or moving the collector.

Give each source an explicit absolute origin `home`. All declared home-relative
provider stores are collected automatically when present, including platform
variants and shared editor stores. Missing optional stores are recorded as absent;
an unreachable source or a failed transfer blocks promotion. The origin home
itself is never copied. Runtime parsers discover storage in `current`, independent
of whether rsync used a local path or SSH.

```json
{
  "mirror_root": "/srv/cchv/mirrors",
  "sources": [
    {
      "id": "work-laptop",
      "label": "Work laptop",
      "ssh": "work-laptop",
      "home": "/home/me",
      "project_roots": [
        {"path": "/home/me/Developer", "mirror_path": "Developer"},
        {"path": "/srv/projects", "mirror_path": "projects/shared"}
      ],
      "paths": [
        {"path": "/srv/exports/session.jsonl", "mirror_path": "manual/session.jsonl", "kind": "file"}
      ]
    }
  ]
}
```

Omit `ssh` for local copies. Local and SSH origins use identical relative mirror
layouts; neither parsers nor the frontend connect to source hosts. SSH sources
require a POSIX shell, SSH and modern rsync supporting `--protect-args`.

`project_roots` are explicit code/project directories, never HOME or its ancestors.
The collector traverses them but copies only provider-declared history material:
Aider's `.aider.chat.history.md` and Crush's `.crush` directory (including database
sidecars). It does not copy source code or the rest of HOME. The relative project
layout is preserved, and discovery uses these recorded roots at any nesting depth.

`paths` remain additive manual extras; `kind` defaults to `directory` and can be
`file`. Include the filename in a file's `mirror_path`. An extra can carry
`providers`, an array of provider IDs for display. Map extra history into the
provider's normal mirror layout if it should be parsed. Conflicting or overlapping
manual mounts are rejected. A manual subpath already covered by an automatic
store is coalesced only when it describes exactly the same origin mapping.
Do not map an unrelated export into a store already covered by a different origin;
use a separate source for that export.

For an existing manual history layout, set `home` and `collect_home: false` to
enable authoritative settings access without adding automatic provider stores.
Explicit `paths` and `project_roots` still collect normally. The default is
`collect_home: true`.

Old `paths`-only configurations remain supported without deleting or migrating
history. Add `home` to enable provider-driven collection. Safe additive changes
keep the existing source ID; changing SSH identity, moving a pinned home, removing
configured mounts, or remapping existing paths requires a new ID. Redundant manual
paths can be removed once the automatic mapping covers the same origin. Labels
can be edited. Existing mirrors remain readable throughout an upgrade.

Set `enabled` to `false` to pause collection. Set `CCHV_MIRROR_ROOT` to the same
root when launching CCHV. Run `python3 collector/collect.py /absolute/path/collector.json`
manually or from an external scheduler. No delete, sender-removal, link-following,
or in-place rsync options are used. The collector never writes to the origin.

## Source inventory

`source.json` records `id`, `label`, configured `origin` (`ssh`, `home`, manual
`paths`, and `project_roots`), resolved `mounts`, and `last_collected_at` after
successful promotion. Each mount records absolute origin `path`, relative
`mirror_path`, `kind`, provider IDs, role (`provider`, `project_root`, or `extra`),
and availability at collection. A failed pull does not advance the success
timestamp or replace the previous inventory. Initial failed collections retain
the declared inventory with no success timestamp. Absent origin paths can still
have retained files in `current`.

CCHV's `list_filesystem_sources` command (also `POST /api/list_filesystem_sources`)
returns this metadata plus the absolute local `current` path. Settings Manager's
**Sources / Directories** section displays every registered source, its origin,
path mappings, provider associations, and last successful collection. This view
is read-only. It does not read `customClaudePaths`, change collector configuration,
edit mirrors, or write settings to SSH hosts. Legacy metadata without resolved
mounts is displayed using its recorded origin paths.

Each source has a stable `id`, a readable `current` tree and `source.json` identity
metadata. IDs cannot be reused for different origins. Labels can change. A failed
source does not stop independent sources. An exclusive per-source lock prevents
concurrent collectors from interleaving updates.

Before transfer, existing current and interrupted incoming trees are backed up.
The new incoming tree must also be successfully backed up before promotion.
Absent upstream files remain in the mirror. Changed files advance in current;
Restic preserves prior captured bytes. Staging is a working directory, not a
snapshot archive or history database. Promotion uses rsync's delayed file
renames: individual files are atomically replaced, but a multi-file generation
is not an atomic filesystem transaction. A crash during promotion can expose a
mix of two backed-up generations; rerunning converges without losing either.

Active source files may change while being copied. This is a capture of observed
bytes, not a claim of application-consistent backup. Database-backed providers
need a consistent upstream filesystem snapshot/export including appropriate WAL
state; this collector does not introduce SQLite caching or database merge logic.

Run collector tests with `python3 -m unittest discover -s collector -v`.

Restart CCHV after registering a new source so its watcher includes that source.
Subsequent changes under an existing current mirror are watched automatically.
The default mirror root is `~/.claude-history-viewer/mirrors`.

## Automatic collection on macOS

After initializing Restic and testing the configuration, install a user LaunchAgent:

```sh
python3 collector/install_launchagent.py \
  --config ~/.config/cchv-collector/sources.json \
  --repository ~/Backups/cchv-history-restic \
  --password-file ~/.config/cchv-collector/restic-password
```

The installer pins absolute collector/configuration paths and dependency search
paths. Keep this checkout available. No password contents are stored in the plist.
The job starts at login and every 60 seconds (`--interval` changes this).
Launchd does not run overlapping instances of the same job. A long collection
finishes before another starts; failed jobs are retried on subsequent intervals.
Sleep, offline hosts, failed backups, or slow transfers can delay freshness; the
last successful mirror remains readable. No source-host agent is installed.

Check `launchctl print "gui/$(id -u)/com.cchv.collector"` and the timestamped
success/failure output in `~/Library/Logs/cchv-collector/`. To stop automatic
collection, run `launchctl bootout "gui/$(id -u)/com.cchv.collector"` and remove
`~/Library/LaunchAgents/com.cchv.collector.plist`. This does not remove any history.

### Provider settings permission

`allow_settings_write` is an optional boolean on each source, defaulting to
`false`. It is published in `source.json` for Settings Manager and updated even
when a capture fails, so permission can be revoked while a source is offline.
Reachability alone never grants write access. Provider settings use a separate
local/SSH control plane; they are not collected into history.

The collector excludes declared provider configuration, known credentials, and
CCHV origin temporary files. A preexisting mirror containing these files blocks
collection before Restic backup, without deleting any retained material. Review
and clean up existing control-plane files/retained versions separately.
See [Phase 2 settings](../docs/filesystem-sources.md#phase-2-sourceprovider-settings)
for transport requirements and edit semantics.
