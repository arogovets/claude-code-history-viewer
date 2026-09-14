import { describe, it, expect, vi, beforeEach } from "vitest";
import { create } from "zustand";

const { mockApi } = vi.hoisted(() => ({
  mockApi: vi.fn(),
}));

vi.mock("@/services/api", () => ({
  api: mockApi,
}));

import { createProjectSlice } from "../store/slices/projectSlice";
import { createSearchSlice } from "../store/slices/searchSlice";
import { createMetadataSlice } from "../store/slices/metadataSlice";
import { createSettingsSlice } from "../store/slices/settingsSlice";
import { createProviderSlice } from "../store/slices/providerSlice";
import type { AppStore } from "../store/useAppStore";

// Minimal store: only the slices to read via get().
// Remaining fields required by AppStore are stubbed with vi.fn() / defaults.
function createTestStore() {
  return create<AppStore>()((...args) => ({
    ...createProjectSlice(...args),
    ...createSearchSlice(...args),
    ...createMetadataSlice(...args),
    ...createSettingsSlice(...args),
    ...createProviderSlice(...args),

    messages: [],
    isLoadingMessages: false,
    hasMoreMessages: false,
    currentPage: 0,
    messageError: null,
    loadMessages: vi.fn(),
    loadMoreMessages: vi.fn(),
    clearMessages: vi.fn(),
    setTargetMessage: vi.fn(),
    targetMessage: null,
    clearTargetMessage: vi.fn(),
    navigateToMessage: vi.fn(),
    analyticsData: null,
    isLoadingAnalytics: false,
    analyticsError: null,
    loadAnalytics: vi.fn(),
    globalStats: null,
    isLoadingGlobalStats: false,
    globalStatsError: null,
    loadGlobalStats: vi.fn(),
    captureMode: false,
    setCaptureMode: vi.fn(),
    boards: [],
    isLoadingBoards: false,
    boardError: null,
    loadBoards: vi.fn(),
    createBoard: vi.fn(),
    updateBoard: vi.fn(),
    deleteBoard: vi.fn(),
    filters: {},
    setFilters: vi.fn(),
    resetFilters: vi.fn(),
    currentRoute: null,
    navigate: vi.fn(),
    goBack: vi.fn(),
    watchedPaths: [],
    startWatcher: vi.fn(),
    stopWatcher: vi.fn(),
    navigator: null,
    initNavigator: vi.fn(),
    archiveSession: vi.fn(),
    archivedSessions: [],
    isLoadingArchive: false,
    archiveError: null,
    loadArchivedSessions: vi.fn(),
    sessionPicker: null,
    openSessionPicker: vi.fn(),
    closeSessionPicker: vi.fn(),
  } as unknown as AppStore));
}

function seedStore(
  store: ReturnType<typeof createTestStore>,
  wslEnabled: boolean,
  excludedDistros: string[] = [],
  claudePath = "/home/user/.claude",
) {
  store.setState({
    claudePath,
    userMetadata: {
      version: 1,
      sessions: {},
      projects: {},
      settings: { wsl: { enabled: wslEnabled, excludedDistros } },
    },
    activeProviders: ["claude"],
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  mockApi.mockResolvedValue([]);
});

describe("filesystem source routing ignores legacy host-discovery settings", () => {
  it.each([true, false])("scans and searches mirrors with legacy WSL enabled=%s", async (enabled) => {
    const store = createTestStore();
    seedStore(store, enabled, ["Debian"], "");
    const legacy = [{ path: "/old/claude", label: "Retained" }];
    store.setState({ userMetadata: { ...store.getState().userMetadata,
      settings: { ...store.getState().userMetadata.settings, customClaudePaths: legacy } } });
    await store.getState().scanProjects();
    expect(mockApi).toHaveBeenCalledWith("scan_all_projects", { activeProviders: ["claude"] });
    await store.getState().searchMessages("hello");
    expect(mockApi).toHaveBeenCalledWith("search_all_providers", {
      query: "hello", activeProviders: ["claude"], filters: {},
    });
    expect(store.getState().userMetadata.settings.customClaudePaths).toEqual(legacy);
    expect(mockApi.mock.calls.map(([command]) => command)).toEqual(["scan_all_projects", "search_all_providers"]);
  });
  it("searches selected providers across all registered sources", async () => {
    const store = createTestStore();
    seedStore(store, false, [], "");
    store.setState({ activeProviders: ["claude", "codex"] });
    await store.getState().searchMessages("hello");
    expect(mockApi).toHaveBeenCalledWith("search_all_providers", {
      query: "hello", activeProviders: ["claude", "codex"], filters: {},
    });
  });
});
