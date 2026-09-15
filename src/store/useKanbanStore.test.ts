import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "@/services/api";
import { useKanbanStore } from "./useKanbanStore";
import type { KanbanData } from "@/types/kanban";
vi.mock("@/services/api", () => ({ api: vi.fn() }));
const data: KanbanData = {
  revision: 0,
  boards: [
    {
      id: "b",
      name: "Board",
      columns: [{ id: "c", name: "Column", projectIds: [] }],
    },
  ],
};

beforeEach(() => {
  vi.resetAllMocks();
  useKanbanStore.setState({
    data,
    loaded: false,
    pending: false,
    error: null,
    selectedBoardId: null,
  });
});
describe("Kanban persistence", () => {
  it("loads and saves through the API and restores saved organization on reload", async () => {
    vi.mocked(api).mockResolvedValueOnce(data);
    await useKanbanStore.getState().load();
    const saved = {
      ...data,
      revision: 1,
      boards: [{ ...data.boards[0]!, name: "Updated" }],
    };
    vi.mocked(api).mockResolvedValueOnce(saved);
    expect(
      await useKanbanStore
        .getState()
        .dispatch({ type: "renameBoard", boardId: "b", name: "Updated" }),
    ).toBe(true);
    expect(api).toHaveBeenLastCalledWith("save_kanban", {
      data: { ...saved, revision: 0 },
    });
    vi.mocked(api).mockResolvedValueOnce(saved);
    await useKanbanStore.getState().load();
    expect(useKanbanStore.getState().data).toEqual(saved);
  });
  it("keeps confirmed data on failed save and requires reload before another edit", async () => {
    useKanbanStore.setState({ loaded: true });
    vi.mocked(api).mockRejectedValueOnce(new Error("Disk full"));
    expect(
      await useKanbanStore
        .getState()
        .dispatch({ type: "deleteBoard", boardId: "b" }),
    ).toBe(false);
    expect(useKanbanStore.getState().data).toEqual(data);
    expect(useKanbanStore.getState().error).toContain("Disk full");
    await useKanbanStore
      .getState()
      .dispatch({ type: "deleteBoard", boardId: "b" });
    expect(api).toHaveBeenCalledTimes(1);
  });
  it("does not allow edits after a failed load or concurrently with another request", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("Unavailable"));
    await useKanbanStore.getState().load();
    expect(
      await useKanbanStore
        .getState()
        .dispatch({ type: "deleteBoard", boardId: "b" }),
    ).toBe(false);
    expect(useKanbanStore.getState().loaded).toBe(false);
    useKanbanStore.setState({ loaded: true, pending: true, error: null });
    expect(
      await useKanbanStore
        .getState()
        .dispatch({ type: "deleteBoard", boardId: "b" }),
    ).toBe(false);
    expect(api).toHaveBeenCalledTimes(1);
  });
});
