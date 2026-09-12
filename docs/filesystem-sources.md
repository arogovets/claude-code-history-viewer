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
