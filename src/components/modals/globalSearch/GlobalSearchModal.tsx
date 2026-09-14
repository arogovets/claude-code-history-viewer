import { getSourceId, sessionMatches } from "@/utils/sourceIdentity";
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
    EyeOff,
    Hash,
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
import type { ClaudeMessage, ClaudeProject, ClaudeSession, ContentItem, LocatedSession } from "@/types";
import {
    getProviderLabel,
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
    sourceId: string;
    sourceLabel: string;
    items: GlobalSearchResult[];
};

const getProjectSourceId = (project?: ClaudeProject | null): string => project?.source_id ?? "";
const getProjectSourceLabel = (project: ClaudeProject): string => project.custom_directory_label || project.source_id || "Source";


export const GlobalSearchModal = ({
    isOpen,
    onClose,
}: GlobalSearchModalProps) => {
    const { t } = useTranslation();
    const [query, setQuery] = useState("");
    const [results, setResults] = useState<GlobalSearchResult[]>([]);
    const [sessionResults, setSessionResults] = useState<LocatedSession[]>([]);
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
        projects,
        selectProject,
        selectSession,
        sessions,
        getSessionDisplayName,
        activeProviders,
        navigateToMessage,
        clearTargetMessage,
        setAnalyticsCurrentView,
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

            // Correlate with session in store to identify source identity accurately
            const sourceId = getSourceId(result.sessionId);

            // If an exclude filter is active and this result belongs to the excluded project, skip it
            if (isExcludedFilter && selectedProject) {
                const targetSourceId = getProjectSourceId(selectedProject);
                const nameMatches =
                    projectName === selectedProject.name ||
                    projectName === selectedProject.actual_path?.split(/[\\/]/).pop() ||
                    projectName === selectedProject.path?.split(/[\\/]/).pop();
                const providerMatches =
                    !result.provider ||
                    !selectedProject.provider ||
                    result.provider === selectedProject.provider;
                if (nameMatches && providerMatches && sourceId === targetSourceId) {
                    continue;
                }
            }

            const matchingProject =
                projects.find((project) => {
                    const providerMatches = (project.provider ?? "claude") === resultProvider;
                    const nameMatches = project.name === projectName;
                    if (!providerMatches || !nameMatches) return false;
                    const projectSourceId = getProjectSourceId(project);
                    return sourceId === projectSourceId;
                }) ||
                projects.find(
                    (project) =>
                        (project.provider ?? "claude") === resultProvider &&
                        project.name === projectName
                );

            // If matching project is marked hidden in user metadata and not explicitly selected, skip it
            if (matchingProject && isProjectHidden?.(matchingProject.path)) {
                if (effectiveProjectPath !== matchingProject.path) {
                    continue;
                }
            }

            const providerLabel = getProviderLabel(
                (key, fallback) => t(key, fallback),
                result.provider,
            );
            const sourceLabel = matchingProject
                ? getProjectSourceLabel(matchingProject)
                : sourceId
                  ? "Source"
                  : "Source";
            const groupKey = `${resultProvider}::${sourceId}::${projectName}`;
            const groupLabel = `${projectName} (${providerLabel})`;

            if (!groups.has(groupKey)) {
                groups.set(groupKey, {
                    label: groupLabel,
                    projectName,
                    provider: result.provider,
                    pathUnavailable: matchingProject?.path_status === "unavailable",
                    sourceId,
                    sourceLabel,
                    items: [],
                });
            }
            groups.get(groupKey)!.items.push(result);
        }

        return groups;
    }, [projects, results, isExcludedFilter, selectedProject, effectiveProjectPath, isProjectHidden, t]);

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

            if (trimmedQuery.length < 2) {
                setResults([]);
                setSessionResults([]);
                setIsSearching(false);
                return;
            }

            setIsSearching(true);
            try {
                // Concurrently run session ID search across all providers
                const sessionSearchPromise = api<LocatedSession[]>("search_sessions_by_id", {
                    query: trimmedQuery,
                    limit: 10,
                })
                    .catch(() => [] as LocatedSession[])
                    .then((sessionsFound) => {
                        let filteredSessions = sessionsFound;
                        if (isExcludedFilter && selectedProject) {
                            filteredSessions = filteredSessions.filter(
                                (s) => s.project.path !== selectedProject.path && s.project.name !== selectedProject.name
                            );
                        } else if (effectiveProjectPath !== "all" && selectedProject) {
                            filteredSessions = filteredSessions.filter(
                                (s) => s.project.path === selectedProject.path || s.project.name === selectedProject.name
                            );
                        }
                        setSessionResults(filteredSessions);
                        return filteredSessions;
                    });
                const filters: Record<string, unknown> = {};
                if (!isExcludedFilter && effectiveProjectPath !== "all") {
                    const selected = projects.find((p) => p.path === effectiveProjectPath);
                    if (selected) {
                        const candidates = new Set<string>();
                        if (selected.name) candidates.add(selected.name);
                        const pathLeaf = selected.path.split(/[\\/]/).pop();
                        if (pathLeaf && !pathLeaf.startsWith("source:")) candidates.add(pathLeaf);
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
                const providersToSearch = !isExcludedFilter && selectedProject?.provider
                    ? [selectedProject.provider]
                    : activeProviders;
                const searchResults = await api<GlobalSearchResult[]>("search_all_providers", {
                    query: trimmedQuery, activeProviders: providersToSearch, filters, limit: MAX_RESULTS,
                });

                if (isExcludedFilter && selectedProject) {
                    const selectedSourceId = getProjectSourceId(selectedProject);
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

                        const sessionSourceId = getSourceId(res.sessionId);
                        const sourceMatches = sessionSourceId === selectedSourceId;

                        if (nameMatches && providerMatches && sourceMatches) {
                            return false;
                        }
                        return true;
                    });
                    setResults(filtered);
                } else if (selectedProject) {
                    const selectedSourceId = getProjectSourceId(selectedProject);
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
                            const sessionSourceId =
                                getSourceId(res.sessionId);
                            if (sessionSourceId !== selectedSourceId) {
                                return false;
                            }
                        }
                        return true;
                    });
                    setResults(filtered);
                } else {
                    setResults(searchResults);
                }
                await sessionSearchPromise;
                setSelectedIndex(0);
            } catch (error) {
                console.error("Global search failed:", error);
                setResults([]);
                setSessionResults([]);
                toast.error(t("globalSearch.searchFailed"));
            } finally {
                setIsSearching(false);
            }
        },
        [activeProviders, effectiveProjectPath, isExcludedFilter, selectedProject, projects, sessions, messageTypeFilter, t],
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

    const handleSelectSession = useCallback(
        async (located: LocatedSession) => {
            const toastId = toast.loading(t("globalSearch.openingSession", "Opening session..."));
            try {
                setAnalyticsCurrentView("messages");
                await selectProject(located.project);
                await selectSession(located.session);
                toast.dismiss(toastId);
                onClose();
            } catch (error) {
                console.error("Failed to navigate to located session:", error);
                toast.dismiss(toastId);
                toast.error(t("globalSearch.navigationFailed", "Failed to open session"));
                onClose();
            }
        },
        [selectProject, selectSession, setAnalyticsCurrentView, onClose, t]
    );

    const totalSelectable = sessionResults.length + flattenedResults.length;

    // Keyboard navigation
    const handleKeyDown = useCallback(
        (e: React.KeyboardEvent) => {
            if (totalSelectable === 0) return;

            switch (e.key) {
                case "ArrowDown":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev < totalSelectable - 1 ? prev + 1 : 0,
                    );
                    break;
                case "ArrowUp":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev > 0 ? prev - 1 : totalSelectable - 1,
                    );
                    break;
                case "Enter":
                    e.preventDefault();
                    if (selectedIndex < sessionResults.length) {
                        const target = sessionResults[selectedIndex];
                        if (target) {
                            void handleSelectSession(target);
                        }
                    } else {
                        const targetMsg = flattenedResults[selectedIndex - sessionResults.length];
                        if (targetMsg) {
                            void handleSelectResult(targetMsg);
                        }
                    }
                    break;
                case "Escape":
                    e.preventDefault();
                    onClose();
                    break;
            }
        },
        [totalSelectable, sessionResults, flattenedResults, selectedIndex, handleSelectSession, handleSelectResult, onClose],
    );

    // Scroll selected item into view
    useEffect(() => {
        if (resultsContainerRef.current && totalSelectable > 0) {
            const selectedElement = resultsContainerRef.current.querySelector(
                `[data-index="${selectedIndex}"]`,
            );
            selectedElement?.scrollIntoView({ block: "nearest" });
        }
    }, [selectedIndex, totalSelectable]);

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
            setSessionResults([]);
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

    let currentResultIndex = sessionResults.length;

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
                                                        getProjectSourceId(selectedProject)
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    <Server className="w-2.5 h-2.5 shrink-0" />
                                                    <span className="truncate max-w-[80px]">
                                                        {getProjectSourceLabel(selectedProject)}
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
                                                        getProjectSourceId(selectedProject)
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    <Server className="w-2.5 h-2.5 shrink-0" />
                                                    <span className="truncate max-w-[80px]">
                                                        {getProjectSourceLabel(selectedProject)}
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
                                        const sourceId = getProjectSourceId(project);
                                        const sourceLabel = getProjectSourceLabel(project);
                                        const providerLabel = getProviderLabel((k, fb) => t(k, fb), project.provider);
                                        return (
                                            <SelectItem
                                                key={`exclude:${project.path}`}
                                                value={`exclude:${project.path}`}
                                                textValue={`${t("globalSearch.excludePrefix", "Exclude:")} ${project.name} ${providerLabel} ${sourceLabel}`}
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
                                                                sourceId
                                                                    ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                                    : "bg-muted/70 text-muted-foreground border border-border/50"
                                                            )}
                                                            title={
                                                                project.custom_directory_label ||
                                                                "Source"
                                                            }
                                                        >
                                                            <Server className="w-2.5 h-2.5 shrink-0" />
                                                            <span className="truncate max-w-[140px]">{sourceLabel}</span>
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
                                        const sourceId = getProjectSourceId(project);
                                        const sourceLabel = getProjectSourceLabel(project);
                                        const providerLabel = getProviderLabel((k, fb) => t(k, fb), project.provider);
                                        return (
                                            <SelectItem
                                                key={project.path}
                                                value={project.path}
                                                textValue={`${project.name} ${providerLabel} ${sourceLabel}`}
                                            >
                                                <div className="flex items-center justify-between gap-3 w-full min-w-0 py-0.5">
                                                    <span
                                                        className="truncate font-medium text-xs text-foreground"
                                                        title={project.actual_path || project.path || project.name}
                                                    >
                                                        {project.name}
                                                    </span>
                                                    <div className="flex items-center gap-1.5 shrink-0 ml-auto">
                                                        {/* Source label */}
                                                        <span
                                                            className={cn(
                                                                "px-1.5 py-0.5 text-2xs font-medium rounded flex items-center gap-1 shrink-0",
                                                                sourceId
                                                                    ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                                    : "bg-muted/70 text-muted-foreground border border-border/50"
                                                            )}
                                                            title={
                                                                project.custom_directory_label ||
                                                                "Source"
                                                            }
                                                        >
                                                            <Server className="w-2.5 h-2.5 shrink-0" />
                                                            <span className="truncate max-w-[140px]">{sourceLabel}</span>
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
                    {isSearching && results.length === 0 && sessionResults.length === 0 && (
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

                    {!isSearching && query.trim().length >= 2 && results.length === 0 && sessionResults.length === 0 && (
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

                    {/* Session Matches */}
                    {sessionResults.length > 0 && (
                        <div className="py-2 border-b border-border/40">
                            <div className="px-4 py-1.5 text-xs font-semibold text-muted-foreground bg-muted sticky top-0 truncate flex items-center justify-between z-10">
                                <div className="flex items-center gap-1.5 min-w-0">
                                    <Hash className="w-3.5 h-3.5 text-primary shrink-0" />
                                    <span className="font-semibold text-foreground">
                                        {t("globalSearch.matchingSessions", "Matching Sessions")}
                                    </span>
                                </div>
                                <span className="text-2xs text-muted-foreground font-mono shrink-0 ml-2">
                                    {sessionResults.length}
                                </span>
                            </div>
                            {sessionResults.map((item, sIndex) => {
                                const isSelected = sIndex === selectedIndex;
                                const displayName = getSessionDisplayName(item.session.session_id, item.session.summary) ||
                                    item.session.summary ||
                                    t("globalSearch.untitledSession", "Untitled Session");
                                const providerId = item.session.provider || item.project.provider || "claude";
                                const sourceId = getProjectSourceId(item.project);
                                const sourceLabel = getProjectSourceLabel(item.project);

                                return (
                                    <button
                                        key={item.session.session_id}
                                        data-index={sIndex}
                                        onClick={() => handleSelectSession(item)}
                                        className={cn(
                                            "w-full text-left px-4 py-2.5 hover:bg-muted/50 transition-colors border-b border-border/20 last:border-0",
                                            isSelected && "bg-muted"
                                        )}
                                    >
                                        <div className="flex items-start gap-3">
                                            <div className="flex-1 min-w-0">
                                                <div className="flex items-center gap-2 mb-1 flex-wrap">
                                                    <Badge
                                                        size="sm"
                                                        className={cn(
                                                            "rounded px-1.5 py-0.5 text-2xs font-medium",
                                                            getProviderBadgeStyle(providerId)
                                                        )}
                                                    >
                                                        {getProviderLabel((key, fallback) => t(key, fallback), providerId)}
                                                    </Badge>
                                                    <span className="font-medium text-xs text-foreground truncate max-w-[180px]">
                                                        {item.project.name}
                                                    </span>
                                                    <span
                                                        className={cn(
                                                            "px-1.5 py-0.5 text-2xs font-medium rounded flex items-center gap-1 shrink-0",
                                                            sourceId
                                                                ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                                : "bg-muted/70 text-muted-foreground border border-border/50"
                                                        )}
                                                    >
                                                        <Server className="w-2.5 h-2.5 shrink-0" />
                                                        <span className="truncate max-w-[120px]">{sourceLabel}</span>
                                                    </span>
                                                    {item.session.last_modified && (
                                                        <span className="text-2xs text-muted-foreground ml-auto shrink-0">
                                                            {formatTimestamp(item.session.last_modified)}
                                                        </span>
                                                    )}
                                                </div>
                                                <p className="text-sm font-medium text-foreground truncate mb-1">
                                                    {displayName}
                                                </p>
                                                <div className="flex items-center gap-2 text-2xs text-muted-foreground font-mono truncate">
                                                    <span className="text-muted-foreground/60 shrink-0">ID:</span>
                                                    <span className="truncate bg-muted/60 px-1 py-0.5 rounded text-foreground/80">
                                                        {highlightText(item.session.actual_session_id || item.session.session_id)}
                                                    </span>
                                                    {item.session.message_count > 0 && (
                                                        <span className="shrink-0 text-muted-foreground/70 ml-auto">
                                                            {item.session.message_count} {t("common.messages", "messages")}
                                                        </span>
                                                    )}
                                                </div>
                                            </div>
                                        </div>
                                    </button>
                                );
                            })}
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
                                                        group.sourceId
                                                            ? "bg-sky-500/15 text-sky-700 dark:text-sky-300 border border-sky-500/30"
                                                            : "bg-muted/70 text-muted-foreground border border-border/50"
                                                    )}
                                                >
                                                    <Server className="w-2.5 h-2.5 shrink-0" />
                                                    <span className="truncate max-w-[140px]">{group.sourceLabel}</span>
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
                    {(results.length > 0 || sessionResults.length > 0) && (
                        <span>
                            {t("globalSearch.results", {
                                count: results.length + sessionResults.length,
                            })}
                        </span>
                    )}
                </div>
            </DialogContent>
        </Dialog>
    );
};
