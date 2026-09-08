import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import { api } from "@/services/api";
import { useTranslation } from "react-i18next";
import {
    Search,
    ArrowUp,
    ArrowDown,
    CornerDownLeft,
    X,
    Loader2,
    Filter,
    User,
    Bot,
    MessageSquare,
    Lightbulb,
    Server,
    Laptop,
    EyeOff,
} from "lucide-react";
import {
    Dialog,
    DialogContent,
    Input,
    Select,
    SelectContent,
    SelectItem,
    SelectTrigger,
    SelectValue,
    Badge,
} from "@/components/ui";
import { useAppStore } from "@/store/useAppStore";
import type { ClaudeMessage, ClaudeProject, ClaudeSession, ContentItem } from "@/types";
import {
    getProviderLabel,
    getWslSearchableProviderIds,
    hasNonDefaultProvider,
    getProviderBadgeStyle,
} from "@/utils/providers";
import { cn } from "@/lib/utils";
import { toast } from "sonner";

type GlobalSearchResult = ClaudeMessage;

type MessageTypeFilter = "all" | "user" | "assistant";

const PROJECT_FILTER_STORAGE_KEY = "cchv_global_search_project_filter";

const loadPersistedProjectFilter = (): string => {
    try {
        return localStorage.getItem(PROJECT_FILTER_STORAGE_KEY) || "all";
    } catch {
        return "all";
    }
};

const persistProjectFilter = (value: string): void => {
    try {
        localStorage.setItem(PROJECT_FILTER_STORAGE_KEY, value);
    } catch {
        // ignore quota/access errors
    }
};

interface GlobalSearchModalProps {
    isOpen: boolean;
    onClose: () => void;
}

const MAX_RESULTS = 100;

type SearchResultGroup = {
    label: string;
    projectName: string;
    provider?: string;
    pathUnavailable: boolean;
    isRemote: boolean;
    hostLabel: string;
    items: GlobalSearchResult[];
};

const isRemoteProject = (project?: ClaudeProject | null): boolean => {
    if (!project) return false;
    return (
        project.path.startsWith("remote://") ||
        Boolean(
            project.custom_directory_label &&
                (project.custom_directory_label.includes("@") ||
                    project.custom_directory_label.includes(":"))
        )
    );
};

const getProjectHostLabel = (
    project: ClaudeProject,
    t: (key: string, fallback?: string) => string,
    compact = false
): string => {
    const isRemote = isRemoteProject(project);
    if (isRemote) {
        if (project.custom_directory_label) {
            if (compact && project.custom_directory_label.includes("@")) {
                return project.custom_directory_label.split("@")[1] || project.custom_directory_label;
            }
            return project.custom_directory_label;
        }
        return t("common.remote", "Remote");
    }
    if (project.custom_directory_label) {
        return project.custom_directory_label;
    }
    return t("common.local", "Local");
};

const sessionMatches = (s: ClaudeSession, targetId?: string): boolean => {
    if (!targetId) return false;
    if (s.session_id === targetId || s.actual_session_id === targetId) return true;
    const cleanTarget = (targetId.includes("#") ? targetId.split("#")[1] : targetId) || targetId;
    const cleanSession = (s.session_id.includes("#") ? s.session_id.split("#")[1] : s.session_id) || s.session_id;
    return (
        s.actual_session_id === cleanTarget ||
        cleanSession === cleanTarget ||
        s.session_id === cleanTarget ||
        (cleanSession ? cleanSession.endsWith(cleanTarget) : false)
    );
};

export const GlobalSearchModal = ({
    isOpen,
    onClose,
}: GlobalSearchModalProps) => {
    const { t } = useTranslation();
    const [query, setQuery] = useState("");
    const [results, setResults] = useState<GlobalSearchResult[]>([]);
    const [isSearching, setIsSearching] = useState(false);
    const [resolvingResultUuid, setResolvingResultUuid] = useState<string | null>(null);
    const [selectedIndex, setSelectedIndex] = useState(0);
    const [messageTypeFilter, setMessageTypeFilter] = useState<MessageTypeFilter>("all");
    const inputRef = useRef<HTMLInputElement>(null);
    const resultsContainerRef = useRef<HTMLDivElement>(null);
    const debounceTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    // Bumped on every result click and on close — cancels an in-flight
    // session-resolution sweep so it stops issuing project requests.
    const resolveTokenRef = useRef(0);

    const {
        claudePath,
        projects,
        selectProject,
        selectSession,
        sessions,
        getSessionDisplayName,
        activeProviders,
        navigateToMessage,
        clearTargetMessage,
        setAnalyticsCurrentView,
        userMetadata,
        isProjectHidden,
    } = useAppStore();

    const [selectedProjectPath, setSelectedProjectPathState] = useState<string>(loadPersistedProjectFilter);

    const setSelectedProjectPath = useCallback((value: string) => {
        setSelectedProjectPathState(value);
        persistProjectFilter(value);
    }, []);

    const isExcludedFilter = selectedProjectPath.startsWith("exclude:");
    const effectiveProjectPath = isExcludedFilter
        ? selectedProjectPath.slice("exclude:".length)
        : selectedProjectPath;

    const selectedProject = useMemo(
        () => projects.find((p) => p.path === effectiveProjectPath),
        [projects, effectiveProjectPath]
    );

    // Group results by project name
    const groupedResults = useMemo(() => {
        const groups = new Map<string, SearchResultGroup>();

        for (const result of results) {
            const projectName =
                result.projectName || t("globalSearch.unknownProject");
            const resultProvider = result.provider ?? "claude";

            // Correlate with session in store to identify remote vs local accurately
            const matchingSession = sessions.find((s) => sessionMatches(s, result.sessionId));
            const isRemote = matchingSession
                ? matchingSession.session_id.startsWith("remote://") ||
                  matchingSession.file_path.startsWith("remote://")
                : false;

            // If an exclude filter is active and this result belongs to the excluded project, skip it
            if (isExcludedFilter && selectedProject) {
                const isTargetRemote = isRemoteProject(selectedProject);
                const nameMatches =
                    projectName === selectedProject.name ||
                    projectName === selectedProject.actual_path?.split(/[\\/]/).pop() ||
                    projectName === selectedProject.path?.split(/[\\/]/).pop();
                const providerMatches =
                    !result.provider ||
                    !selectedProject.provider ||
                    result.provider === selectedProject.provider;
                if (nameMatches && providerMatches && isRemote === isTargetRemote) {
                    continue;
                }
            }

            const matchingProject =
                projects.find((project) => {
                    const providerMatches = (project.provider ?? "claude") === resultProvider;
                    const nameMatches = project.name === projectName;
                    if (!providerMatches || !nameMatches) return false;
                    const pRemote = isRemoteProject(project);
                    return isRemote ? pRemote : !pRemote;
                }) ||
                projects.find(
                    (project) =>
                        (project.provider ?? "claude") === resultProvider &&
                        project.name === projectName
                );

            // If matching project is marked hidden in user metadata and not explicitly selected, skip it
            if (matchingProject && isProjectHidden?.(matchingProject.actual_path || matchingProject.path)) {
                if (effectiveProjectPath !== matchingProject.path) {
                    continue;
                }
            }

            const providerLabel = getProviderLabel(
                (key, fallback) => t(key, fallback),
                result.provider,
            );
            const hostLabel = matchingProject
                ? getProjectHostLabel(matchingProject, t, false)
                : isRemote
                  ? t("common.remote", "Remote")
                  : t("common.local", "Local");
            const groupKey = `${resultProvider}::${isRemote ? "remote" : "local"}::${projectName}`;
            const groupLabel = `${projectName} (${providerLabel})`;

            if (!groups.has(groupKey)) {
                groups.set(groupKey, {
                    label: groupLabel,
                    projectName,
                    provider: result.provider,
                    pathUnavailable: matchingProject?.path_status === "unavailable",
                    isRemote,
                    hostLabel,
                    items: [],
                });
            }
            groups.get(groupKey)!.items.push(result);
        }

        return groups;
    }, [projects, results, sessions, isExcludedFilter, selectedProject, effectiveProjectPath, isProjectHidden, t]);

    // Flatten grouped results for keyboard navigation
    const flattenedResults = useMemo(() => {
        const flat: GlobalSearchResult[] = [];
        for (const group of groupedResults.values()) {
            flat.push(...group.items);
        }
        return flat;
    }, [groupedResults]);

    // Get session display name for a search result
    const getSessionName = useCallback((result: GlobalSearchResult): string | undefined => {
        if (!result.sessionId || result.sessionId === "unknown-session") return undefined;
        const cleanId = (result.sessionId.includes("#") ? result.sessionId.split("#")[1] : result.sessionId) || result.sessionId;
        const name = getSessionDisplayName(cleanId) || getSessionDisplayName(result.sessionId);
        if (name) return name;
        // No custom/known name: show a short, stable conversation handle so results
        // from different conversations are still distinguishable (#420).
        return t("globalSearch.conversationId", { id: cleanId.slice(0, 8) });
    }, [getSessionDisplayName, t]);

    // Debounced search
    const performSearch = useCallback(
        async (searchQuery: string) => {
            const trimmedQuery = searchQuery.trim();

            const hasNonClaudeProviders = hasNonDefaultProvider(activeProviders);
            const customClaudePaths = userMetadata?.settings?.customClaudePaths;
            const hasCustomPaths = (customClaudePaths?.length ?? 0) > 0;
            const wslEnabled = userMetadata?.settings?.wsl?.enabled ?? false;
            const hasAlternativeSource = hasNonClaudeProviders || hasCustomPaths || wslEnabled;
            const nativeClaudePath = claudePath || undefined;
            const wslProviders = wslEnabled ? getWslSearchableProviderIds(activeProviders) : undefined;

            if (trimmedQuery.length < 2 || (!claudePath && !hasAlternativeSource)) {
                setResults([]);
                setIsSearching(false);
                return;
            }

            setIsSearching(true);
            try {
                const filters: Record<string, unknown> = {};
                if (!isExcludedFilter && effectiveProjectPath !== "all") {
                    const selected = projects.find((p) => p.path === effectiveProjectPath);
                    if (selected) {
                        const candidates = new Set<string>();
                        if (selected.name) candidates.add(selected.name);
                        const pathLeaf = selected.path.split(/[\\/]/).pop();
                        if (pathLeaf && !pathLeaf.startsWith("remote://")) candidates.add(pathLeaf);
                        if (selected.actual_path) {
                            const actualLeaf = selected.actual_path.split(/[\\/]/).pop();
                            if (actualLeaf) candidates.add(actualLeaf);
                        }
                        filters.projects = Array.from(candidates);
                    } else {
                        const dirName = effectiveProjectPath.split(/[\\/]/).pop() || effectiveProjectPath;
                        filters.projects = [dirName];
                    }
                }
                if (messageTypeFilter !== "all") {
                    filters.messageType = messageTypeFilter;
                }
                const wslExcludedDistros = userMetadata?.settings?.wsl?.excludedDistros ?? [];
                const useAllProvidersSearch = hasNonClaudeProviders || hasCustomPaths || wslEnabled;
                const providersToSearch = !isExcludedFilter && selectedProject?.provider
                    ? [selectedProject.provider]
                    : activeProviders;

                const searchResults = await api<GlobalSearchResult[]>(
                    useAllProvidersSearch ? "search_all_providers" : "search_messages",
                    useAllProvidersSearch
                        ? {
                              claudePath: nativeClaudePath,
                              query: trimmedQuery,
                              activeProviders: providersToSearch,
                              filters,
                              limit: MAX_RESULTS,
                              customClaudePaths: hasCustomPaths ? customClaudePaths : undefined,
                              wslEnabled,
                              wslProviders,
                              wslExcludedDistros,
                          }
                        : { claudePath: nativeClaudePath, query: trimmedQuery, filters, limit: MAX_RESULTS },
                );

                if (isExcludedFilter && selectedProject) {
                    const isSelectedRemote = isRemoteProject(selectedProject);
                    const filtered = searchResults.filter((res) => {
                        const nameMatches =
                            res.projectName &&
                            (res.projectName === selectedProject.name ||
                                res.projectName === selectedProject.actual_path?.split(/[\\/]/).pop() ||
                                res.projectName === selectedProject.path?.split(/[\\/]/).pop());
                        const providerMatches =
                            !res.provider ||
                            !selectedProject.provider ||
                            res.provider === selectedProject.provider;

                        const matchingSession = sessions.find((s) => sessionMatches(s, res.sessionId));
                        const isSessionRemote = matchingSession
                            ? matchingSession.session_id.startsWith("remote://") ||
                              matchingSession.file_path.startsWith("remote://")
                            : false;
                        const remoteMatches = isSessionRemote === isSelectedRemote;

                        if (nameMatches && providerMatches && remoteMatches) {
                            return false;
                        }
                        return true;
                    });
                    setResults(filtered);
                } else if (selectedProject) {
                    const isSelectedRemote = isRemoteProject(selectedProject);
                    const filtered = searchResults.filter((res) => {
                        if (
                            res.provider &&
                            selectedProject.provider &&
                            res.provider !== selectedProject.provider
                        ) {
                            return false;
                        }
                        const matchingSession = sessions.find((s) => sessionMatches(s, res.sessionId));
                        if (matchingSession) {
                            const isSessionRemote =
                                matchingSession.session_id.startsWith("remote://") ||
                                matchingSession.file_path.startsWith("remote://");
                            if (isSessionRemote !== isSelectedRemote) {
                                return false;
                            }
                        }
                        return true;
                    });
                    setResults(filtered);
                } else {
                    setResults(searchResults);
                }
                setSelectedIndex(0);
            } catch (error) {
                console.error("Global search failed:", error);
                setResults([]);
                toast.error(t("globalSearch.searchFailed"));
            } finally {
                setIsSearching(false);
            }
        },
        [claudePath, activeProviders, effectiveProjectPath, isExcludedFilter, selectedProject, projects, sessions, messageTypeFilter, userMetadata, t],
    );

    // Handle input change with debounce
    const handleInputChange = useCallback(
        (e: React.ChangeEvent<HTMLInputElement>) => {
            const value = e.target.value;
            setQuery(value);

            if (debounceTimeoutRef.current) {
                clearTimeout(debounceTimeoutRef.current);
            }

            debounceTimeoutRef.current = setTimeout(() => {
                performSearch(value);
            }, 300);
        },
        [performSearch],
    );

    // Navigate to selected result
    const handleSelectResult = useCallback(
        async (result: GlobalSearchResult) => {
            if (resolvingResultUuid) return;
            const targetKey = result.uuid || result.sessionId;
            setResolvingResultUuid(targetKey);
            const toastId = toast.loading(t("globalSearch.openingSession", "Opening session..."));

            try {
                const targetSession = sessions.find((s) => sessionMatches(s, result.sessionId));

                if (targetSession) {
                    // Ensure the conversation pane is the active view — otherwise
                    // a result clicked while in analytics/tokenStats/etc. loads the
                    // session but stays hidden behind the other view (issue #390).
                    setAnalyticsCurrentView("messages");
                    await selectSession(targetSession);
                    if (result.uuid) {
                        navigateToMessage(result.uuid, { history: "replace" });
                    }
                    toast.dismiss(toastId);
                    onClose();
                    return;
                }

                // Snapshot excludeSidechain once to keep requests consistent
                // across the scan and avoid repeated getState() calls. The
                // setting is user-configurable; taking a snapshot is intentional
                // so a mid-scan toggle does not change half the requests.
                const { excludeSidechain } = useAppStore.getState();
                const token = ++resolveTokenRef.current;

                // 1. Fast indexed lookup (<1ms) via locate_session
                try {
                    const located = await api<{ project: ClaudeProject; session: ClaudeSession } | null>(
                        "locate_session",
                        { sessionId: result.sessionId },
                    );
                    if (token !== resolveTokenRef.current) return;
                    if (located?.project && located?.session) {
                        setAnalyticsCurrentView("messages");
                        await selectProject(located.project);
                        await selectSession(located.session);
                        if (result.uuid) {
                            navigateToMessage(result.uuid, { history: "replace" });
                        }
                        toast.dismiss(toastId);
                        onClose();
                        return;
                    }
                } catch {
                    // Fall back to candidate scanning
                }

                // The search result carries the project name and provider —
                // rank matching projects first so the common case resolves in
                // ONE request instead of sweeping every project. The rest are
                // still tried (defensively) but in parallel batches with an
                // early exit, not one serial await per project.
                const resultProvider = result.provider ?? "claude";
                const rank = (project: (typeof projects)[number]): number => {
                    const projectProvider = project.provider ?? "claude";
                    if (projectProvider !== resultProvider) return 3;
                    if (result.projectName && project.name === result.projectName) return 0;
                    if (
                        result.projectName &&
                        (project.name.includes(result.projectName) ||
                            project.actual_path?.endsWith(result.projectName))
                    ) {
                        return 1;
                    }
                    return 2;
                };
                const candidates = [...projects].sort((a, b) => rank(a) - rank(b));

                const findInProject = async (
                    project: (typeof projects)[number],
                ): Promise<{ project: typeof project; session: ClaudeSession } | null> => {
                    try {
                        const projectProvider = project.provider ?? "claude";
                        const projectSessions = await api<ClaudeSession[]>(
                            projectProvider !== "claude" ? "load_provider_sessions" : "load_project_sessions",
                            projectProvider !== "claude"
                                ? { provider: projectProvider, projectPath: project.path, excludeSidechain }
                                : { projectPath: project.path, excludeSidechain },
                        );
                        const session = projectSessions.find((s) => sessionMatches(s, result.sessionId));
                        return session ? { project, session } : null;
                    } catch (error) {
                        console.error(
                            `Failed to load sessions for project ${project.name}:`,
                            error,
                        );
                        return null;
                    }
                };

                const BATCH_SIZE = 6;
                for (let i = 0; i < candidates.length; i += BATCH_SIZE) {
                    if (token !== resolveTokenRef.current) return; // cancelled
                    const batch = candidates.slice(i, i + BATCH_SIZE);
                    const found = (await Promise.all(batch.map(findInProject))).find(
                        (hit): hit is NonNullable<typeof hit> => hit !== null,
                    );
                    if (token !== resolveTokenRef.current) return; // cancelled
                    if (found) {
                        setAnalyticsCurrentView("messages");
                        await selectProject(found.project);
                        await selectSession(found.session);
                        if (result.uuid) {
                            navigateToMessage(result.uuid, { history: "replace" });
                        }
                        toast.dismiss(toastId);
                        onClose();
                        return;
                    }
                }

                // Session not found in any project
                clearTargetMessage();
                toast.error(t("globalSearch.sessionNotFound", "Session not found"));
                onClose();
            } catch (error) {
                clearTargetMessage();
                console.error("Failed to navigate to search result:", error);
                toast.error(t("globalSearch.navigationFailed", "Failed to open session"));
                onClose();
            } finally {
                toast.dismiss(toastId);
                setResolvingResultUuid(null);
            }
        },
        [resolvingResultUuid, projects, sessions, selectProject, selectSession, navigateToMessage, clearTargetMessage, setAnalyticsCurrentView, onClose, t],
    );

    // Keyboard navigation
    const handleKeyDown = useCallback(
        (e: React.KeyboardEvent) => {
            if (flattenedResults.length === 0) return;

            switch (e.key) {
                case "ArrowDown":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev < flattenedResults.length - 1 ? prev + 1 : 0,
                    );
                    break;
                case "ArrowUp":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev > 0 ? prev - 1 : flattenedResults.length - 1,
                    );
                    break;
                case "Enter":
                    e.preventDefault();
                    if (flattenedResults[selectedIndex]) {
                        handleSelectResult(flattenedResults[selectedIndex]);
                    }
                    break;
                case "Escape":
                    e.preventDefault();
                    onClose();
                    break;
            }
        },
        [flattenedResults, selectedIndex, handleSelectResult, onClose],
    );

    // Scroll selected item into view
    useEffect(() => {
        if (resultsContainerRef.current && flattenedResults.length > 0) {
            const selectedElement = resultsContainerRef.current.querySelector(
                `[data-index="${selectedIndex}"]`,
            );
            selectedElement?.scrollIntoView({ block: "nearest" });
        }
    }, [selectedIndex, flattenedResults.length]);

    // Focus input when modal opens
    useEffect(() => {
        if (isOpen) {
            setTimeout(() => inputRef.current?.focus(), 0);
        } else {
            // Cancel any in-flight result-resolution sweep.
            resolveTokenRef.current++;
            setResolvingResultUuid(null);
            setQuery("");
            setResults([]);
            setSelectedIndex(0);
            setMessageTypeFilter("all");
        }
    }, [isOpen]);

    // Re-search when filters change. `query` is intentionally omitted —
    // keystroke-driven searches go through handleInputChange's debounce.
    // This effect only fires when performSearch identity changes (i.e., filter deps).
    useEffect(() => {
        if (query.trim().length >= 2) {
            performSearch(query);
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [performSearch]);

    // Cleanup debounce on unmount
    useEffect(() => {
        return () => {
            if (debounceTimeoutRef.current) {
                clearTimeout(debounceTimeoutRef.current);
            }
        };
    }, []);

    // Get preview text centered around the search term
    const getPreviewText = (message: GlobalSearchResult): string => {
        if (!message.content) return t("globalSearch.noPreview");

        const content = message.content;
        let fullText = "";

        if (typeof content === "string") {
            fullText = content;
        } else if (Array.isArray(content)) {
            const texts: string[] = [];
            for (const item of content as ContentItem[]) {
                if (item.type === "text" && "text" in item) {
                    texts.push(item.text as string);
                }
            }
            fullText = texts.join(" ");
        }

        if (!fullText) return t("globalSearch.noPreview");

        // Find search term position and show surrounding context
        const trimmedQuery = query.trim().toLowerCase();
        if (trimmedQuery.length >= 2) {
            const lowerText = fullText.toLowerCase();
            const matchIndex = lowerText.indexOf(trimmedQuery);
            if (matchIndex !== -1) {
                const contextRadius = 60;
                const start = Math.max(0, matchIndex - contextRadius);
                const end = Math.min(fullText.length, matchIndex + trimmedQuery.length + contextRadius);
                const slice = fullText.slice(start, end);
                const prefix = start > 0 ? "..." : "";
                const suffix = end < fullText.length ? "..." : "";
                return prefix + slice + suffix;
            }
        }

        return fullText.slice(0, 150) + (fullText.length > 150 ? "..." : "");
    };

    // Format timestamp
    const formatTimestamp = (timestamp: string): string => {
        try {
            const date = new Date(timestamp);
            return date.toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
            });
        } catch {
            return "";
        }
    };

    // Memoize regex to avoid re-creation per result item
    const highlightRegex = useMemo(() => {
        const trimmed = query.trim();
        if (!trimmed) return null;
        return new RegExp(
            `(${trimmed.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`,
            "i",
        );
    }, [query]);

    const highlightText = (text: string): React.ReactNode => {
        if (!highlightRegex) return text;

        const parts = text.split(highlightRegex);
        return parts.map((part, index) =>
            highlightRegex.test(part) ? (
                <mark
                    key={index}
                    className="bg-yellow-300 dark:bg-yellow-500/40 text-foreground rounded-sm px-0.5"
                >
                    {part}
                </mark>
            ) : (
                part
            ),
        );
    };

    let currentResultIndex = 0;

    return (
        <Dialog open={isOpen} onOpenChange={(open) => !open && onClose()}>
            <DialogContent
                className="sm:max-w-2xl p-0 gap-0 overflow-hidden"
                onKeyDown={handleKeyDown}
                showCloseButton={false}
                aria-label={t("globalSearch.title")}
            >
                {/* Search Header */}
                <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
                    <Search className="w-4 h-4 text-muted-foreground shrink-0" />
                    <Input
                        ref={inputRef}
                        type="text"
                        value={query}
                        onChange={handleInputChange}
                        placeholder={t("globalSearch.placeholder")}
                        className="border-0 shadow-none focus-visible:ring-0 px-0 h-auto text-sm"
                        autoComplete="off"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck={false}
                    />
                    {isSearching && (
                        <Loader2 className="w-4 h-4 text-muted-foreground animate-spin shrink-0" />
                    )}
                    {query && !isSearching && (
                        <button
                            onClick={() => {
                                setQuery("");
                                setResults([]);
                                inputRef.current?.focus();
                            }}
                            className="p-1 hover:bg-muted rounded"
                            aria-label={t("globalSearch.clearSearch")}
                        >
                            <X className="w-3 h-3 text-muted-foreground" />
                        </button>
                    )}
                </div>

                {/* Filters Bar */}
                <div className="flex items-center gap-2 px-4 py-2 border-b border-border bg-muted/20">
                    {/* Message Type Filter */}
                    <div className="flex items-center gap-1">
                        {(["all", "user", "assistant"] as const).map((type) => (
                            <button
                                key={type}
                                onClick={() => setMessageTypeFilter(type)}
                                className={cn(
                                    "flex items-center gap-1 px-2 py-1 text-xs rounded-md transition-colors",
                                    messageTypeFilter === type
                                        ? "bg-foreground/10 text-foreground font-medium"
                                        : "text-muted-foreground hover:text-foreground hover:bg-muted"
                                )}
                                aria-label={t(`globalSearch.filterType.${type}`)}
                            >
                                {type === "all" && <MessageSquare className="w-3 h-3" />}
                                {type === "user" && <User className="w-3 h-3" />}
                                {type === "assistant" && <Bot className="w-3 h-3" />}
                                <span>{t(`globalSearch.filterType.${type}`)}</span>
                            </button>
                        ))}
                    </div>

                    {/* Divider */}
                    {projects.length > 1 && (
                        <div className="w-px h-4 bg-border" />
                    )}

                    {/* Project Filter */}
                    {projects.length > 1 && (
                        <>
                            <Filter className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                            <Select value={selectedProjectPath} onValueChange={setSelectedProjectPath}>
                                <SelectTrigger
                                    className="h-7 text-xs border-border min-w-[130px] max-w-[280px] w-auto shrink-0"
                                    aria-label={t("globalSearch.allProjects")}
                                >
                                    <SelectValue placeholder={t("globalSearch.allProjects")}>
                                        {isExcludedFilter && selectedProject ? (
                                            <div className="flex items-center gap-1.5 min-w-0 max-w-full text-amber-600 dark:text-amber-400">
                                                <EyeOff className="w-3.5 h-3.5 shrink-0 text-amber-500" />
                                                <span className="font-semibold text-xs tracking-tight">
                                                    {t("globalSearch.excludeBadge", "Exclude")}:
                                                </span>
                                                <span className="truncate font-medium">
                                                    {selectedProject.name}
                                                </span>
                                                <span
                                                    className={cn(
                                                        "px-1 py-0 text-[10px] leading-tight font-medium rounded shrink-0 border border-current/20",
                                                        getProviderBadgeStyle(selectedProject.provider)
                                                    )}
                                                >
                                                    {getProviderLabel((k, fb) => t(k, fb), selectedProject.provider)}
                                                </span>
                                                <span
                                                    className={cn(
                                                        "px-1 py-0 text-[10px] leading-tight font-medium rounded flex items-center gap-0.5 shrink-0",
                                                        isRemoteProject(selectedProject)
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    {isRemoteProject(selectedProject) ? (
                                                        <Server className="w-2.5 h-2.5 shrink-0" />
                                                    ) : (
                                                        <Laptop className="w-2.5 h-2.5 shrink-0" />
                                                    )}
                                                    <span className="truncate max-w-[80px]">
                                                        {getProjectHostLabel(selectedProject, t, true)}
                                                    </span>
                                                </span>
                                            </div>
                                        ) : selectedProject ? (
                                            <div className="flex items-center gap-1.5 min-w-0 max-w-full">
                                                <span className="truncate font-medium">
                                                    {selectedProject.name}
                                                </span>
                                                <span
                                                    className={cn(
                                                        "px-1 py-0 text-[10px] leading-tight font-medium rounded shrink-0 border border-current/20",
                                                        getProviderBadgeStyle(selectedProject.provider)
                                                    )}
                                                >
                                                    {getProviderLabel((k, fb) => t(k, fb), selectedProject.provider)}
                                                </span>
                                                <span
                                                    className={cn(
                                                        "px-1 py-0 text-[10px] leading-tight font-medium rounded flex items-center gap-0.5 shrink-0",
                                                        isRemoteProject(selectedProject)
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    {isRemoteProject(selectedProject) ? (
                                                        <Server className="w-2.5 h-2.5 shrink-0" />
                                                    ) : (
                                                        <Laptop className="w-2.5 h-2.5 shrink-0" />
                                                    )}
                                                    <span className="truncate max-w-[80px]">
                                                        {getProjectHostLabel(selectedProject, t, true)}
                                                    </span>
                                                </span>
                                            </div>
                                        ) : (
                                            <span className="truncate">{t("globalSearch.allProjects")}</span>
                                        )}
                                    </SelectValue>
                                </SelectTrigger>
                                <SelectContent className="max-h-80 min-w-[24rem] max-w-[34rem]">
                                    <SelectItem value="all" textValue={t("globalSearch.allProjects")}>
                                        <div className="flex items-center justify-between w-full py-0.5">
                                            <span className="font-medium text-xs">
                                                {t("globalSearch.allProjects")}
                                            </span>
                                            <span className="text-2xs text-muted-foreground font-mono ml-2">
                                                {projects.length}
                                            </span>
                                        </div>
                                    </SelectItem>

                                    {/* Exclude Section */}
                                    <div className="px-2 py-1 text-2xs font-semibold text-amber-600 dark:text-amber-400 uppercase tracking-wider flex items-center gap-1 mt-1 border-t border-border pt-1.5">
                                        <EyeOff className="w-3 h-3" />
                                        <span>{t("globalSearch.excludeSection", "Exclude project (spam filter)")}</span>
                                    </div>
                                    {projects.map((project) => {
                                        const isRemote = isRemoteProject(project);
                                        const hostLabel = getProjectHostLabel(project, t, false);
                                        const providerLabel = getProviderLabel((k, fb) => t(k, fb), project.provider);
                                        return (
                                            <SelectItem
                                                key={`exclude:${project.path}`}
                                                value={`exclude:${project.path}`}
                                                textValue={`${t("globalSearch.excludePrefix", "Exclude:")} ${project.name} ${providerLabel} ${hostLabel}`}
                                            >
                                                <div className="flex items-center justify-between gap-3 w-full min-w-0 py-0.5">
                                                    <div className="flex items-center gap-1.5 min-w-0">
                                                        <EyeOff className="w-3 h-3 text-amber-500 shrink-0" />
                                                        <span className="text-amber-600 dark:text-amber-400 font-semibold text-2xs uppercase tracking-wider shrink-0">
                                                            {t("globalSearch.excludePrefix", "Exclude:")}
                                                        </span>
                                                        <span
                                                            className="truncate font-medium text-xs text-foreground"
                                                            title={project.actual_path || project.path || project.name}
                                                        >
                                                            {project.name}
                                                        </span>
                                                    </div>
                                                    <div className="flex items-center gap-1.5 shrink-0 ml-auto">
                                                        <span
                                                            className={cn(
                                                                "px-1.5 py-0.5 text-2xs font-medium rounded flex items-center gap-1 shrink-0",
                                                                isRemote
                                                                    ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                                    : "bg-muted/70 text-muted-foreground border border-border/50"
                                                            )}
                                                            title={
                                                                project.custom_directory_label ||
                                                                (isRemote ? t("common.remote", "Remote") : t("common.local", "Local"))
                                                            }
                                                        >
                                                            {isRemote ? (
                                                                <Server className="w-2.5 h-2.5 shrink-0" />
                                                            ) : (
                                                                <Laptop className="w-2.5 h-2.5 shrink-0" />
                                                            )}
                                                            <span className="truncate max-w-[140px]">{hostLabel}</span>
                                                        </span>
                                                        <span
                                                            className={cn(
                                                                "px-1.5 py-0.5 text-2xs font-medium rounded shrink-0 border border-current/20 leading-tight",
                                                                getProviderBadgeStyle(project.provider)
                                                            )}
                                                        >
                                                            {providerLabel}
                                                        </span>
                                                    </div>
                                                </div>
                                            </SelectItem>
                                        );
                                    })}

                                    {/* Include Section */}
                                    <div className="px-2 py-1 text-2xs font-semibold text-muted-foreground uppercase tracking-wider mt-1 border-t border-border pt-1.5">
                                        {t("globalSearch.includeSection", "Only in project")}
                                    </div>
                                    {projects.map((project) => {
                                        const isRemote = isRemoteProject(project);
                                        const hostLabel = getProjectHostLabel(project, t, false);
                                        const providerLabel = getProviderLabel((k, fb) => t(k, fb), project.provider);
                                        return (
                                            <SelectItem
                                                key={project.path}
                                                value={project.path}
                                                textValue={`${project.name} ${providerLabel} ${hostLabel}`}
                                            >
                                                <div className="flex items-center justify-between gap-3 w-full min-w-0 py-0.5">
                                                    <span
                                                        className="truncate font-medium text-xs text-foreground"
                                                        title={project.actual_path || project.path || project.name}
                                                    >
                                                        {project.name}
                                                    </span>
                                                    <div className="flex items-center gap-1.5 shrink-0 ml-auto">
                                                        {/* Remote / Local Badge */}
                                                        <span
                                                            className={cn(
                                                                "px-1.5 py-0.5 text-2xs font-medium rounded flex items-center gap-1 shrink-0",
                                                                isRemote
                                                                    ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                                    : "bg-muted/70 text-muted-foreground border border-border/50"
                                                            )}
                                                            title={
                                                                project.custom_directory_label ||
                                                                (isRemote ? t("common.remote", "Remote") : t("common.local", "Local"))
                                                            }
                                                        >
                                                            {isRemote ? (
                                                                <Server className="w-2.5 h-2.5 shrink-0" />
                                                            ) : (
                                                                <Laptop className="w-2.5 h-2.5 shrink-0" />
                                                            )}
                                                            <span className="truncate max-w-[140px]">{hostLabel}</span>
                                                        </span>

                                                        {/* Provider Badge */}
                                                        <span
                                                            className={cn(
                                                                "px-1.5 py-0.5 text-2xs font-medium rounded shrink-0 border border-current/20 leading-tight",
                                                                getProviderBadgeStyle(project.provider)
                                                            )}
                                                        >
                                                            {providerLabel}
                                                        </span>
                                                    </div>
                                                </div>
                                            </SelectItem>
                                        );
                                    })}
                                </SelectContent>
                            </Select>
                        </>
                    )}
                </div>

                {/* Results */}
                <div
                    ref={resultsContainerRef}
                    className="max-h-100 overflow-y-auto"
                >
                    {/* Loading skeleton */}
                    {isSearching && results.length === 0 && (
                        <div className="py-4 space-y-3 px-4">
                            {Array.from({ length: 4 }).map((_, i) => (
                                <div key={i} className="animate-pulse">
                                    <div className="flex items-center gap-2 mb-1.5">
                                        <div className="h-4 w-12 bg-muted rounded" />
                                        <div className="h-3 w-20 bg-muted rounded" />
                                    </div>
                                    <div className="h-4 w-full bg-muted rounded mb-1" />
                                    <div className="h-4 w-3/4 bg-muted rounded" />
                                </div>
                            ))}
                        </div>
                    )}

                    {!isSearching && query.trim().length >= 2 && results.length === 0 && (
                        <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                            {t("globalSearch.noResults")}
                        </div>
                    )}

                    {/* Empty state with search tips */}
                    {!query && (
                        <div className="px-6 py-8 space-y-4">
                            <div className="text-center">
                                <Search className="w-8 h-8 text-muted-foreground/40 mx-auto mb-3" />
                                <p className="text-sm text-muted-foreground">
                                    {t("globalSearch.hint")}
                                </p>
                            </div>
                            <div className="space-y-2">
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.minChars")}</span>
                                </div>
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.filters")}</span>
                                </div>
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.navigate")}</span>
                                </div>
                            </div>
                        </div>
                    )}

                    {/* Typing but not enough chars */}
                    {query && query.trim().length < 2 && !isSearching && (
                        <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                            {t("globalSearch.tips.minChars")}
                        </div>
                    )}

                    {results.length > 0 && (
                        <div className="py-2">
                            {Array.from(groupedResults.entries()).map(
                                ([groupKey, group]) => (
                                    <div key={groupKey}>
                                        {/* Project Header */}
                                        <div className="px-4 py-1.5 text-xs font-medium text-muted-foreground bg-muted sticky top-0 truncate flex items-center justify-between">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <span className="font-semibold text-foreground truncate">
                                                    {group.projectName}
                                                </span>
                                                {group.provider && (
                                                    <Badge
                                                        size="sm"
                                                        className={cn(
                                                            "rounded px-1 py-0 text-2xs",
                                                            getProviderBadgeStyle(group.provider)
                                                        )}
                                                    >
                                                        {getProviderLabel((key, fallback) => t(key, fallback), group.provider)}
                                                    </Badge>
                                                )}
                                                <span
                                                    className={cn(
                                                        "px-1.5 py-0.5 text-2xs font-medium rounded flex items-center gap-1 shrink-0",
                                                        group.isRemote
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted/70 text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    {group.isRemote ? (
                                                        <Server className="w-2.5 h-2.5 shrink-0" />
                                                    ) : (
                                                        <Laptop className="w-2.5 h-2.5 shrink-0" />
                                                    )}
                                                    <span className="truncate max-w-[140px]">{group.hostLabel}</span>
                                                </span>
                                                {group.pathUnavailable && (
                                                    <Badge
                                                        size="sm"
                                                        className="rounded px-1 py-0 text-2xs bg-amber-500/15 text-amber-700 dark:text-amber-300"
                                                        title={t("project.pathUnavailableDescription", {
                                                            defaultValue: "Last-known location is unavailable",
                                                        })}
                                                    >
                                                        {t("project.pathUnavailable", "Location unavailable")}
                                                    </Badge>
                                                )}
                                            </div>
                                            <span className="text-2xs text-muted-foreground font-mono shrink-0 ml-2">
                                                {group.items.length}
                                            </span>
                                        </div>

                                        {/* Results in this project */}
                                        {group.items.map((result) => {
                                            const index = currentResultIndex++;
                                            const isSelected = index === selectedIndex;
                                            const isResolvingThis = resolvingResultUuid === (result.uuid || result.sessionId);

                                            return (
                                                <button
                                                    key={result.uuid}
                                                    data-index={index}
                                                    disabled={!!resolvingResultUuid}
                                                    onClick={() => handleSelectResult(result)}
                                                    className={cn(
                                                        "w-full text-left px-4 py-2.5 hover:bg-muted/50 transition-colors",
                                                        isSelected && "bg-muted",
                                                        isResolvingThis && "bg-primary/10 opacity-90 cursor-wait"
                                                    )}
                                                >
                                                    <div className="flex items-start gap-3">
                                                        <div className="flex-1 min-w-0">
                                                            <div className="flex items-center gap-2 mb-1">
                                                                <span
                                                                    className={cn(
                                                                        "inline-flex items-center gap-1 text-xs px-1.5 py-0.5 rounded font-medium",
                                                                        result.type === "user"
                                                                            ? "bg-blue-500/10 text-blue-500"
                                                                            : result.type === "assistant"
                                                                              ? "bg-amber-500/10 text-amber-500"
                                                                              : "bg-gray-500/10 text-gray-500"
                                                                    )}
                                                                >
                                                                    {result.type === "user" && <User className="w-3 h-3" />}
                                                                    {result.type === "assistant" && <Bot className="w-3 h-3" />}
                                                                    {result.type}
                                                                </span>
                                                                <span className="text-xs text-muted-foreground">
                                                                    {formatTimestamp(result.timestamp)}
                                                                </span>
                                                                {isResolvingThis && (
                                                                    <span className="inline-flex items-center gap-1 text-xs text-primary font-medium ml-auto animate-pulse">
                                                                        <Loader2 className="w-3 h-3 animate-spin shrink-0" />
                                                                        {t("common.loading", "Loading...")}
                                                                    </span>
                                                                )}
                                                            </div>
                                                            {(() => {
                                                                const sessionName = getSessionName(result);
                                                                return sessionName ? (
                                                                    <p className="flex items-center gap-1 text-xs text-muted-foreground/70 mb-0.5">
                                                                        <MessageSquare className="w-3 h-3 shrink-0" />
                                                                        <span className="truncate">{sessionName}</span>
                                                                    </p>
                                                                ) : null;
                                                            })()}
                                                            <p className="text-sm text-foreground line-clamp-2">
                                                                {highlightText(getPreviewText(result))}
                                                            </p>
                                                        </div>
                                                    </div>
                                                </button>
                                            );
                                        })}
                                    </div>
                                ),
                            )}
                        </div>
                    )}
                </div>

                {/* Footer with keyboard hints */}
                <div className="flex items-center justify-between px-4 py-2 border-t border-border bg-muted/30 text-xs text-muted-foreground">
                    <div className="flex items-center gap-4">
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <ArrowUp className="w-3 h-3 inline" />
                            </kbd>
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <ArrowDown className="w-3 h-3 inline" />
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.navigate")}
                            </span>
                        </div>
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <CornerDownLeft className="w-3 h-3 inline" />
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.select")}
                            </span>
                        </div>
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono text-px10">
                                esc
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.close")}
                            </span>
                        </div>
                    </div>
                    {results.length > 0 && (
                        <span>
                            {t("globalSearch.results", {
                                count: results.length,
                            })}
                        </span>
                    )}
                </div>
            </DialogContent>
        </Dialog>
    );
};
