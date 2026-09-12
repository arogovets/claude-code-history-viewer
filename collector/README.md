# Pull-only history collector

This standalone Python 3 program invokes rsync and Restic. CCHV does not run it.
Source machines need only readable history directories and, for SSH pulls, SSH
and rsync. Use SSH configuration aliases and keys; no CCHV server or agent runs
on a source machine.

Initialize an external Restic repository and set `RESTIC_REPOSITORY` and
`RESTIC_PASSWORD_FILE` in the collector's environment. Keep the repository
outside the mirror root. Never run retention/prune policies that remove captured
versions. CCHV neither implements nor invokes retention, restore, or deletion.
See the [Restic backup documentation](https://restic.readthedocs.io/en/stable/040_backup.html).
A backup must exit successfully; incomplete backups block mirror advancement.

Example configuration (absolute paths; directories by default):

```json
{
  "mirror_root": "/srv/cchv/mirrors",
  "sources": [
    {
      "id": "work-laptop",
      "label": "Work laptop",
      "ssh": "work-laptop",
      "paths": [
        {"path": "/home/me/.claude/projects", "mirror_path": ".claude/projects"},
        {"path": "/home/me/.codex/sessions", "mirror_path": ".codex/sessions"}
      ]
    }
  ]
}
```

Omit `ssh` for local copies. Set `kind` to `file` and include the destination filename in `mirror_path` to copy a single native data file. Set `enabled` to `false` to keep a configured source paused. Set `CCHV_MIRROR_ROOT` to the same root when launching
CCHV. Run `python3 collector/collect.py /absolute/path/collector.json` manually or
from an external scheduler. Modern rsync supporting `--protect-args` is required
for SSH pulls. No delete, sender-removal, link-following, or in-place options are
used. The collector never writes to the origin.

Each source has a stable `id`, a readable `current` tree and `source.json` identity
metadata. IDs cannot be reused for changed origins. Labels can change. A failed
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
