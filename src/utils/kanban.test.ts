import { describe, expect, it, vi } from "vitest";
import { applyKanbanAction, matchesBoardFilter, newKanbanId } from "./kanban";
import { projectKey, type KanbanData } from "@/types/kanban";
import type { ClaudeProject } from "@/types";

export const fixture = (): KanbanData => ({
  revision: 4,
  boards: [
    {
      id: "work",
      name: "Work",
      columns: [
        { id: "todo", name: "To do", projectIds: ["a", "b", "c"] },
        { id: "done", name: "Done", projectIds: ["d"] },
      ],
    },
    {
      id: "personal",
      name: "Personal",
      columns: [{ id: "done", name: "Done", projectIds: ["a"] }],
    },
  ],
});

describe("project Kanban organization", () => {
  it("creates IDs on HTTP origins without crypto.randomUUID", () => {
    const getRandomValues = crypto.getRandomValues.bind(crypto);
    vi.stubGlobal("crypto", { getRandomValues });
    try {
      const ids = new Set(Array.from({ length: 100 }, () => newKanbanId()));
      expect(ids.size).toBe(100);
      expect([...ids].every((id) => /^[a-f0-9]{32}$/.test(id))).toBe(true);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keeps a project's membership, status and order independent on every board", () => {
    const original = fixture();
    const moved = applyKanbanAction(original, {
      type: "moveProject",
      boardId: "work",
      columnId: "done",
      projectId: "a",
      beforeId: "d",
    });
    expect(moved.boards[0]!.columns.map((c) => c.projectIds)).toEqual([
      ["b", "c"],
      ["a", "d"],
    ]);
    expect(moved.boards[1]).toEqual(original.boards[1]);
    const removed = applyKanbanAction(moved, {
      type: "removeProject",
      boardId: "work",
      projectId: "a",
    });
    expect(removed.boards[1]!.columns[0]!.projectIds).toEqual(["a"]);
    expect(original).toEqual(fixture());
  });

  it("reorders cards in both directions and moves into empty columns", () => {
    let data = fixture();
    data = applyKanbanAction(data, {
      type: "moveProject",
      boardId: "work",
      columnId: "todo",
      projectId: "a",
    });
    expect(data.boards[0]!.columns[0]!.projectIds).toEqual(["b", "c", "a"]);
    data = applyKanbanAction(data, {
      type: "moveProject",
      boardId: "work",
      columnId: "todo",
      projectId: "a",
      beforeId: "b",
    });
    expect(data.boards[0]!.columns[0]!.projectIds).toEqual(["a", "b", "c"]);
    data = applyKanbanAction(data, {
      type: "removeProject",
      boardId: "work",
      projectId: "d",
    });
    data = applyKanbanAction(data, {
      type: "moveProject",
      boardId: "work",
      columnId: "done",
      projectId: "c",
    });
    expect(data.boards[0]!.columns[1]!.projectIds).toEqual(["c"]);
  });

  it("does not duplicate memberships or lose cards on invalid or stale drops", () => {
    const data = fixture();
    expect(
      applyKanbanAction(data, {
        type: "addProject",
        boardId: "work",
        columnId: "done",
        projectId: "a",
      }),
    ).toBe(data);
    for (const [columnId, beforeId] of [
      ["missing", undefined],
      ["done", "missing"],
      ["todo", "a"],
    ]) {
      expect(
        applyKanbanAction(data, {
          type: "moveProject",
          boardId: "work",
          columnId: columnId!,
          projectId: "a",
          beforeId,
        }),
      ).toBe(data);
    }
    expect(
      applyKanbanAction(data, {
        type: "moveProject",
        boardId: "personal",
        columnId: "done",
        projectId: "b",
      }),
    ).toBe(data);
  });

  it("moves projects in order before deleting a column and preserves other boards", () => {
    const data = fixture();
    const next = applyKanbanAction(data, {
      type: "deleteColumn",
      boardId: "work",
      columnId: "todo",
      destinationId: "done",
    });
    expect(next.boards[0]!.columns).toEqual([
      { id: "done", name: "Done", projectIds: ["d", "a", "b", "c"] },
    ]);
    expect(next.boards[1]).toEqual(data.boards[1]);
    expect(
      applyKanbanAction(next, {
        type: "deleteColumn",
        boardId: "work",
        columnId: "done",
        destinationId: "done",
      }),
    ).toBe(next);
  });

  it("supports board and column CRUD and column reordering", () => {
    let data = fixture();
    data = applyKanbanAction(data, {
      type: "createBoard",
      board: {
        id: "new",
        name: " New ",
        columns: [{ id: "inbox", name: "Inbox", projectIds: [] }],
      },
    });
    data = applyKanbanAction(data, {
      type: "renameBoard",
      boardId: "new",
      name: " Renamed ",
    });
    data = applyKanbanAction(data, {
      type: "addColumn",
      boardId: "new",
      column: { id: "next", name: "Next", projectIds: [] },
    });
    data = applyKanbanAction(data, {
      type: "renameColumn",
      boardId: "new",
      columnId: "next",
      name: " Later ",
    });
    data = applyKanbanAction(data, {
      type: "moveColumn",
      boardId: "new",
      columnId: "next",
      index: 0,
    });
    expect(data.boards[2]).toEqual({
      id: "new",
      name: "Renamed",
      columns: [
        { id: "next", name: "Later", projectIds: [] },
        { id: "inbox", name: "Inbox", projectIds: [] },
      ],
    });
    data = applyKanbanAction(data, { type: "deleteBoard", boardId: "new" });
    expect(data).toEqual(fixture());
  });

  it("distinguishes the same folder across providers and custom history directories", () => {
    const base = {
      path: "/history/project",
      actual_path: "/repo",
    } as ClaudeProject;
    expect(projectKey(base)).toBe(
      projectKey({ ...base, name: "Renamed", provider: "claude" }),
    );
    expect(projectKey(base)).not.toBe(
      projectKey({ ...base, provider: "codex" }),
    );
    expect(projectKey(base)).not.toBe(
      projectKey({ ...base, path: "/other-history/project" }),
    );
    expect(
      projectKey({ ...base, path: "source:laptop|codex:///repo" }),
    ).not.toBe(projectKey({ ...base, path: "source:desktop|codex:///repo" }));
  });
});

describe("live board filters", () => {
  it("matches any selected board and updates unassigned membership immediately", () => {
    const data = fixture();
    expect(matchesBoardFilter(data, "a", ["personal"], false)).toBe(true);
    expect(matchesBoardFilter(data, "b", ["personal"], false)).toBe(false);
    expect(matchesBoardFilter(data, "b", ["personal", "work"], false)).toBe(
      true,
    );
    expect(matchesBoardFilter(data, "new", [], true)).toBe(true);
    expect(matchesBoardFilter(data, "a", [], true)).toBe(false);
    const assigned = applyKanbanAction(data, {
      type: "addProject",
      boardId: "work",
      columnId: "todo",
      projectId: "new",
    });
    expect(matchesBoardFilter(assigned, "new", [], true)).toBe(false);
  });
});
