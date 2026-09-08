import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InputHTMLAttributes, ReactNode } from "react";
import { GlobalSearchModal } from "./GlobalSearchModal";

type MockProps = {
    children?: ReactNode;
    open?: boolean;
    [key: string]: unknown;
};

vi.mock("@/components/ui", () => ({
    Dialog: ({ open, children }: MockProps) => (open ? <div>{children}</div> : null),
    DialogContent: ({ children, ...props }: MockProps) => {
        delete props.showCloseButton;
        delete props.overlayClassName;
        return <div {...props}>{children}</div>;
    },
    Input: (props: InputHTMLAttributes<HTMLInputElement>) => <input {...props} />,
    Select: ({ children }: MockProps) => <div>{children}</div>,
    SelectContent: ({ children }: MockProps) => <div>{children}</div>,
    SelectItem: ({ children, ...props }: MockProps) => {
        delete props.textValue;
        return <div {...props}>{children}</div>;
    },
    SelectTrigger: ({ children, ...props }: MockProps) => <button {...props}>{children}</button>,
    SelectValue: ({ children }: MockProps) => <span>{children}</span>,
    Badge: ({ children, ...props }: MockProps) => <div {...props}>{children}</div>,
}));

const { mockApi, storeState, translate, useAppStoreMock } = vi.hoisted(() => {
    const state = {
        claudePath: "",
        projects: [],
        selectProject: vi.fn(),
        selectSession: vi.fn(),
        sessions: [],
        getSessionDisplayName: vi.fn(),
        activeProviders: ["claude"],
        navigateToMessage: vi.fn(),
        clearTargetMessage: vi.fn(),
        setAnalyticsCurrentView: vi.fn(),
        userMetadata: {
            settings: {
                wsl: {
                    enabled: true,
                    excludedDistros: [],
                },
            },
        },
    };

    const store = Object.assign(() => state, {
        getState: () => state,
    });

    return {
        mockApi: vi.fn(),
        storeState: state,
        translate: (key: string, fallback?: string) => fallback ?? key,
        useAppStoreMock: store,
    };
});

vi.mock("@/services/api", () => ({
    api: mockApi,
}));

vi.mock("@/store/useAppStore", () => ({
    useAppStore: useAppStoreMock,
}));

vi.mock("react-i18next", async (importOriginal) => {
    const actual = await importOriginal<typeof import("react-i18next")>();
    return {
        ...actual,
        useTranslation: () => ({
            t: translate,
        }),
    };
});

vi.mock("sonner", () => ({
    toast: {
        error: vi.fn(),
    },
}));

describe("GlobalSearchModal WSL search routing", () => {
    beforeEach(() => {
        vi.clearAllMocks();
        mockApi.mockResolvedValue([]);
        storeState.claudePath = "";
        storeState.activeProviders = ["claude"];
        storeState.userMetadata.settings.wsl.enabled = true;
    });

    it("searches WSL when no native Claude path is configured", async () => {
        storeState.activeProviders = ["claude", "codex"];
        render(<GlobalSearchModal isOpen onClose={vi.fn()} />);

        fireEvent.change(screen.getByPlaceholderText("globalSearch.placeholder"), {
            target: { value: "hello" },
        });

        await waitFor(() => {
            expect(mockApi).toHaveBeenCalledWith(
                "search_all_providers",
                expect.objectContaining({
                    claudePath: undefined,
                    query: "hello",
                    activeProviders: ["claude", "codex"],
                    wslEnabled: true,
                    wslProviders: ["claude"],
                }),
            );
        });
    });

    it("searches a Codex-only installation without a native Claude path", async () => {
        storeState.activeProviders = ["codex"];
        storeState.userMetadata.settings.wsl.enabled = false;

        render(<GlobalSearchModal isOpen onClose={vi.fn()} />);

        fireEvent.change(screen.getByPlaceholderText("globalSearch.placeholder"), {
            target: { value: "hello" },
        });

        await waitFor(() => {
            expect(mockApi).toHaveBeenCalledWith(
                "search_all_providers",
                expect.objectContaining({
                    claudePath: undefined,
                    query: "hello",
                    activeProviders: ["codex"],
                    wslEnabled: false,
                }),
            );
        });
    });

    it("keeps the native search path when native Claude is available", async () => {
        storeState.claudePath = "/home/user/.claude";
        storeState.userMetadata.settings.wsl.enabled = false;

        render(<GlobalSearchModal isOpen onClose={vi.fn()} />);

        fireEvent.change(screen.getByPlaceholderText("globalSearch.placeholder"), {
            target: { value: "hello" },
        });

        await waitFor(() => {
            expect(mockApi).toHaveBeenCalledWith(
                "search_messages",
                expect.objectContaining({
                    claudePath: "/home/user/.claude",
                    query: "hello",
                }),
            );
        });
    });

    it("renders project filter with provider and remote/local badges", () => {
        storeState.projects = [
            {
                name: "cchv",
                path: "/Users/emac/Dev/cchv",
                actual_path: "/Users/emac/Dev/cchv",
                provider: "claude",
                session_count: 5,
                message_count: 50,
                last_modified: "2026-09-07T00:00:00Z",
            },
            {
                name: "master",
                path: "remote://http://100.93.94.80:3728#/home/arogovets/.codex/sessions",
                actual_path: "/home/arogovets/master",
                provider: "codex",
                custom_directory_label: "arogovets@100.93.94.80",
                session_count: 3,
                message_count: 30,
                last_modified: "2026-09-07T00:00:00Z",
            },
        ];

        render(<GlobalSearchModal isOpen onClose={vi.fn()} />);

        // Check project names
        expect(screen.getAllByText("cchv").length).toBeGreaterThanOrEqual(1);
        expect(screen.getAllByText("master").length).toBeGreaterThanOrEqual(1);

        // Check providers
        expect(screen.getAllByText("Claude Code").length).toBeGreaterThanOrEqual(1);
        expect(screen.getAllByText("Codex CLI").length).toBeGreaterThanOrEqual(1);

        // Check local and remote badges
        expect(screen.getAllByText("Local").length).toBeGreaterThanOrEqual(1);
        expect(screen.getAllByText("arogovets@100.93.94.80").length).toBeGreaterThanOrEqual(1);

        // Check exclude section is rendered
        expect(screen.getByText("Exclude project (spam filter)")).toBeDefined();
    });

    it("persists project filter selection in localStorage and restores it", () => {
        localStorage.setItem("cchv_global_search_project_filter", "exclude:/Users/emac/Dev/cchv");
        storeState.projects = [
            {
                name: "cchv",
                path: "/Users/emac/Dev/cchv",
                actual_path: "/Users/emac/Dev/cchv",
                provider: "claude",
                session_count: 5,
                message_count: 50,
                last_modified: "2026-09-07T00:00:00Z",
            },
            {
                name: "master",
                path: "/Users/emac/Dev/master",
                actual_path: "/Users/emac/Dev/master",
                provider: "claude",
                session_count: 3,
                message_count: 30,
                last_modified: "2026-09-07T00:00:00Z",
            },
        ];

        render(<GlobalSearchModal isOpen onClose={vi.fn()} />);
        expect(screen.getAllByText("Exclude:").length).toBeGreaterThanOrEqual(1);
    });
});
