import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { api } from "@/services/api";
import { useKanbanStore } from "@/store/useKanbanStore";
import { ProjectKanban } from "./ProjectKanban";
import { ProjectDetails } from "./ProjectDetails";
import { projectKey, type KanbanData } from "@/types/kanban";
import type { ClaudeProject } from "@/types";
import i18n from "@/i18n";

const app = vi.hoisted(() => ({
  projects: [] as ClaudeProject[],
  userMetadata: { projects: {} },
  sessions: [],
  sessionSelectionIds: [],
  sessionSortOrder: "recent",
  getSessionDisplayName: (id: string) => id,
  isServerReadOnly: false,
  setAnalyticsCurrentView: vi.fn(),
}));
vi.mock("@/store/useAppStore", () => ({
  useAppStore: (selector?: (state: typeof app) => unknown) =>
    selector ? selector(app) : app,
}));
vi.mock("@/services/api", () => ({ api: vi.fn() }));
const alpha: ClaudeProject = {
  name: "Alpha",
  path: "/history/alpha",
  actual_path: "/repo/alpha",
  session_count: 3,
  message_count: 12,
  last_modified: "2026-09-10",
};
const beta: ClaudeProject = {
  ...alpha,
  name: "Beta",
  path: "/history/beta",
  actual_path: "/repo/beta",
};
let disk: KanbanData;

beforeEach(async () => {
  cleanup();
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  app.projects = [alpha, beta];
  app.isServerReadOnly = false;
  disk = {
    revision: 1,
    boards: [
      {
        id: "work",
        name: "Work",
        columns: [
          {
            id: "todo",
            name: "To do",
            projectIds: [projectKey(alpha), projectKey(beta)],
          },
          { id: "done", name: "Done", projectIds: [] },
        ],
      },
      {
        id: "personal",
        name: "Personal",
        columns: [
          { id: "done", name: "Done", projectIds: [projectKey(alpha)] },
        ],
      },
    ],
  };
  useKanbanStore.setState({
    data: { revision: 0, boards: [] },
    loaded: false,
    pending: false,
    error: null,
    selectedBoardId: null,
  });
  vi.mocked(api).mockImplementation(async (command, args) => {
    if (command === "save_kanban")
      disk = { ...(args!.data as KanbanData), revision: disk.revision + 1 };
    return structuredClone(disk);
  });
});

const ready = async () => {
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "New board" })).toBeEnabled(),
  );
};
const transfer = () => ({ setData: vi.fn(), effectAllowed: "move" });

describe("Project Kanban UI", () => {
  it("drags cards within a column and across columns without changing another board", async () => {
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await ready();
    const a = screen.getByRole("button", { name: "Alpha" }).closest("article")!;
    const b = screen.getByRole("button", { name: "Beta" }).closest("article")!;
    fireEvent.dragStart(b, { dataTransfer: transfer() });
    fireEvent.dragOver(a);
    fireEvent.drop(a);
    await waitFor(() =>
      expect(disk.boards[0]!.columns[0]!.projectIds).toEqual([
        projectKey(beta),
        projectKey(alpha),
      ]),
    );
    await ready();
    fireEvent.dragStart(
      screen.getByRole("button", { name: "Alpha" }).closest("article")!,
      { dataTransfer: transfer() },
    );
    const empty = within(
      screen.getByRole("region", { name: "Done" }),
    ).getByText("Add a project or drop one here.");
    fireEvent.dragOver(empty);
    fireEvent.drop(empty);
    await waitFor(() =>
      expect(disk.boards[0]!.columns[1]!.projectIds).toEqual([
        projectKey(alpha),
      ]),
    );
    expect(disk.boards[1]!.columns[0]!.projectIds).toEqual([projectKey(alpha)]);
  });

  it("reorders columns by dragging and supports adding and renaming columns", async () => {
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await ready();
    const todoHeader = screen.getByRole("heading", {
      name: "To do",
    }).parentElement!;
    const doneHeader = screen.getByRole("heading", {
      name: "Done",
    }).parentElement!;
    fireEvent.dragStart(todoHeader.querySelector('[draggable="true"]')!, {
      dataTransfer: transfer(),
    });
    fireEvent.dragOver(doneHeader);
    fireEvent.drop(doneHeader);
    await waitFor(() =>
      expect(disk.boards[0]!.columns.map((c) => c.id)).toEqual([
        "done",
        "todo",
      ]),
    );
    await ready();
    fireEvent.click(screen.getByRole("button", { name: "Add column" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Review" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Actions for Review column" }),
      { key: "ArrowDown" },
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Rename Review column" }),
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Testing" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "Testing" }),
      ).toBeInTheDocument(),
    );
    expect(disk.boards[1]!.columns.map((c) => c.name)).toEqual(["Done"]);
  });

  it("creates, renames and deletes boards with safe empty states", async () => {
    disk = { revision: 0, boards: [] };
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await ready();
    fireEvent.click(screen.getByRole("button", { name: "New board" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Roadmap" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(disk.boards[0]!.columns).toHaveLength(3);
    fireEvent.click(screen.getByRole("button", { name: "Rename board" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Release" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(disk.boards[0]!.name).toBe("Release");
    fireEvent.click(screen.getByRole("button", { name: "Delete board" }));
    fireEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Delete",
      }),
    );
    await waitFor(() =>
      expect(
        screen.getByText("Your projects, organized your way"),
      ).toBeInTheDocument(),
    );
    expect(app.projects).toEqual([alpha, beta]);
  });

  it("deletes populated columns by moving projects and opens existing project details", async () => {
    const open = vi.fn();
    render(<ProjectKanban onOpenProject={open} />);
    await ready();
    fireEvent.click(screen.getByRole("button", { name: "Alpha" }));
    expect(open).toHaveBeenCalledWith(alpha);
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Actions for To do column" }),
      { key: "ArrowDown" },
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Delete To do column" }),
    );
    fireEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Delete",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(disk.boards[0]!.columns[0]!.projectIds).toEqual([
      projectKey(alpha),
      projectKey(beta),
    ]);
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Actions for Done column" }),
      { key: "ArrowDown" },
    );
    expect(
      screen.getByRole("button", { name: "Delete Done column" }),
    ).toBeDisabled();
  });

  it("adds existing projects and manages memberships and status from project details", async () => {
    const view = render(<ProjectDetails project={beta} />);
    await waitFor(() =>
      expect(
        screen.getByRole("checkbox", { name: "Membership in Personal" }),
      ).toBeEnabled(),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Membership in Personal" }),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("checkbox", { name: "Membership in Personal" }),
      ).toBeChecked(),
    );
    fireEvent.change(screen.getByRole("combobox", { name: "Status on Work" }), {
      target: { value: "done" },
    });
    await waitFor(() =>
      expect(disk.boards[0]!.columns[1]!.projectIds).toEqual([
        projectKey(beta),
      ]),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Membership in Work" }),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("checkbox", { name: "Membership in Work" }),
      ).not.toBeChecked(),
    );
    expect(disk.boards[1]!.columns[0]!.projectIds).toContain(projectKey(beta));
    view.unmount();
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await ready();
    fireEvent.click(screen.getByRole("button", { name: "Add projects" }));
    fireEvent.click(screen.getByRole("button", { name: "Add Beta to board" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Add Beta to board" }),
      ).not.toBeInTheDocument(),
    );
    expect(disk.boards[0]!.columns[0]!.projectIds).toContain(projectKey(beta));
  });

  it("closes a stale editor when its board disappears after a conflict reload", async () => {
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await ready();
    fireEvent.click(screen.getByRole("button", { name: "Rename board" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Draft name" },
    });
    vi.mocked(api).mockRejectedValueOnce(
      new Error("Boards changed in another window"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        within(screen.getByRole("dialog")).getByRole("alert"),
      ).toBeInTheDocument(),
    );
    disk.boards = disk.boards.filter((board) => board.id !== "work");
    fireEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Reload boards",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("combobox", { name: "Choose board" })).toHaveValue(
      "personal",
    );
    expect(disk.boards[0]!.name).toBe("Personal");
  });

  it("disables edits in read-only mode", async () => {
    app.isServerReadOnly = true;
    render(<ProjectKanban onOpenProject={vi.fn()} />);
    await screen.findByRole("button", { name: "Alpha" });
    expect(screen.getByRole("button", { name: "New board" })).toBeDisabled();
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Actions for Alpha" }),
      { key: "ArrowDown" },
    );
    expect(
      screen.getByRole("button", { name: "Remove Alpha from this board" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("combobox", { name: "Move Alpha to column" }),
    ).toBeDisabled();
  });
});
