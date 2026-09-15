import { useTranslation } from "react-i18next";
import { useKanbanStore } from "@/store/useKanbanStore";

export function BoardFilter({
  boardIds,
  unassigned,
  onChange,
}: {
  boardIds: string[];
  unassigned: boolean;
  onChange: (ids: string[], unassigned: boolean) => void;
}) {
  const { t } = useTranslation();
  const boards = useKanbanStore((s) => s.data.boards);
  return (
    <details className="relative text-xs">
      <summary className="cursor-pointer rounded-md border border-border px-3 py-2">
        {t("kanban.filterBoards")}{" "}
        {boardIds.length + Number(unassigned) > 0 &&
          `(${boardIds.length + Number(unassigned)})`}
      </summary>
      <div className="absolute left-0 top-full z-40 mt-1 max-h-72 w-64 overflow-y-auto rounded-lg border border-border bg-popover p-3 shadow-lg">
        <p className="mb-2 text-muted-foreground">{t("kanban.matchAny")}</p>
        <button className="mb-2 underline" onClick={() => onChange([], false)}>
          {t("kanban.allProjects")}
        </button>
        <label className="flex gap-2 py-2">
          <input
            type="checkbox"
            checked={unassigned}
            onChange={(e) => onChange(boardIds, e.target.checked)}
          />
          {t("kanban.unassigned")}
        </label>
        {boards.map((board) => (
          <label key={board.id} className="flex items-center gap-2 py-2">
            <input
              type="checkbox"
              checked={boardIds.includes(board.id)}
              onChange={(e) =>
                onChange(
                  e.target.checked
                    ? [...boardIds, board.id]
                    : boardIds.filter((id) => id !== board.id),
                  unassigned,
                )
              }
            />
            <span
              className="h-2.5 w-2.5 shrink-0 rounded-full"
              style={{ backgroundColor: board.color ?? "#64748b" }}
            />
            <span className="break-words">{board.name}</span>
          </label>
        ))}
      </div>
    </details>
  );
}
