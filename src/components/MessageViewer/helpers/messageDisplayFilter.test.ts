import { describe, expect, it } from "vitest";
import type { ClaudeMessage } from "../../../types";
import type { MessageFilter } from "../../../store/slices/filterSlice";
import { applyMessageDisplayFilter } from "./messageDisplayFilter";

const defaultFilter: MessageFilter = {
  roles: { user: true, assistant: true },
  contentTypes: {
    text: true,
    thinking: true,
    toolCalls: true,
    commands: true,
    parallelTasks: true,
  },
};

const makeMessage = (
  uuid: string,
  overrides: Record<string, unknown>,
): ClaudeMessage => {
  const type = (overrides.type as string) || "user";
  const role = (overrides.role as string) || (type === "assistant" ? "assistant" : "user");
  return {
    uuid,
    type,
    role,
    timestamp: "2026-07-07T00:00:00.000Z",
    content: "",
    ...overrides,
  } as unknown as ClaudeMessage;
};

describe("applyMessageDisplayFilter", () => {
  it("returns all messages when all filters are enabled", () => {
    const messages = [
      makeMessage("user-1", { type: "user", content: "Hello" }),
      makeMessage("asst-1", { type: "assistant", content: "Hi" }),
    ];
    expect(applyMessageDisplayFilter(messages, defaultFilter)).toEqual(messages);
  });

  it("filters out user messages when user role is disabled", () => {
    const messages = [
      makeMessage("user-1", { type: "user", content: "Hello" }),
      makeMessage("asst-1", { type: "assistant", content: "Hi" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      roles: { user: false, assistant: true },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["asst-1"]);
  });

  it("filters out assistant messages when assistant role is disabled", () => {
    const messages = [
      makeMessage("user-1", { type: "user", content: "Hello" }),
      makeMessage("asst-1", { type: "assistant", content: "Hi" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      roles: { user: true, assistant: false },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["user-1"]);
  });

  it("hides text-only user messages and text-only assistant messages when text filter is disabled", () => {
    const messages = [
      makeMessage("user-text", { type: "user", content: "User prompt" }),
      makeMessage("user-array-text", {
        type: "user",
        content: [{ type: "text", text: "Array prompt" }],
      }),
      makeMessage("asst-text", { type: "assistant", content: "Assistant reply" }),
      makeMessage("asst-thinking-and-text", {
        type: "assistant",
        content: [
          { type: "thinking", thinking: "pondering..." },
          { type: "text", text: "result text" },
        ],
      }),
      makeMessage("asst-tool-and-text", {
        type: "assistant",
        content: [
          { type: "tool_use", id: "t1", name: "bash" },
          { type: "text", text: "running bash" },
        ],
      }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      contentTypes: { ...defaultFilter.contentTypes, text: false },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual([
      "asst-thinking-and-text",
      "asst-tool-and-text",
    ]);
  });

  it("hides thinking-only messages when thinking filter is disabled", () => {
    const messages = [
      makeMessage("asst-thinking-only", {
        type: "assistant",
        content: [{ type: "thinking", thinking: "pondering..." }],
      }),
      makeMessage("asst-text", { type: "assistant", content: "Assistant reply" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      contentTypes: { ...defaultFilter.contentTypes, thinking: false },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["asst-text"]);
  });

  it("hides tool-call-only messages when toolCalls filter is disabled", () => {
    const messages = [
      makeMessage("asst-tool-only", {
        type: "assistant",
        content: [{ type: "tool_use", id: "t1", name: "bash" }],
      }),
      makeMessage("asst-text", { type: "assistant", content: "Assistant reply" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      contentTypes: { ...defaultFilter.contentTypes, toolCalls: false },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["asst-text"]);
  });

  it("hides command messages when commands filter is disabled", () => {
    const messages = [
      makeMessage("user-cmd", {
        type: "user",
        content: "<command-name>npm test</command-name><command-args></command-args>",
      }),
      makeMessage("user-text", { type: "user", content: "Regular text message" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      contentTypes: { ...defaultFilter.contentTypes, commands: false },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["user-text"]);
  });

  it("preserves command messages when text filter is disabled but commands is enabled", () => {
    const messages = [
      makeMessage("user-cmd", {
        type: "user",
        content: "<command-name>npm test</command-name><command-args></command-args>",
      }),
      makeMessage("user-text", { type: "user", content: "Regular text message" }),
    ];
    const filter: MessageFilter = {
      ...defaultFilter,
      contentTypes: { ...defaultFilter.contentTypes, text: false, commands: true },
    };
    const result = applyMessageDisplayFilter(messages, filter);
    expect(result.map((m) => m.uuid)).toEqual(["user-cmd"]);
  });
});

