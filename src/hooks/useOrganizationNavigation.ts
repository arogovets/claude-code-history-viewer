import { useEffect, useRef } from "react";
import { useAppStore } from "@/store/useAppStore";
import { useKanbanStore } from "@/store/useKanbanStore";
import { projectKey } from "@/types/kanban";
import { isWebUI } from "@/utils/platform";
import { toast } from "sonner";
import i18n from "@/i18n";

/** Organization routes coexist with the existing session/msg query links. */
export function useOrganizationNavigation() {
  const projects = useAppStore((s) => s.projects);
  const loading = useAppStore((s) => s.isLoadingProjects);
  const ready = useRef(false);
  const restoring = useRef(false);
  const restore = useRef<() => Promise<void>>(async () => {});
  useEffect(() => {
    if (!isWebUI()) return;
    let disposed = false;
    let generation = 0;
    restore.current = async () => {
      const request = ++generation;
      restoring.current = true;
      const url = new URL(window.location.href);
      try {
        const state = useAppStore.getState();
        if (url.searchParams.get("view") === "kanban") {
          if (!useKanbanStore.getState().loaded)
            await useKanbanStore.getState().load();
          if (disposed || request !== generation) return;
          const id = url.searchParams.get("board");
          if (id) useKanbanStore.getState().selectBoard(id);
          state.setAnalyticsCurrentView("kanban");
        } else if (url.searchParams.get("view") === "project") {
          const project = state.projects.find(
            (p) => projectKey(p) === url.searchParams.get("project"),
          );
          if (!project) {
            toast.error(i18n.t("kanban.projectNotFound"));
            return;
          }
          await state.selectProject(project);
          if (!disposed && request === generation)
            state.setAnalyticsCurrentView("projectDetails");
        }
      } finally {
        if (request === generation) {
          restoring.current = false;
          ready.current = true;
        }
      }
    };
    const write = () => {
      if (!ready.current || restoring.current) return;
      const state = useAppStore.getState();
      const url = new URL(window.location.href);
      for (const key of ["view", "project", "board"])
        url.searchParams.delete(key);
      if (state.analytics.currentView === "kanban") {
        url.searchParams.set("view", "kanban");
        const kanban = useKanbanStore.getState();
        const board =
          kanban.data.boards.find((b) => b.id === kanban.selectedBoardId) ??
          kanban.data.boards[0];
        if (board) url.searchParams.set("board", board.id);
      } else if (
        state.selectedProject &&
        (state.analytics.currentView === "projectDetails" ||
          (state.analytics.currentView === "messages" &&
            !state.selectedSession))
      ) {
        url.searchParams.set("view", "project");
        url.searchParams.set("project", projectKey(state.selectedProject));
      }
      if (url.searchParams.has("view")) {
        url.searchParams.delete("session");
        url.searchParams.delete("msg");
      }
      if (url.href !== window.location.href)
        window.history.pushState(null, "", url);
    };
    const unsubscribeApp = useAppStore.subscribe((state, previous) => {
      if (
        state.analytics.currentView !== previous.analytics.currentView ||
        state.selectedProject !== previous.selectedProject
      )
        write();
    });
    const unsubscribeBoards = useKanbanStore.subscribe((state, previous) => {
      if (
        state.selectedBoardId !== previous.selectedBoardId ||
        state.data !== previous.data
      )
        write();
    });
    const pop = () => {
      void restore.current();
    };
    window.addEventListener("popstate", pop);
    return () => {
      disposed = true;
      unsubscribeApp();
      unsubscribeBoards();
      window.removeEventListener("popstate", pop);
    };
  }, []);
  useEffect(() => {
    if (
      !loading &&
      (projects.length > 0 ||
        new URL(window.location.href).searchParams.get("view") !== "project") &&
      !ready.current &&
      !restoring.current
    )
      void restore.current();
  }, [projects, loading]);
}
