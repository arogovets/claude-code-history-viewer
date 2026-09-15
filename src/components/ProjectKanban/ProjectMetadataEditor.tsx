import { projectMetadataKey } from "@/types/kanban";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useAppStore } from "@/store/useAppStore";
import { Button } from "@/components/ui/button";
import type { ClaudeProject } from "@/types";

export function ProjectMetadataEditor({ project }: { project: ClaudeProject }) {
  const { t } = useTranslation();
  const metadata = useAppStore(
    (s) =>
      s.userMetadata.projects[projectMetadataKey(project)] ??
      s.userMetadata.projects[project.actual_path],
  );
  const readOnly = useAppStore((s) => s.isServerReadOnly);
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [links, setLinks] = useState("");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const safeLinks = (metadata?.links ?? []).filter((link) =>
    /^https?:\/\//i.test(link),
  );
  return (
    <div className="space-y-3">
      {!editing ? (
        <>
          {metadata?.description && (
            <p className="whitespace-pre-wrap text-sm">
              {metadata.description}
            </p>
          )}
          {safeLinks.map((link) => (
            <a
              className="block break-all text-sm text-accent underline"
              key={link}
              href={link}
              target="_blank"
              rel="noopener noreferrer"
            >
              {link}
            </a>
          ))}
          <Button
            variant="outline"
            disabled={readOnly}
            onClick={() => {
              setName(metadata?.alias ?? project.name);
              setDescription(metadata?.description ?? "");
              setLinks((metadata?.links ?? []).join("\n"));
              setError("");
              setEditing(true);
            }}
          >
            {t("kanban.editProject")}
          </Button>
        </>
      ) : (
        <form
          className="space-y-3"
          onSubmit={async (event) => {
            event.preventDefault();
            const values = links
              .split("\n")
              .map((link) => link.trim())
              .filter(Boolean);
            if (
              values.some((link) => {
                try {
                  return !["http:", "https:"].includes(new URL(link).protocol);
                } catch {
                  return true;
                }
              })
            ) {
              setError(t("kanban.invalidLinks"));
              return;
            }
            setSaving(true);
            setError("");
            try {
              useAppStore.getState().clearMetadataError();
              await useAppStore.getState().updateProjectMetadata(projectMetadataKey(project), {
                alias: name.trim(),
                description: description.trim(),
                links: [...new Set(values)],
              });
              const failure = useAppStore.getState().metadataError;
              if (failure) setError(failure);
              else setEditing(false);
            } catch (e) {
              setError(String(e));
            } finally {
              setSaving(false);
            }
          }}
        >
          <label className="block text-sm">
            {t("kanban.name")}
            <input
              className="mt-1 w-full rounded border border-border bg-background p-2"
              required
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
          <label className="block text-sm">
            {t("kanban.projectDescription")}
            <textarea
              className="mt-1 w-full rounded border border-border bg-background p-2"
              rows={4}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </label>
          <label className="block text-sm">
            {t("kanban.links")}
            <textarea
              className="mt-1 w-full rounded border border-border bg-background p-2"
              rows={3}
              value={links}
              onChange={(e) => setLinks(e.target.value)}
            />
          </label>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <div className="flex gap-2">
            <Button disabled={saving || !name.trim()}>
              {t("kanban.save")}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={saving}
              onClick={() => setEditing(false)}
            >
              {t("kanban.cancel")}
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}
