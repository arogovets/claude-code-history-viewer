import type { ClaudeSession } from "@/types";

export function splitSourceIdentity(id: string): { sourceId: string; nativeId: string } {
  const separator = id.indexOf("|");
  if (id.startsWith("source:") && separator > 7) {
    return { sourceId: id.slice(7, separator), nativeId: id.slice(separator + 1) };
  }
  return { sourceId: "", nativeId: id };
}

export const getSourceId = (id?: string): string => splitSourceIdentity(id ?? "").sourceId;

/** Search carries the native session ID; session lists may use a file path.
 * Compare their native IDs only after establishing identical source identity. */
export function sessionMatches(session: ClaudeSession, targetId?: string): boolean {
  if (!targetId) return false;
  const target = splitSourceIdentity(targetId);
  const listed = splitSourceIdentity(session.session_id);
  if (target.sourceId && target.sourceId !== listed.sourceId) return false;
  return session.session_id === targetId || listed.nativeId === target.nativeId ||
    session.actual_session_id === target.nativeId;
}
