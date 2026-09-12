# Project boards

Open **Project boards** from the header to create and switch between Kanban boards. Each new board starts with To do, In progress, and Done. Rename, add, delete, or reorder columns independently on each board. Drag column handles or use the left/right buttons.

Use **Add projects** to choose existing projects and their starting column. A project can belong to any number of boards, once per board. Drag a card onto another card to insert it before that card, or onto a column's background to append it. The card's column selector and up/down buttons provide keyboard and touch alternatives.

Click a card to open **Project details**. This page is also available from the header whenever a project is selected, and appears when selecting a project without a session. Its **Board memberships** controls add/remove membership and change the project's column independently for each board.

Deleting a populated column requires selecting another column to receive its projects, preserving their relative order. The last column cannot be deleted. Removing a project from a board or deleting a board does not delete any project or session history.

## Persistence

Organization is saved atomically in `~/.claude-history-viewer/boards.json` using the same `load_kanban` and `save_kanban` commands for desktop and WebUI. Existing installations start with no boards; provider history and `user-data.json` need no migration. WebUI stores boards on its server, not in browser local storage. Read-only servers allow viewing and block edits.

Each board contains an ordered array of columns. Each column contains ordered references to existing projects; containment defines the project's status and order within that board. There is no task entity or global project status. References use the provider and history storage path, preserving distinctions between providers and custom history directories. The temporary offline marker on remote paths is excluded from identity. Projects absent from the current scan remain as unavailable cards until they return or their membership is removed.

Saves include a revision check. A stale window must reload before editing again, and failed saves retain the last confirmed state. Desktop and WebUI requests in the same process share a save mutex. The revision check is not a cross-process file lock; running separate app processes against the same file concurrently is not supported.

## Verification

- `pnpm exec vitest run src/utils/kanban.test.ts src/store/useKanbanStore.test.ts src/components/ProjectKanban/ProjectKanban.test.tsx`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features webui-server commands::kanban -- --test-threads=1`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features webui-server read_only -- --test-threads=1`

For browser development, `VITE_MOCK=1 pnpm dev` provides in-memory boards and an existing mock project. Mock boards reset when the development server restarts.

## Project organization and offline history

Explorer and Add projects support multiple board filters plus “not assigned to any board.” Matching is inclusive (any selected board or unassigned); assignments update these filters immediately. Board colors appear on memberships and as dots on cards belonging to other boards. Card and column controls live in their three-dot menus, and cards show last activity.

Project details include the Explorer session list, an editable name, description, and HTTP(S) links. WebUI URLs use `?view=kanban&board=ID` and `?view=project&project=ENCODED_PROJECT_KEY`, including browser back/forward navigation.

Successful remote scans schedule a background download of complete session lists and messages into SQLite. Later scans retry interrupted downloads and fetch sessions whose modification date changed. Unavailable hosts use saved session pages, including an explicit empty-cache state. Histories never downloaded before a host went offline require that host to reconnect; cached lists alone cannot reconstruct message bodies.
