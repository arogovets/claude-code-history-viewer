# Filesystem history sources

CCHV discovers history exclusively in registered local mirrors. A separate
[pull-only collector](../collector/README.md) copies local or SSH-accessible
history into those mirrors; source hosts need no CCHV server, watcher, or agent.
Source IDs are stable identities, while labels are editable display names.
Provider handles, search results, grouping, and metadata preserve source identity
so equal session IDs from different machines do not collide.

An unavailable source leaves its last successful mirror readable. Missing origin
files are retained. Growing or changed files advance the readable `current`
mirror only after successful external Restic backup. Previously captured versions
remain in Restic; do not apply pruning policies that discard them. See the
collector guide for configuration, consistency limits, and recovery behavior.

The application reads and watches local mirrors, not origin paths. Collected
history is read-only within CCHV; aliases, hiding, boards, and upstream Archive
Manager remain available. Archive Manager keeps its existing local archive
format and storage. Native provider database readers remain necessary to read
provider-owned formats; this redesign adds no history database or locator.
Existing abandoned cache and snapshot files are not deleted or migrated.

## Phase 1: provider collection and source inventory

The collector and Rust share [`collector/provider-specs.json`](../collector/provider-specs.json).
Each `ProviderId` has exactly one specification of home-relative storage and/or
project-local history patterns. This replaces manually maintained provider path
lists. Configure a source's absolute origin `home` and optional `project_roots`;
manual `paths` remain additive. The collector never copies all of HOME. Project
roots preserve only declared history files and directories, including Aider and
Crush, while retaining the project directory structure.

All parsers continue to read local `source.current`. Platform-specific stores
are selected from mirror candidates, so a Linux source can be read on macOS.
The transport does not enter provider code. Source IDs qualify opaque provider
handles and session IDs; ordinary file paths are naturally isolated by mirror.

Settings Manager now has a read-only **Sources / Directories** inventory. It uses
`list_filesystem_sources` from `sources.rs`, backed by collector `source.json`,
including sources awaiting their first successful capture. Each source exposes
its ID/label, local or SSH origin, configured home and project roots, origin-to-
mirror mappings, file/directory kinds, provider associations, absolute `current`,
and last successful collection timestamp when available.

Legacy `customClaudePaths` remains persisted, but normal scan, search and startup
routing no longer depends on it or on a viewer-host Claude folder. Automatic
`CLAUDE_CONFIG_DIR` registration has been removed from discovery. The Claude
settings editor remains explicitly a local-machine editor, not source-aware
settings management. Remote settings mutation and Archive removal are out of scope.

The [collector guide](../collector/README.md) documents safe upgrades from manual
paths, optional provider stores, filtering, metadata, and retention semantics.
Existing origin mappings remain pinned; safe additions do not change source IDs.

## Rebuild provenance

The implementation was rebuilt from `fdfc766`, before remote-history architecture,
then transplanted onto a branch based on current main without rewriting history.

| Later commit | Retained treatment |
| --- | --- |
| `3684eea` | DeepSeek provider, without remote aggregation |
| `22a0605` | Functional changes |
| `6e001fc` | Omitted release metadata |
| `7c5c1d1` | Functional UI/filter fixes, without version metadata |
| `17e66ae` | Unrelated fatal-error dismissal |
| `fd483d4` | Provider badges with explicit source labels |
| `de61184` | Filtering fixes, without release metadata |
| `57526b1` | Omitted SQLite history cache, locator, and offline fallback |
| `4205735` | Antigravity improvements, without version metadata |
| `f190750` | Aider/Crush performance, navigator, and hiding improvements |
| `f94c008` | Omitted custom snapshot/storage/sync system |

Uncommitted project-board work from the original checkout was preserved and its
commands wired into desktop and WebUI APIs. Unrelated backend test isolation was
also retained. No remote HTTP aggregation or hard-coded host routing is used.

## Verification

Run frontend TypeScript, ESLint, i18n validation, Vitest, and production build;
Rust formatting, Clippy with warnings denied, and tests for both default and all
features; and `python3 -m unittest discover -s collector -v`. Collector tests
exercise real rsync and Restic, retention across changes/deletions, failed backup,
offline origins, stable identity, individual files, and symlink rejection.
Production-library integration tests exercise source isolation and confinement.

## Phase 2: source/provider settings

Settings Manager selects a registered source, then a provider and one declared
scope. The same `collector/provider-specs.json` manifest used by provider discovery
now has an optional `settings.scopes` specification. It describes files and scopes,
not transport. Claude Code user/project/local settings, MCP and managed scopes,
Codex user/project TOML, and OpenCode user/project JSON/JSONC are supported.
These are the standard default locations; environment-specific configuration
location overrides are not guessed. Managed files are always read-only.

A source must explicitly opt into settings writes in its collector configuration:

```json
{
  "id": "lenovo",
  "label": "Lenovo",
  "ssh": "lenovo",
  "home": "/home/your-user",
  "allow_settings_write": false
}
```

The collector publishes this capability into `source.json`, including revocation
when a capture fails. Omitted means false. Set it to `true` only for sources on
which you intend to allow explicit Apply operations. This permission is separate
from reachability and the server's global read-only mode; all must allow a write.
Settings Manager does not implicitly grant this capability.

Settings reads use the authoritative origin through a transport interface:
local Python 3 or batch-mode SSH running the same Python 3 POSIX worker. SSH aliases
and `user@host` use the normal SSH configuration. Both machines need Python 3;
no Python package installation is required. Transport operations have bounded
timeouts and cannot prompt for passwords. Unavailable/unsupported transports do
not fall back to CCHV's host or the history mirror.

The UI reports host online/offline, live/snapshot, snapshot time, and write status.
Unavailable origins use the last sanitized snapshot read-only, with no applicable
revision. Invalid origin configuration is not exposed raw. There is no offline
edit queue or automatic retry on reconnect. A failed or conflicting Apply disables
further Apply until the user refreshes and reviews.

Snapshots live under `~/.claude-history-viewer/settings-snapshots/`, independently
of mirrors and Restic. Their identity includes source origin, provider, scope, and
project selection. Only parsed, sanitized configuration is persisted or returned;
comments are omitted, and sensitive keys, environment values, MCP arguments,
commands, headers, and credential-bearing URLs are redacted. Raw source bytes
remain in memory/on transport pipes and at the authoritative origin only.

Apply checks the SHA-256 revision of the original bytes, merges the proposed
configuration with a fresh origin read, validates syntax and known structural
constraints, and performs an origin-side revision check immediately before an
atomic replacement. A source-side lock serializes CCHV writers; an additional
check catches external modifications before replacement. Temporary files are
created with restrictive permissions in the origin directory, flushed, renamed,
and followed by a separate authoritative read to refresh the snapshot and UI.
Paths are opened without following symlinks, and arbitrary settings paths cannot
be supplied by the frontend. As with other portable POSIX read-check-rename
implementations, a writer that ignores the lock can race in the interval between
the final comparison and rename; this is not a filesystem-level compare-and-swap.

Edits are **add/update merges**. Omitted fields and redacted values preserve the
current origin values. Arrays containing redacted entries must remain unchanged.
Deleting fields or replacing a container of protected values is not inferred from
whole-file editor state. Formatting/comments may be normalized on Apply; unknown
configuration fields and protected values survive. Validation checks JSON/JSONC
or TOML syntax and basic known field shapes, not every provider-version-specific
schema constraint. Mixed settings/MCP presets must be reviewed by scope and are
rejected before any partial application.

Projects are resolved through recorded source mount mappings, never by stripping
a mirror prefix. Project labels that contain an original source path are first
mapped to the corresponding mirror path using those same records. Missing or
conflicting mappings disable editing. CCHV presets remain locally editable even
when a source is offline or settings writes are disabled. Archive remains available;
its provider cleanup-policy edit links to source-aware Settings Manager.

### Collection boundary

Configuration/credential files declared for these providers, plus origin-side
CCHV temporary files, are excluded from future history pulls. If a preexisting
mirror contains one of these files, collection stops **before a further Restic
backup**, without deleting anything. Such mirrors and any already retained secret
versions require a separate operator cleanup/retention review. Phase 2 does not
rewrite or purge existing history or Restic snapshots.

Scope references: [Claude Code configuration](https://code.claude.com/docs/en/configuration),
[Codex configuration](https://developers.openai.com/codex/config-basic),
and [OpenCode configuration](https://opencode.ai/docs/config/).

### Antigravity settings and existing manual sources

Antigravity exposes CLI user settings, global MCP, project MCP, and the legacy
IDE MCP file through the same source-aware settings transport. Current paths
follow the [Antigravity MCP documentation](https://antigravity.google/docs/mcp)
and [CLI settings documentation](https://antigravity.google/docs/cli/settings).
Missing scopes are reported independently; no file is copied from another scope.

Existing manual sources can add an explicit `home` with `collect_home: false`
to enable settings without expanding the history mounts. Set
`allow_settings_write: true` to permit reviewed settings changes on that source.
The collector records the home after successful collection; refresh sources
and settings afterwards.
