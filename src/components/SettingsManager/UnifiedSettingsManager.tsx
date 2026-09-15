/**
 * UnifiedSettingsManager Component
 *
 * Refactored settings manager with improved UX:
 * - Sidebar for scope switching (always visible)
 * - Integrated preset panel
 * - Accordion sections for settings
 * - MCP servers as a section, not a tab
 */

import * as React from "react";
import { api } from "@/services/api";
import { Button } from "@/components/ui/button";
import { useAnalyticsNavigation } from "@/hooks/analytics/useAnalyticsNavigation";
import { useAppStore } from "@/store/useAppStore";
import type { AllSettingsResponse, SettingsScope, ClaudeCodeSettings, MCPServerConfig, MCPSource } from "@/types";
import { mappedProjectPath } from "@/utils/providerSettings";
import type { FilesystemSource } from "@/types/filesystemSources";
import type { ProviderSettingsSpec, ProviderSettingsState, ProviderSettingsSelection } from "@/types/providerSettings";
import { SettingsEditorPane } from "./editor/SettingsEditorPane";
import { PresetPanel } from "./sidebar/PresetPanel";
import { SourcesDirectoriesSection } from "./sections/SourcesDirectoriesSection";

export type ActivePanel = "editor" | "diagnostics";

// ============================================================================
// Types
// ============================================================================

interface UnifiedSettingsManagerProps {
  projectPath?: string;
  sourceId?: string;
  className?: string;
}

export interface SettingsManagerContextValue {
  // Settings state
  allSettings: AllSettingsResponse | null;
  activeScope: SettingsScope;
  setActiveScope: (scope: SettingsScope) => void;
  currentSettings: ClaudeCodeSettings;
  isReadOnly: boolean;
  projectPath?: string;
  setProjectPath: (path: string | undefined) => void;

  // Panel state
  activePanel: ActivePanel;
  setActivePanel: (panel: ActivePanel) => void;

  // Pending changes state (for dirty tracking across components)
  pendingSettings: ClaudeCodeSettings | null;
  setPendingSettings: React.Dispatch<React.SetStateAction<ClaudeCodeSettings | null>>;
  hasUnsavedChanges: boolean;

  // MCP state
  mcpServers: {
    userClaudeJson: Record<string, MCPServerConfig>;
    localClaudeJson: Record<string, MCPServerConfig>;
    userSettings: Record<string, MCPServerConfig>;
    userMcpFile: Record<string, MCPServerConfig>;
    projectMcpFile: Record<string, MCPServerConfig>;
  };
  saveMCPServers: (source: MCPSource, servers: Record<string, MCPServerConfig>, targetProjectPath?: string) => Promise<void>;

  // Actions
  loadSettings: () => Promise<void>;
  saveSettings: (settings: ClaudeCodeSettings, targetScope?: SettingsScope, targetProjectPath?: string) => Promise<void>;
}

// Create context
// eslint-disable-next-line react-refresh/only-export-components
export const SettingsManagerContext = React.createContext<SettingsManagerContextValue | null>(null);

// Hook to use context
// eslint-disable-next-line react-refresh/only-export-components
export const useSettingsManager = () => {
  const context = React.useContext(SettingsManagerContext);
  if (!context) {
    throw new Error("useSettingsManager must be used within UnifiedSettingsManager");
  }
  return context;
};

// ============================================================================
// Main Component
// ============================================================================

export const UnifiedSettingsManager: React.FC<UnifiedSettingsManagerProps> = ({ projectPath, sourceId: initialSourceId, className }) => {
  const { switchToArchive } = useAnalyticsNavigation();
  const [sources, setSources] = React.useState<FilesystemSource[]>([]);
  const [providers, setProviders] = React.useState<ProviderSettingsSpec[]>([]);
  const [sourceId, setSourceId] = React.useState(initialSourceId ?? "");
  const [projectInput, setProjectInput] = React.useState(projectPath ?? "");
  const [chosenProject, setChosenProject] = React.useState(projectPath ?? "");
  const [provider, setProvider] = React.useState("claude");
  const [scope, setScope] = React.useState("user");
  const [refreshKey, refresh] = React.useReducer((n: number) => n + 1, 0);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    let active = true;
    Promise.all([api<FilesystemSource[]>("list_filesystem_sources"), api<ProviderSettingsSpec[]>("list_provider_settings")])
      .then(([nextSources, nextProviders]) => {
        if (!active) return;
        setSources(nextSources); setProviders(nextProviders);
        setSourceId((id) => nextSources.some((s) => s.id === id) ? id : nextSources[0]?.id ?? "");
      }).catch((reason: unknown) => { if (active) setError(String(reason)); });
    return () => { active = false; };
  }, [refreshKey]);
  const selected = providers.find((p) => p.provider === provider);
  const source = sources.find((s) => s.id === sourceId);
  const mirroredProject = source ? mappedProjectPath(source, chosenProject) : null;
  const selection = { sourceId, provider, scope, projectPath: mirroredProject };
  const projectScope = selected?.settings.scopes.find((s) => s.id === scope)?.base === "project";
  return <div className={`space-y-4 ${className ?? ""}`}>
    <div className="flex justify-between items-center"><h2 className="text-xl font-semibold">Source provider settings</h2>
      <Button variant="ghost" onClick={switchToArchive}>Archive Manager</Button></div>
    <div className="flex flex-wrap gap-3">
      <label>Source<select aria-label="Source" className="block bg-background border rounded p-2" value={sourceId} onChange={(e) => setSourceId(e.target.value)}>
        {sources.map((source) => <option key={source.id} value={source.id}>{source.label} ({source.id})</option>)}
      </select></label>
      <label>Provider<select aria-label="Provider" className="block bg-background border rounded p-2" value={provider} onChange={(e) => { setProvider(e.target.value); setScope("user"); }}>
        {providers.map((p) => <option key={p.provider} value={p.provider}>{p.provider === "claude" ? "Claude Code" : p.provider === "codex" ? "Codex" : p.provider === "antigravity" ? "Antigravity" : p.provider === "opencode" ? "OpenCode" : p.provider}</option>)}
      </select></label>
      <label>Scope<select aria-label="Scope" className="block bg-background border rounded p-2" value={scope} onChange={(e) => setScope(e.target.value)}>
        {selected?.settings.scopes.map((s) => <option key={s.id} value={s.id}>{s.label}{s.editable ? "" : " · read-only"}</option>)}
      </select></label>
      <Button variant="outline" onClick={refresh}>Refresh sources</Button>
    </div>
    {projectScope && <div className="space-y-2">
      <label className="block">Project directory<input aria-label="Project directory" className="block w-full bg-background border rounded p-2" value={projectInput} onChange={(e) => setProjectInput(e.target.value)} /></label>
      <Button variant="outline" onClick={() => setChosenProject(projectInput)}>Select project</Button>
      <p className="text-sm text-muted-foreground">The directory must map through this source’s recorded mounts. Missing or ambiguous mappings are read-only.</p>
    </div>}
    {error && <p role="alert">{error}</p>}
    {sourceId && selected ? <SourceEditor key={JSON.stringify(selection)} selection={selection} refreshKey={refreshKey} />
      : <><p>No registered sources with provider settings are available.</p><LocalPresetLibrary /></>}
    <details className="border rounded p-3"><summary>Source and directory inventory</summary><SourcesDirectoriesSection refreshKey={refreshKey} /></details>
  </div>;
};

function SourceEditor({ selection, refreshKey }: { selection: ProviderSettingsSelection; refreshKey: number }) {
  const serverReadOnly = useAppStore((s) => s.isServerReadOnly);
  const [state, setState] = React.useState<ProviderSettingsState | null>(null);
  const [text, setText] = React.useState("");
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [pendingSettings, setPendingSettings] = React.useState<ClaudeCodeSettings | null>(null);
  const [generation, setGeneration] = React.useState(0);
  const mounted = React.useRef(true);
  const sequence = React.useRef(0);
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const accept = (next: ProviderSettingsState) => {
    setState(next); setText(next.content ?? ""); setPendingSettings(null); setGeneration((n) => n + 1);
  };
  const loadSettings = React.useCallback(async () => {
    const attempt = ++sequence.current;
    setBusy(true); setState(null); setError(null);
    try {
      const next = await api<ProviderSettingsState>("read_provider_settings", { selection });
      if (mounted.current && attempt === sequence.current) accept(next);
    } catch (reason) { if (mounted.current && attempt === sequence.current) setError(String(reason)); }
    finally { if (mounted.current && attempt === sequence.current) setBusy(false); }
  }, [selection.sourceId, selection.provider, selection.scope, selection.projectPath]); // eslint-disable-line react-hooks/exhaustive-deps
  React.useEffect(() => { void loadSettings(); }, [loadSettings, refreshKey]);
  const isReadOnly = serverReadOnly || busy || !state?.write_enabled || !state.online || !state.live;
  const apply = async (content: string) => {
    if (isReadOnly || !state?.revision) throw new Error("Refresh an online, write-enabled source before applying");
    setBusy(true); setError(null);
    try {
      const next = await api<ProviderSettingsState>("apply_provider_settings", { request: { selection, revision: state.revision, content } });
      if (mounted.current) accept(next);
    } catch (reason) {
      if (mounted.current) { setError(String(reason)); setState((s) => s ? { ...s, write_enabled: false } : s); }
      throw reason;
    } finally { if (mounted.current) setBusy(false); }
  };
  const activeScope: SettingsScope = selection.scope.startsWith("managed") ? "managed"
    : selection.scope === "project" ? "project" : selection.scope === "local" ? "local" : "user";
  const visual = selection.provider === "claude" && ["user", "project", "local", "managed_macos", "managed_linux"].includes(selection.scope);
  const currentSettings: ClaudeCodeSettings = visual && state?.content ? JSON.parse(state.content) : {};
  const allSettings: AllSettingsResponse = { user: null, project: null, local: null, managed: null, [activeScope]: visual ? state?.content ?? null : null };
  const saveSettings = async (value: ClaudeCodeSettings, targetScope?: SettingsScope, targetProject?: string) => {
    if (!visual || (targetScope && targetScope !== activeScope) || (targetProject && targetProject !== selection.projectPath)) {
      throw new Error("Select the target source and scope, then refresh and review before applying a preset");
    }
    await apply(JSON.stringify(value));
  };
  const context: SettingsManagerContextValue = {
    allSettings, activeScope, setActiveScope: () => {}, currentSettings, isReadOnly: isReadOnly || !visual,
    projectPath: selection.projectPath ?? undefined, setProjectPath: () => {}, activePanel: "editor", setActivePanel: () => {},
    pendingSettings, setPendingSettings, hasUnsavedChanges: !!pendingSettings && JSON.stringify(pendingSettings) !== JSON.stringify(currentSettings),
    mcpServers: { userClaudeJson: {}, localClaudeJson: {}, userSettings: {}, userMcpFile: {}, projectMcpFile: {} },
    saveMCPServers: async () => { throw new Error("Select the MCP scope above to review and apply MCP configuration"); },
    loadSettings, saveSettings,
  };
  return <SettingsManagerContext.Provider value={context}>
    <section className="space-y-3" aria-label="Provider configuration">
      <div className="flex gap-3 items-center"><Button onClick={() => void loadSettings()} disabled={busy}>Refresh settings</Button>
        {busy && <span role="status">Reading authoritative settings…</span>}</div>
      {state && <div role="status" className="border rounded p-3 text-sm space-y-1">
        <p>{state.configuration_required ? "Source configuration incomplete" : state.online ? "Host online" : "Host offline"} · {state.live ? "Live" : state.snapshot_at ? "Snapshot" : "No snapshot"} · {isReadOnly ? "Write disabled" : "Write enabled"}</p>
        {state.snapshot_at && <p>Snapshot: {new Date(state.snapshot_at).toLocaleString()}</p>}
        {state.origin_path && <p className="break-all">{state.origin_path}</p>}
        {state.reason && <p>{state.reason}</p>}
      </div>}
      {error && <p role="alert" className="text-destructive">{error}</p>}
      <p className="text-sm text-muted-foreground">Secrets are hidden. Redacted values and omitted fields preserve existing values. Apply merges changes; it does not delete omitted fields. Offline edits are never queued.</p>
      {state?.content != null && (visual
        ? <SettingsEditorPane key={generation} sourceScoped />
        : <><label className="block">Configuration ({state.format})<textarea aria-label="Configuration" className="block w-full min-h-80 p-3 bg-background border rounded font-mono text-sm" value={text} readOnly={isReadOnly} onChange={(e) => setText(e.target.value)} /></label>
          {!isReadOnly && <Button onClick={() => void apply(text).catch(() => {})} disabled={text === state.content}>Apply</Button>}</>)}
    </section>
    {<details className="border rounded p-3 mt-4"><summary>CCHV presets · stored locally</summary>
      <p className="text-sm text-muted-foreground my-2">Preset storage is independent of source availability. Applying a preset requires a live, writable target scope.</p><PresetPanel /></details>}
  </SettingsManagerContext.Provider>;
}

function LocalPresetLibrary() {
  const [pendingSettings, setPendingSettings] = React.useState<ClaudeCodeSettings | null>(null);
  const context: SettingsManagerContextValue = {
    allSettings: null, activeScope: "user", setActiveScope: () => {}, currentSettings: {}, isReadOnly: true,
    setProjectPath: () => {}, activePanel: "editor", setActivePanel: () => {}, pendingSettings, setPendingSettings,
    hasUnsavedChanges: false, mcpServers: { userClaudeJson: {}, localClaudeJson: {}, userSettings: {}, userMcpFile: {}, projectMcpFile: {} },
    saveMCPServers: async () => { throw new Error("Select a live provider scope before applying"); },
    saveSettings: async () => { throw new Error("Select a live provider scope before applying"); }, loadSettings: async () => {},
  };
  return <SettingsManagerContext.Provider value={context}><details className="border rounded p-3"><summary>CCHV presets · stored locally</summary><PresetPanel /></details></SettingsManagerContext.Provider>;
}
