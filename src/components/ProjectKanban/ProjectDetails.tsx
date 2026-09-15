import { projectMetadataKey } from "@/types/kanban";
import { ProjectMetadataEditor } from "./ProjectMetadataEditor";
import { SessionList } from "@/components/ProjectTree/components/SessionList";
import { formatDateCompact } from "@/utils/time";
import type { ClaudeSession } from "@/types";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useAppStore } from "@/store/useAppStore";
import { useKanbanStore } from "@/store/useKanbanStore";
import type { ClaudeProject } from "@/types";
import { projectKey } from "@/types/kanban";
import { KanbanFeedback } from "./KanbanFeedback";

export function ProjectDetails({
  project,
  onSessionSelect,
}: {
  project: ClaudeProject;
  onSessionSelect?: (session: ClaudeSession) => void;
}) {
  const { t } = useTranslation();
  const readOnly = useAppStore((s) => s.isServerReadOnly);
  const metadata = useAppStore((s) => s.userMetadata.projects);
  const setView = useAppStore((s) => s.setAnalyticsCurrentView);
  const { data, loaded, pending, error, load, dispatch, selectBoard } =
    useKanbanStore();
  const sessions = useAppStore((s) => s.sessions);
  const sessionsTotal = useAppStore((s) => s.sessionsTotal);
  const hasMoreSessions = useAppStore((s) => s.hasMoreSessions);
  const isLoadingSessions = useAppStore((s) => s.isLoadingSessions);
  const isLoadingMoreSessions = useAppStore((s) => s.isLoadingMoreSessions);
  const loadMoreSessions = useAppStore((s) => s.loadMoreSessions);
  const id = projectKey(project);
  const locked = pending || !loaded || !!error || readOnly;
  useEffect(() => {
    void load();
  }, [load]);

  return (
    <section
      className="h-full overflow-y-auto p-4 md:p-6"
      aria-label={t("kanban.projectDetails")}
    >
      <div className="mx-auto max-w-3xl space-y-6">
        <div>
          <p className="text-xs text-muted-foreground">
            {t("kanban.projectDetails")}
          </p>
          <h2 className="mt-1 break-words text-xl font-semibold">
            {metadata[projectMetadataKey(project)]?.alias ||
              metadata[project.actual_path]?.alias ||
              project.name}
          </h2>
          <p className="mt-2 break-all text-sm text-muted-foreground">
            {project.actual_path}
          </p>
          <p className="mt-2 text-sm">
            {project.provider ?? "claude"} ·{" "}
            {t("kanban.sessionCount", { count: project.session_count })}
          </p>
        </div>
        <ProjectMetadataEditor key={id} project={project} />
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="font-semibold">{t("kanban.memberships")}</h3>
          <Button variant="outline" onClick={() => setView("kanban")}>
            {t("kanban.title")}
          </Button>
        </div>
        <p className="text-sm text-muted-foreground">
          {t("kanban.membershipsDescription")}
        </p>
        <KanbanFeedback />
        {!loaded && !error && <p>{t("kanban.loading")}</p>}
        {loaded && !data.boards.length && (
          <p className="text-sm text-muted-foreground">
            {t("kanban.noBoards")}
          </p>
        )}
        <div className="space-y-3">
          {data.boards.map((board) => {
            const column = board.columns.find((c) => c.projectIds.includes(id));
            return (
              <div
                key={board.id}
                className="flex flex-wrap items-center gap-3 rounded-lg border border-border p-4"
              >
                <label className="flex min-w-0 flex-1 items-center gap-3 text-sm font-medium">
                  <input
                    type="checkbox"
                    checked={!!column}
                    disabled={locked}
                    aria-label={t("kanban.membership", { name: board.name })}
                    onChange={(e) => {
                      if (e.target.checked && board.columns[0])
                        void dispatch({
                          type: "addProject",
                          boardId: board.id,
                          columnId: board.columns[0].id,
                          projectId: id,
                        });
                      else
                        void dispatch({
                          type: "removeProject",
                          boardId: board.id,
                          projectId: id,
                        });
                    }}
                  />
                  <span
                    className="h-3 w-3 shrink-0 rounded-full"
                    style={{ backgroundColor: board.color ?? "#64748b" }}
                  />
                  <span className="break-words">{board.name}</span>
                </label>
                {column && (
                  <select
                    className="max-w-full rounded-md border border-border bg-background px-2 py-2 text-sm"
                    aria-label={t("kanban.statusOnBoard", { name: board.name })}
                    value={column.id}
                    disabled={locked}
                    onChange={(e) =>
                      void dispatch({
                        type: "moveProject",
                        boardId: board.id,
                        columnId: e.target.value,
                        projectId: id,
                      })
                    }
                  >
                    {board.columns.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                  </select>
                )}
                <Button
                  variant="ghost"
                  onClick={() => {
                    selectBoard(board.id);
                    setView("kanban");
                  }}
                >
                  {t("kanban.openBoard")}
                </Button>
              </div>
            );
          })}
        </div>
        <section
          aria-label={t("kanban.sessions")}
          className="rounded-lg border border-border"
        >
          <h3 className="p-3 font-semibold">{t("kanban.sessions")}</h3>
          <SessionList
            sessions={sessions ?? []}
            sessionsTotal={sessionsTotal}
            hasMoreSessions={hasMoreSessions}
            selectedSession={null}
            isLoading={isLoadingSessions}
            isLoadingMoreSessions={isLoadingMoreSessions}
            onLoadMoreSessions={() => void loadMoreSessions()}
            formatTimeAgo={formatDateCompact}
            onSessionSelect={(session) => {
              if (onSessionSelect) onSessionSelect(session);
              else {
                setView("messages");
                void useAppStore.getState().selectSession(session);
              }
            }}
          />
        </section>
      </div>
    </section>
  );
}
