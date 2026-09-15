import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import { api } from "@/services/api";
import { SourcesDirectoriesSection } from "./SourcesDirectoriesSection";
import type { FilesystemSource } from "@/types/filesystemSources";

vi.mock("@/services/api", () => ({ api: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

function source(id: string, ssh: string | null): FilesystemSource {
  return {
    id, label: id, current: `/mirrors/${id}/current`,
    origin: { ssh, home: `/home/${id}`, paths: [], project_roots: [] },
    last_collected_at: "2026-09-14T01:02:03Z",
    mounts: [
      { path: `/home/${id}/.claude`, mirror_path: ".claude", kind: "directory", providers: ["claude"], role: "provider", available: true },
      { path: `/home/${id}/extra.jsonl`, mirror_path: "extra.jsonl", kind: "file", providers: ["codex"], role: "extra", available: true },
    ],
  };
}

describe("Sources / Directories", () => {
  beforeEach(() => { vi.clearAllMocks(); });
  it.each([0, 1, 3])("renders %i sources from collector metadata only", async (count) => {
    vi.mocked(api).mockResolvedValue(Array.from({ length: count }, (_, i) => source(`machine-${i}`, i ? "remote-host" : null)));
    const { container } = render(<SourcesDirectoriesSection />);
    await waitFor(() => expect(screen.queryByRole("status")).not.toBeInTheDocument());
    expect(screen.queryAllByRole("article")).toHaveLength(count);
    if (!count) expect(screen.getByText("settingsManager.sources.empty")).toBeInTheDocument();
    for (const [i, article] of screen.queryAllByRole("article").entries()) {
      expect(within(article).getAllByRole("listitem")).toHaveLength(2);
      expect(article).toHaveTextContent(i ? "settingsManager.sources.ssh" : "settingsManager.sources.local");
      expect(article).toHaveTextContent(`/home/machine-${i}/.claude`);
      expect(article).toHaveTextContent(`/mirrors/machine-${i}/current/.claude`);
      expect(article).toHaveTextContent("claude");
      expect(article).toHaveTextContent("codex");
      expect(article).toHaveTextContent("settingsManager.sources.file");
      expect(article).toHaveTextContent("settingsManager.sources.directory");
    }
    expect(container.querySelectorAll("input, select, textarea, button")).toHaveLength(0);
    expect(api).toHaveBeenCalledExactlyOnceWith("list_filesystem_sources");
  });
  it("shows inventory errors", async () => {
    vi.mocked(api).mockRejectedValue(new Error("Invalid source metadata"));
    render(<SourcesDirectoriesSection />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Invalid source metadata");
  });
  it("shows project roots and pending collection", async () => {
    const pending = source("pending", "offline");
    pending.last_collected_at = null;
    pending.mounts[0] = { ...pending.mounts[0]!, role: "project_root", providers: ["aider", "crush"], available: false };
    vi.mocked(api).mockResolvedValue([pending]);
    render(<SourcesDirectoriesSection />);
    expect(await screen.findByText("settingsManager.sources.pending")).toBeInTheDocument();
    expect(screen.getByRole("article")).toHaveTextContent("settingsManager.sources.projectRoot");
    expect(screen.getByRole("article")).toHaveTextContent("aider, crush");
  });
});
