import { create } from "zustand";
import { api } from "@/services/api";
import type { KanbanAction, KanbanData } from "@/types/kanban";
import { applyKanbanAction } from "@/utils/kanban";

interface KanbanStore {
  data: KanbanData;
  loaded: boolean;
  pending: boolean;
  error: string | null;
  selectedBoardId: string | null;
  selectBoard: (id: string) => void;
  load: () => Promise<void>;
  dispatch: (action: KanbanAction) => Promise<boolean>;
}

export const useKanbanStore = create<KanbanStore>((set, get) => ({
  data: { revision: 0, boards: [] },
  loaded: false,
  pending: false,
  error: null,
  selectedBoardId: null,
  selectBoard: (selectedBoardId) => set({ selectedBoardId }),
  load: async () => {
    if (get().pending) return;
    set({ pending: true, error: null });
    try {
      const data = await api<KanbanData>("load_kanban");
      set({ data, loaded: true });
    } catch (error) {
      set({ error: String(error) });
    } finally {
      set({ pending: false });
    }
  },
  dispatch: async (action) => {
    // Controls share this guard: a second edit cannot race a save or load.
    if (!get().loaded || get().pending || get().error) return false;
    const data = applyKanbanAction(get().data, action);
    if (data === get().data) return false;
    set({ pending: true, error: null });
    try {
      const saved = await api<KanbanData>("save_kanban", { data });
      set({ data: saved });
      return true;
    } catch (error) {
      // Keep the last confirmed state; never show an unsaved edit as successful.
      set({ error: String(error) });
      return false;
    } finally {
      set({ pending: false });
    }
  },
}));
