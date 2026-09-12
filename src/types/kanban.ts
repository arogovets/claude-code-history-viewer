type ClaudeProject = { path: string; provider?: string };

export interface KanbanColumn {
  id: string;
  name: string;
  /** Ordered project references; containment determines this board's status. */
  projectIds: string[];
}
export interface KanbanBoard {
  color?: string;
  id: string;
  name: string;
  columns: KanbanColumn[];
}
export interface KanbanData {
  revision: number;
  boards: KanbanBoard[];
}

/** Provider + storage path distinguishes providers and custom history directories. */
export const projectMetadataKey = (project: ClaudeProject): string =>
  project.path;
export const projectKey = (project: ClaudeProject): string =>
  JSON.stringify([project.provider ?? "claude", projectMetadataKey(project)]);

export type KanbanAction =
  | { type: "createBoard"; board: KanbanBoard }
  | { type: "renameBoard"; boardId: string; name: string; color?: string }
  | { type: "deleteBoard"; boardId: string }
  | { type: "addColumn"; boardId: string; column: KanbanColumn }
  | { type: "renameColumn"; boardId: string; columnId: string; name: string }
  | {
      type: "deleteColumn";
      boardId: string;
      columnId: string;
      destinationId: string;
    }
  | { type: "moveColumn"; boardId: string; columnId: string; index: number }
  | { type: "addProject"; boardId: string; columnId: string; projectId: string }
  | { type: "removeProject"; boardId: string; projectId: string }
  | {
      type: "moveProject";
      boardId: string;
      columnId: string;
      projectId: string;
      beforeId?: string;
    };
