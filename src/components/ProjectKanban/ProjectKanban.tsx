import { projectMetadataKey } from "@/types/kanban";
import { BoardFilter } from "./BoardFilter";
import { ActionMenu } from "./ActionMenu";
import { matchesBoardFilter } from "@/utils/kanban";
import { useEffect, useMemo, useState } from "react";
import type { DragEvent } from "react";
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  GripVertical,
  Pencil,
  Plus,
  Trash2,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useAppStore } from "@/store/useAppStore";
import { useKanbanStore } from "@/store/useKanbanStore";
import type { ClaudeProject } from "@/types";
import { projectKey } from "@/types/kanban";
import { newKanbanId } from "@/utils/kanban";
import { cn } from "@/lib/utils";
import { KanbanFeedback } from "./KanbanFeedback";

const field =
  "w-full rounded-md border border-border bg-background px-3 py-2 text-sm";
type Editor = {
  type:
    | "createBoard"
    | "renameBoard"
    | "deleteBoard"
    | "addColumn"
    | "renameColumn"
    | "deleteColumn";
  columnId?: string;
  boardId?: string;
};
type DragItem = { boardId: string } & (
  | { type: "project"; id: string }
  | { type: "column"; id: string }
);

export function ProjectKanban({
  onOpenProject,
}: {
  onOpenProject: (project: ClaudeProject) => void;
}) {
  const { t } = useTranslation();
  const readOnly = useAppStore((s) => s.isServerReadOnly);
  const projects = useAppStore((s) => s.projects);
  const metadata = useAppStore((s) => s.userMetadata.projects);
  const {
    data,
    loaded,
    pending,
    error,
    selectedBoardId,
    selectBoard,
    load,
    dispatch,
  } = useKanbanStore();
  const board =
    data.boards.find((b) => b.id === selectedBoardId) ?? data.boards[0];
  const locked = pending || !loaded || !!error || readOnly;
  const [editor, setEditor] = useState<Editor | null>(null);
  const [name, setName] = useState("");
  const [color, setColor] = useState("#64748b");
  const [addBoardFilters, setAddBoardFilters] = useState<string[]>([]);
  const [addUnassigned, setAddUnassigned] = useState(false);
  const [destinationId, setDestinationId] = useState("");
  const [adding, setAdding] = useState(false);
  const [query, setQuery] = useState("");
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const projectMap = useMemo(
    () => new Map(projects.map((p) => [projectKey(p), p])),
    [projects],
  );
  const displayName = (project: ClaudeProject) =>
    metadata[projectMetadataKey(project)]?.alias ||
    metadata[project.actual_path]?.alias ||
    project.name;
  const members = new Set(board?.columns.flatMap((c) => c.projectIds));
  const available = projects.filter(
    (p) =>
      !members.has(projectKey(p)) &&
      matchesBoardFilter(data, projectKey(p), addBoardFilters, addUnassigned) &&
      `${displayName(p)} ${p.actual_path} ${p.provider ?? "claude"}`
        .toLowerCase()
        .includes(query.toLowerCase()),
  );

  useEffect(() => {
    void load();
  }, [load]);

  const openEditor = (value: Editor, initialName = "") => {
    setName(initialName);
    setColor(
      value.type === "createBoard" ? "#64748b" : (board?.color ?? "#64748b"),
    );
    setDestinationId(
      board?.columns.find((c) => c.id !== value.columnId)?.id ?? "",
    );
    setEditor({ ...value, boardId: board?.id });
  };
  const editorBoard = data.boards.find((b) => b.id === editor?.boardId);
  // A reload after a conflict may remove the entity being edited. Never apply
  // that draft to the board selected as a fallback.
  useEffect(() => {
    if (
      editor &&
      editor.type !== "createBoard" &&
      (!editorBoard ||
        (editor.columnId &&
          !editorBoard.columns.some((c) => c.id === editor.columnId)))
    ) {
      setEditor(null);
    }
  }, [editor, editorBoard]);

  const saveEditor = async () => {
    if (!editor) return;
    let saved = false;
    if (editor.type === "createBoard") {
      const id = newKanbanId();
      saved = await dispatch({
        type: "createBoard",
        board: {
          id,
          name,
          color,
          columns: ["todo", "inProgress", "done"].map((key) => ({
            id: newKanbanId(),
            name: t(`kanban.${key}`),
            projectIds: [],
          })),
        },
      });
      if (saved) selectBoard(id);
    } else if (editorBoard) {
      const boardId = editorBoard.id;
      switch (editor.type) {
        case "renameBoard":
          saved = await dispatch({ type: "renameBoard", boardId, name, color });
          break;
        case "deleteBoard":
          saved = await dispatch({ type: "deleteBoard", boardId });
          break;
        case "addColumn":
          saved = await dispatch({
            type: "addColumn",
            boardId,
            column: { id: newKanbanId(), name, projectIds: [] },
          });
          break;
        case "renameColumn":
          saved = await dispatch({
            type: "renameColumn",
            boardId,
            columnId: editor.columnId!,
            name,
          });
          break;
        case "deleteColumn":
          saved = await dispatch({
            type: "deleteColumn",
            boardId,
            columnId: editor.columnId!,
            destinationId,
          });
          break;
      }
    }
    if (saved) setEditor(null);
  };
  const startDrag = (event: DragEvent, item: DragItem) => {
    if (locked) {
      event.preventDefault();
      return;
    }
    setDrag(item);
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", item.id);
  };
  const finishDrag = () => {
    setDrag(null);
    setDropTarget(null);
  };
  const dropProject = (
    event: DragEvent,
    columnId: string,
    beforeId?: string,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    if (
      !locked &&
      board &&
      drag?.type === "project" &&
      drag.boardId === board.id
    ) {
      void dispatch({
        type: "moveProject",
        boardId: board.id,
        projectId: drag.id,
        columnId,
        beforeId,
      });
    }
    finishDrag();
  };
  const overProject = (event: DragEvent, target: string) => {
    if (!locked && drag?.type === "project" && drag.boardId === board?.id) {
      event.preventDefault();
      event.stopPropagation();
      setDropTarget(target);
    }
  };
  const deleting =
    editor?.type === "deleteBoard" || editor?.type === "deleteColumn";

  return (
    <section
      className="flex h-full flex-col gap-4 p-4 md:p-6"
      aria-label={t("kanban.title")}
    >
      <div className="flex flex-wrap items-center gap-2">
        <div className="mr-auto">
          <h2 className="text-lg font-semibold">{t("kanban.title")}</h2>
          <p className="text-xs text-muted-foreground">
            {t("kanban.description")}
          </p>
        </div>
        <Button
          variant="outline"
          onClick={() => void load()}
          disabled={pending}
        >
          {t("kanban.reload")}
        </Button>
        <Button
          onClick={() => openEditor({ type: "createBoard" })}
          disabled={locked}
        >
          <Plus className="mr-1 h-4 w-4" />
          {t("kanban.createBoard")}
        </Button>
      </div>
      <KanbanFeedback />
      {!loaded && !error ? (
        <p>{t("kanban.loading")}</p>
      ) : !board ? (
        <div className="m-auto max-w-md text-center">
          <h3 className="text-lg font-medium">{t("kanban.emptyTitle")}</h3>
          <p className="mt-2 text-sm text-muted-foreground">
            {t("kanban.emptyDescription")}
          </p>
        </div>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            <span
              className="h-3 w-3 rounded-full"
              style={{ backgroundColor: board.color ?? "#64748b" }}
            />
            <select
              aria-label={t("kanban.chooseBoard")}
              className={cn(field, "w-auto max-w-xs font-medium")}
              value={board.id}
              disabled={pending}
              onChange={(e) => {
                selectBoard(e.target.value);
                finishDrag();
              }}
            >
              {data.boards.map((b) => (
                <option key={b.id} value={b.id}>
                  {b.name}
                </option>
              ))}
            </select>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("kanban.renameBoard")}
              disabled={locked}
              onClick={() => openEditor({ type: "renameBoard" }, board.name)}
            >
              <Pencil className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("kanban.deleteBoard")}
              disabled={locked}
              onClick={() => openEditor({ type: "deleteBoard" })}
            >
              <Trash2 className="h-4 w-4" />
            </Button>
            <span className="mr-auto text-xs text-muted-foreground">
              {t("kanban.projectCount", { count: members.size })}
            </span>
            <Button
              variant="outline"
              disabled={locked}
              onClick={() => openEditor({ type: "addColumn" })}
            >
              {t("kanban.addColumn")}
            </Button>
            <Button
              variant="outline"
              disabled={locked}
              onClick={() => {
                setQuery("");
                setDestinationId(board.columns[0]?.id ?? "");
                setAdding(true);
              }}
            >
              {t("kanban.addProjects")}
            </Button>
          </div>
          <div className="flex min-h-0 flex-1 gap-4 overflow-x-auto pb-2">
            {board.columns.map((column, columnIndex) => (
              <section
                key={column.id}
                aria-label={column.name}
                className="flex w-72 shrink-0 flex-col rounded-lg border border-border bg-muted/20"
              >
                <div
                  className={cn(
                    "flex flex-wrap items-center gap-1 border-b border-border p-3",
                    dropTarget === `column:${column.id}` && "bg-accent/15",
                  )}
                  onDragOver={(event) => {
                    if (
                      !locked &&
                      drag?.type === "column" &&
                      drag.boardId === board.id
                    ) {
                      event.preventDefault();
                      setDropTarget(`column:${column.id}`);
                    }
                  }}
                  onDrop={(event) => {
                    event.preventDefault();
                    if (
                      !locked &&
                      drag?.type === "column" &&
                      drag.boardId === board.id
                    )
                      void dispatch({
                        type: "moveColumn",
                        boardId: board.id,
                        columnId: drag.id,
                        index: columnIndex,
                      });
                    finishDrag();
                  }}
                >
                  <span
                    draggable={!locked}
                    onDragStart={(e) =>
                      startDrag(e, {
                        type: "column",
                        boardId: board.id,
                        id: column.id,
                      })
                    }
                    onDragEnd={finishDrag}
                    title={t("kanban.dragColumn")}
                    className="cursor-grab text-muted-foreground"
                  >
                    <GripVertical className="h-4 w-4" />
                  </span>
                  <h3 className="min-w-0 flex-1 break-words text-sm font-semibold">
                    {column.name}
                  </h3>
                  <span className="text-xs text-muted-foreground">
                    {column.projectIds.length}
                  </span>
                  <ActionMenu
                    label={t("kanban.columnActions", { name: column.name })}
                  >
                    <div className="flex justify-end">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        aria-label={t("kanban.moveColumnLeft", {
                          name: column.name,
                        })}
                        disabled={locked || columnIndex === 0}
                        onClick={() =>
                          void dispatch({
                            type: "moveColumn",
                            boardId: board.id,
                            columnId: column.id,
                            index: columnIndex - 1,
                          })
                        }
                      >
                        <ArrowLeft className="h-3 w-3" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        aria-label={t("kanban.moveColumnRight", {
                          name: column.name,
                        })}
                        disabled={
                          locked || columnIndex === board.columns.length - 1
                        }
                        onClick={() =>
                          void dispatch({
                            type: "moveColumn",
                            boardId: board.id,
                            columnId: column.id,
                            index: columnIndex + 1,
                          })
                        }
                      >
                        <ArrowRight className="h-3 w-3" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        aria-label={t("kanban.renameColumn", {
                          name: column.name,
                        })}
                        disabled={locked}
                        onClick={() =>
                          openEditor(
                            { type: "renameColumn", columnId: column.id },
                            column.name,
                          )
                        }
                      >
                        <Pencil className="h-3 w-3" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        aria-label={t("kanban.deleteColumn", {
                          name: column.name,
                        })}
                        disabled={locked || board.columns.length === 1}
                        title={
                          board.columns.length === 1
                            ? t("kanban.lastColumn")
                            : undefined
                        }
                        onClick={() =>
                          openEditor({
                            type: "deleteColumn",
                            columnId: column.id,
                          })
                        }
                      >
                        <Trash2 className="h-3 w-3" />
                      </Button>
                    </div>
                  </ActionMenu>
                </div>
                <div
                  className="min-h-24 flex-1 overflow-y-auto p-2"
                  onDragOver={(e) => overProject(e, `end:${column.id}`)}
                  onDrop={(e) => dropProject(e, column.id)}
                >
                  {column.projectIds.map((id, index) => {
                    const project = projectMap.get(id);
                    const label = project
                      ? displayName(project)
                      : t("kanban.unavailableProject");
                    return (
                      <article
                        key={id}
                        draggable={!locked}
                        onDragStart={(e) =>
                          startDrag(e, {
                            type: "project",
                            boardId: board.id,
                            id,
                          })
                        }
                        onDragEnd={finishDrag}
                        onDragOver={(e) => overProject(e, id)}
                        onDrop={(e) => dropProject(e, column.id, id)}
                        className={cn(
                          "mb-2 rounded-md border border-border bg-card p-3 shadow-sm",
                          drag?.id === id && "opacity-50",
                          dropTarget === id && "border-t-4 border-t-accent",
                        )}
                      >
                        <div className="flex items-start gap-1">
                          <button
                            className="min-w-0 flex-1 break-words text-left text-sm font-medium hover:underline disabled:opacity-60"
                            disabled={!project}
                            onClick={() => project && onOpenProject(project)}
                          >
                            {label}
                          </button>
                          <ActionMenu
                            label={t("kanban.projectActions", { name: label })}
                          >
                            <div className="mt-3 flex items-center gap-1">
                              <select
                                className={cn(
                                  field,
                                  "min-w-0 flex-1 px-1 py-1 text-xs",
                                )}
                                aria-label={t("kanban.moveProject", {
                                  name: label,
                                })}
                                value={column.id}
                                disabled={locked}
                                onChange={(e) =>
                                  void dispatch({
                                    type: "moveProject",
                                    boardId: board.id,
                                    projectId: id,
                                    columnId: e.target.value,
                                  })
                                }
                              >
                                {board.columns.map((c) => (
                                  <option key={c.id} value={c.id}>
                                    {c.name}
                                  </option>
                                ))}
                              </select>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7"
                                aria-label={t("kanban.moveUp", { name: label })}
                                disabled={locked || index === 0}
                                onClick={() =>
                                  void dispatch({
                                    type: "moveProject",
                                    boardId: board.id,
                                    columnId: column.id,
                                    projectId: id,
                                    beforeId: column.projectIds[index - 1],
                                  })
                                }
                              >
                                <ArrowUp className="h-3 w-3" />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7"
                                aria-label={t("kanban.moveDown", {
                                  name: label,
                                })}
                                disabled={
                                  locked ||
                                  index === column.projectIds.length - 1
                                }
                                onClick={() =>
                                  void dispatch({
                                    type: "moveProject",
                                    boardId: board.id,
                                    columnId: column.id,
                                    projectId: id,
                                    beforeId: column.projectIds[index + 2],
                                  })
                                }
                              >
                                <ArrowDown className="h-3 w-3" />
                              </Button>
                            </div>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="-mr-1 -mt-1 h-6 w-6 shrink-0"
                              aria-label={t("kanban.removeProject", {
                                name: label,
                              })}
                              disabled={locked}
                              onClick={() =>
                                void dispatch({
                                  type: "removeProject",
                                  boardId: board.id,
                                  projectId: id,
                                })
                              }
                            >
                              <X className="h-3 w-3" />
                            </Button>
                          </ActionMenu>
                        </div>
                        <p
                          className="mt-1 truncate text-xs text-muted-foreground"
                          title={project?.actual_path ?? id}
                        >
                          {project?.actual_path ?? id}
                        </p>
                        <p className="mt-2 text-xs text-muted-foreground">
                          {project
                            ? `${project.provider ?? "claude"} · ${t("kanban.sessionCount", { count: project.session_count })}`
                            : t("kanban.unavailableDescription")}
                        </p>

                        {project && (
                          <p className="mt-3 text-xs text-muted-foreground">
                            <time dateTime={project.last_modified}>
                              {t("kanban.lastUpdated")}:{" "}
                              {new Date(project.last_modified).toLocaleString()}
                            </time>
                          </p>
                        )}
                        <div className="mt-2 flex gap-1.5">
                          {data.boards
                            .filter(
                              (other) =>
                                other.id !== board.id &&
                                other.columns.some((c) =>
                                  c.projectIds.includes(id),
                                ),
                            )
                            .map((other) => (
                              <button
                                key={other.id}
                                className="h-3 w-3 rounded-full border border-border"
                                style={{
                                  backgroundColor: other.color ?? "#64748b",
                                }}
                                title={other.name}
                                aria-label={t("kanban.alsoOnBoard", {
                                  name: other.name,
                                })}
                                onClick={() => selectBoard(other.id)}
                              />
                            ))}
                        </div>
                      </article>
                    );
                  })}
                  <div
                    className={cn(
                      "min-h-16 rounded-md border border-dashed p-3 text-center text-xs text-muted-foreground",
                      dropTarget === `end:${column.id}`
                        ? "border-accent bg-accent/10"
                        : "border-transparent",
                    )}
                  >
                    {column.projectIds.length === 0
                      ? t("kanban.emptyColumn")
                      : drag?.type === "project"
                        ? t("kanban.dropHere")
                        : ""}
                  </div>
                </div>
              </section>
            ))}
          </div>
        </>
      )}
      <Dialog
        open={!!editor}
        onOpenChange={(open) => {
          if (!open && !pending) setEditor(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {editor &&
                t(`kanban.${editor.type}`, {
                  name:
                    board?.columns.find((c) => c.id === editor.columnId)
                      ?.name ?? "",
                })}
            </DialogTitle>
            <DialogDescription>
              {deleting
                ? t(
                    editor?.type === "deleteBoard"
                      ? "kanban.deleteBoardDescription"
                      : "kanban.deleteColumnDescription",
                  )
                : t("kanban.nameHint")}
            </DialogDescription>
          </DialogHeader>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void saveEditor();
            }}
            className="space-y-4"
          >
            {!deleting && (
              <label className="block space-y-1 text-sm">
                <span>{t("kanban.name")}</span>
                <input
                  className={field}
                  autoFocus
                  required
                  maxLength={120}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
            )}
            {(editor?.type === "createBoard" ||
              editor?.type === "renameBoard") && (
              <label className="flex items-center gap-3 text-sm">
                {t("kanban.color")}
                <input
                  aria-label={t("kanban.color")}
                  type="color"
                  value={color}
                  onChange={(e) => setColor(e.target.value)}
                />
              </label>
            )}
            {editor?.type === "deleteBoard" && (
              <p className="break-words font-medium">{board?.name}</p>
            )}
            {editor?.type === "deleteColumn" && (
              <label className="block space-y-1 text-sm">
                <span>{t("kanban.moveProjectsTo")}</span>
                <select
                  className={field}
                  value={destinationId}
                  onChange={(e) => setDestinationId(e.target.value)}
                >
                  {board?.columns
                    .filter((c) => c.id !== editor.columnId)
                    .map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                </select>
              </label>
            )}
            <KanbanFeedback />
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="outline"
                disabled={pending}
                onClick={() => setEditor(null)}
              >
                {t("kanban.cancel")}
              </Button>
              <Button
                type="submit"
                variant={deleting ? "destructive" : "default"}
                disabled={locked || (!deleting && !name.trim())}
              >
                {t(deleting ? "kanban.delete" : "kanban.save")}
              </Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
      <Dialog open={adding} onOpenChange={setAdding}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("kanban.addProjects")}</DialogTitle>
            <DialogDescription>{t("kanban.addDescription")}</DialogDescription>
          </DialogHeader>
          <label className="space-y-1 text-sm">
            <span>{t("kanban.column")}</span>
            <select
              className={field}
              value={destinationId}
              onChange={(e) => setDestinationId(e.target.value)}
            >
              {board?.columns.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          <input
            className={field}
            aria-label={t("kanban.searchProjects")}
            placeholder={t("kanban.searchProjects")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <KanbanFeedback />
          <BoardFilter
            boardIds={addBoardFilters}
            unassigned={addUnassigned}
            onChange={(ids, unassigned) => {
              setAddBoardFilters(ids);
              setAddUnassigned(unassigned);
            }}
          />
          <div className="max-h-72 space-y-1 overflow-y-auto">
            {available.map((project) => (
              <div
                key={projectKey(project)}
                className="flex items-center gap-2 rounded-md border border-border p-2"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">
                    {displayName(project)}
                  </p>
                  <p
                    className="truncate text-xs text-muted-foreground"
                    title={project.actual_path}
                  >
                    {project.provider ?? "claude"} · {project.actual_path}
                  </p>
                </div>
                <Button
                  variant="outline"
                  disabled={locked || !destinationId}
                  aria-label={t("kanban.addProject", {
                    name: displayName(project),
                  })}
                  onClick={() =>
                    board &&
                    void dispatch({
                      type: "addProject",
                      boardId: board.id,
                      columnId: destinationId,
                      projectId: projectKey(project),
                    })
                  }
                >
                  {t("kanban.add")}
                </Button>
              </div>
            ))}
            {!available.length && (
              <p className="p-3 text-sm text-muted-foreground">
                {t("kanban.noProjects")}
              </p>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </section>
  );
}
