import type { KanbanAction, KanbanData } from "@/types/kanban";

/** getRandomValues also works on HTTP WebUI origins where randomUUID is absent. */
export function newKanbanId(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

/** Immutable board operations. No operation mutates the project/history entities. */
export function applyKanbanAction(
  data: KanbanData,
  action: KanbanAction,
): KanbanData {
  const next: KanbanData = JSON.parse(JSON.stringify(data));
  if (action.type === "createBoard") {
    if (
      next.boards.some((b) => b.id === action.board.id) ||
      !action.board.name.trim() ||
      !action.board.columns.length
    )
      return data;
    next.boards.push({ ...action.board, name: action.board.name.trim() });
    return next;
  }
  const board = next.boards.find((b) => b.id === action.boardId);
  if (!board) return data;
  switch (action.type) {
    case "renameBoard":
      if (!action.name.trim()) return data;
      board.name = action.name.trim();
      if (action.color) board.color = action.color;
      break;
    case "deleteBoard":
      next.boards = next.boards.filter((b) => b.id !== board.id);
      break;
    case "addColumn":
      if (
        !action.column.name.trim() ||
        board.columns.some((c) => c.id === action.column.id)
      )
        return data;
      board.columns.push({
        ...action.column,
        name: action.column.name.trim(),
        projectIds: [],
      });
      break;
    case "renameColumn": {
      const column = board.columns.find((c) => c.id === action.columnId);
      if (!column || !action.name.trim()) return data;
      column.name = action.name.trim();
      break;
    }
    case "deleteColumn": {
      const column = board.columns.find((c) => c.id === action.columnId);
      const destination = board.columns.find(
        (c) => c.id === action.destinationId,
      );
      if (!column || !destination || column === destination) return data;
      destination.projectIds.push(...column.projectIds);
      board.columns = board.columns.filter((c) => c.id !== column.id);
      break;
    }
    case "moveColumn": {
      const index = board.columns.findIndex((c) => c.id === action.columnId);
      if (index < 0) return data;
      const [column] = board.columns.splice(index, 1);
      board.columns.splice(
        Math.max(0, Math.min(action.index, board.columns.length)),
        0,
        column!,
      );
      break;
    }
    case "addProject": {
      const column = board.columns.find((c) => c.id === action.columnId);
      if (
        !column ||
        board.columns.some((c) => c.projectIds.includes(action.projectId))
      )
        return data;
      column.projectIds.push(action.projectId);
      break;
    }
    case "removeProject":
      board.columns.forEach((c) => {
        c.projectIds = c.projectIds.filter((id) => id !== action.projectId);
      });
      break;
    case "moveProject": {
      const column = board.columns.find((c) => c.id === action.columnId);
      if (
        !column ||
        !board.columns.some((c) => c.projectIds.includes(action.projectId))
      )
        return data;
      if (action.beforeId === action.projectId) return data;
      // Validate the drop anchor before removing anything; stale drops cannot lose a card.
      if (action.beforeId && !column.projectIds.includes(action.beforeId))
        return data;
      board.columns.forEach((c) => {
        c.projectIds = c.projectIds.filter((id) => id !== action.projectId);
      });
      const index = action.beforeId
        ? column.projectIds.indexOf(action.beforeId)
        : column.projectIds.length;
      column.projectIds.splice(index, 0, action.projectId);
      break;
    }
  }
  return next;
}

export function matchesBoardFilter(
  data: KanbanData,
  projectId: string,
  boardIds: string[],
  unassigned: boolean,
): boolean {
  if (!boardIds.length && !unassigned) return true;
  const memberships = data.boards.filter((board) =>
    board.columns.some((column) => column.projectIds.includes(projectId)),
  );
  return (
    (unassigned && !memberships.length) ||
    memberships.some((board) => boardIds.includes(board.id))
  );
}
