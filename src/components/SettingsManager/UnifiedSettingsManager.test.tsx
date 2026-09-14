import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "@/services/api";
import { UnifiedSettingsManager } from "./UnifiedSettingsManager";
import type { ProviderSettingsState } from "@/types/providerSettings";
import { mappedProjectPath } from "@/utils/providerSettings";
import type { FilesystemSource } from "@/types/filesystemSources";

vi.mock("@/services/api", () => ({ api: vi.fn() }));
vi.mock("@/hooks/analytics/useAnalyticsNavigation", () => ({ useAnalyticsNavigation: () => ({ switchToArchive: vi.fn() }) }));
vi.mock("@/store/useAppStore", () => ({ useAppStore: (select: (s: unknown) => unknown) => select({ isServerReadOnly: false }) }));
vi.mock("./editor/SettingsEditorPane", () => ({ SettingsEditorPane: () => <div>Claude visual editor</div> }));
vi.mock("./sidebar/PresetPanel", () => ({ PresetPanel: () => <div>Local presets</div> }));
vi.mock("./sections/SourcesDirectoriesSection", () => ({ SourcesDirectoriesSection: () => <div>Source inventory</div> }));

const source = (id: string): FilesystemSource => ({ id, label: id, current: `/mirrors/${id}/current`,
  origin: { ssh: id === "a" ? null : "remote", home: `/home/${id}`, paths: [], project_roots: [] },
  allow_settings_write: true, mounts: [], last_collected_at: null });
const specs = ["claude", "codex", "opencode", "antigravity"].map((provider) => ({ provider, settings: { scopes: [
  { id: "user", label: "User settings", path: "config", base: "home", format: "json", editable: true },
  { id: "managed_linux", label: "Managed settings", path: "config", base: "absolute", format: "json", editable: false },
] } }));
const live = (override: Partial<ProviderSettingsState> = {}): ProviderSettingsState => ({ online: true, live: true,
  write_enabled: true, snapshot_at: "2026-09-14T00:00:00Z", revision: "base-revision", content: '{"model":"before"}',
  format: "json", origin_path: "/home/a/config", reason: null, ...override });
let state: ProviderSettingsState;
beforeEach(() => {
  vi.clearAllMocks(); state = live();
  vi.mocked(api).mockImplementation(async (command) => {
    if (command === "list_filesystem_sources") return [source("a"), source("b")];
    if (command === "list_provider_settings") return specs;
    if (command === "read_provider_settings") return state;
    if (command === "apply_provider_settings") return live({ revision: "new-revision", content: '{"model":"reread-origin"}' });
    throw new Error(`Unexpected legacy API: ${command}`);
  });
});
async function openGeneric() {
  render(<UnifiedSettingsManager />);
  await screen.findByText("Claude visual editor");
  fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "opencode" } });
  return screen.findByLabelText("Configuration");
}
describe("Source-aware Settings Manager", () => {
  it("offers Antigravity and reads its selected source settings", async () => {
    render(<UnifiedSettingsManager />);
    await screen.findByRole("option", { name: "Antigravity" });
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "antigravity" } });
    expect(await screen.findByLabelText("Configuration")).toHaveValue('{"model":"before"}');
    expect(api).toHaveBeenCalledWith("read_provider_settings", { selection: {
      sourceId: "a", provider: "antigravity", scope: "user", projectPath: null,
    } });
  });
  it("applies to the selected source with its base revision and displays the origin reread", async () => {
    const editor = await openGeneric();
    fireEvent.change(editor, { target: { value: '{"model":"after"}' } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(screen.getByLabelText("Configuration")).toHaveValue('{"model":"reread-origin"}'));
    expect(api).toHaveBeenCalledWith("apply_provider_settings", { request: {
      selection: { sourceId: "a", provider: "opencode", scope: "user", projectPath: null }, revision: "base-revision", content: '{"model":"after"}',
    } });
  });
  it.each([
    { online: false, live: false, write_enabled: false },
    { online: true, live: true, write_enabled: false },
    { online: true, live: false, write_enabled: false },
  ])("never offers Apply for a read-only source or snapshot %j", async (flags) => {
    state = live(flags);
    const editor = await openGeneric();
    expect(editor).toHaveAttribute("readonly");
    expect(screen.queryByRole("button", { name: "Apply" })).not.toBeInTheDocument();
    expect(screen.getByText(/Write disabled/)).toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([c]) => c === "apply_provider_settings")).toBe(false);
  });
  it("requires refresh after conflicts without retrying or queuing the edit", async () => {
    const editor = await openGeneric();
    vi.mocked(api).mockRejectedValueOnce(new Error("Revision conflict; refresh and review"));
    fireEvent.change(editor, { target: { value: '{"model":"after"}' } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Revision conflict");
    expect(screen.queryByRole("button", { name: "Apply" })).not.toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.filter(([c]) => c === "apply_provider_settings")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Refresh settings" }));
    await waitFor(() => expect(screen.getByLabelText("Configuration")).toHaveValue('{"model":"before"}'));
    expect(vi.mocked(api).mock.calls.filter(([c]) => c === "apply_provider_settings")).toHaveLength(1);
  });
  it("discards the previous source draft and reads the newly selected machine", async () => {
    const editor = await openGeneric();
    fireEvent.change(editor, { target: { value: '{"model":"a draft"}' } });
    state = live({ content: '{"model":"machine-b"}', write_enabled: false });
    fireEvent.change(screen.getByLabelText("Source"), { target: { value: "b" } });
    await waitFor(() => expect(screen.getByLabelText("Configuration")).toHaveValue('{"model":"machine-b"}'));
    expect(api).toHaveBeenCalledWith("read_provider_settings", { selection: { sourceId: "b", provider: "opencode", scope: "user", projectPath: null } });
    expect(screen.queryByRole("button", { name: "Apply" })).not.toBeInTheDocument();
  });
  it("shows incomplete source configuration without claiming the host is offline", async () => {
    state = live({ configuration_required: true, online: false, live: false, write_enabled: false, content: null,
      reason: "Add an authoritative home to the source configuration" });
    render(<UnifiedSettingsManager />);
    expect(await screen.findByText(/Source configuration incomplete/)).toBeInTheDocument();
    expect(screen.getByText(/Add an authoritative home/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Apply" })).not.toBeInTheDocument();
    expect(screen.queryByText(/Host offline/)).not.toBeInTheDocument();
  });
  it("maps origin project labels through recorded mounts only", () => {
    const machine = source("a");
    machine.mounts = [{ path: "/remote/work", mirror_path: "projects", kind: "directory", providers: [], role: "project_root", available: true }];
    expect(mappedProjectPath(machine, "/remote/work/demo")).toBe("/mirrors/a/current/projects/demo");
    expect(mappedProjectPath(machine, "/mirrors/a/current/projects/demo")).toBe("/mirrors/a/current/projects/demo");
    expect(mappedProjectPath(machine, "/remote/work/../escape")).toBeNull();
    expect(mappedProjectPath(machine, "/remote/elsewhere")).toBeNull();
    machine.mounts.push({ ...machine.mounts[0]!, mirror_path: "other" });
    expect(mappedProjectPath(machine, "/remote/work/demo")).toBeNull();
  });
});
