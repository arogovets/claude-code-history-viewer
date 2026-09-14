import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "@/services/api";
import type { FilesystemSource } from "@/types/filesystemSources";

export function SourcesDirectoriesSection({ refreshKey = 0 }: { refreshKey?: number }) {
  const { t } = useTranslation();
  const [sources, setSources] = useState<FilesystemSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    api<FilesystemSource[]>("list_filesystem_sources")
      .then((result) => { if (active) setSources(result); })
      .catch((reason: unknown) => { if (active) setError(String(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [refreshKey]);

  return (
    <section className="p-4 space-y-3" aria-label={t("settingsManager.sources.title")}>
      <h3 className="font-medium">{t("settingsManager.sources.title")}</h3>
      <p className="text-sm text-muted-foreground">{t("settingsManager.sources.description")}</p>
      {loading ? <p role="status">{t("settingsManager.loading")}</p>
        : error ? <p role="alert">{error}</p>
        : sources.length === 0 ? <p>{t("settingsManager.sources.empty")}</p>
        : sources.map((source) => (
          <article key={source.id} className="border border-border rounded-md p-3 space-y-2">
            <h4 className="font-medium">{source.label} · {source.origin.ssh
              ? `${t("settingsManager.sources.ssh")} (${source.origin.ssh})`
              : t("settingsManager.sources.local")}</h4>
            <p className="text-xs text-muted-foreground break-all">{source.id} · {source.current}</p>
            {source.origin.home && <p className="text-xs break-all">{t("settingsManager.sources.home")}: {source.origin.home}</p>}
            <p className="text-xs text-muted-foreground">{source.last_collected_at
              ? `${t("settingsManager.sources.collected")}: ${source.last_collected_at}`
              : t("settingsManager.sources.pending")}</p>
            <ul className="space-y-2">
              {source.mounts.map((mount) => (
                <li key={mount.mirror_path} className="text-sm border-t border-border pt-2">
                  <div className="font-medium">
                    {mount.role === "project_root" && `${t("settingsManager.sources.projectRoot")} · `}
                    {mount.providers.join(", ") || t("settingsManager.sources.extra")}
                    <span className="text-xs font-normal text-muted-foreground"> · {mount.kind === "file"
                      ? t("settingsManager.sources.file") : t("settingsManager.sources.directory")}</span>
                  </div>
                  <div className="break-all">{mount.path} <span aria-label={t("settingsManager.sources.mirroredTo")}>→</span> {source.current}/{mount.mirror_path}</div>
                  {mount.available === false && <p className="text-xs text-muted-foreground">{t("settingsManager.sources.absent")}</p>}
                </li>
              ))}
            </ul>
          </article>
        ))}
    </section>
  );
}
