import { describe, expect, it } from "vitest";
import type { ClaudeSession } from "@/types";
import { getSourceId, sessionMatches } from "./sourceIdentity";

describe("filesystem source identity", () => {
  const session = { session_id: "source:laptop|/mirrors/laptop/current/s.jsonl", actual_session_id: "same" } as ClaudeSession;
  it("matches native search IDs within the same source", () => {
    expect(sessionMatches(session, "source:laptop|same")).toBe(true);
    expect(sessionMatches(session, "source:desktop|same")).toBe(false);
    expect(sessionMatches(session, session.session_id)).toBe(true);
  });
  it("keeps readable labels out of identity", () => {
    expect(getSourceId(session.session_id)).toBe("laptop");
    expect(getSourceId("unqualified")).toBe("");
  });
});
