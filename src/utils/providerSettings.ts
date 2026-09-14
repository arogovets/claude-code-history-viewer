import type { FilesystemSource } from "@/types/filesystemSources";

export function mappedProjectPath(source: FilesystemSource, path: string): string | null {
  if (!path.startsWith("/") || path.split("/").some((part) => part === "." || part === "..") || path.includes("\\")) return null;
  const candidates = new Set<string>();
  for (const mount of [...source.mounts, ...source.origin.paths, ...source.origin.project_roots]) {
    if (mount.kind !== "directory") continue;
    const mirror = `${source.current}/${mount.mirror_path}`;
    if (path === mirror || path.startsWith(`${mirror}/`)) candidates.add(path);
    if (path === mount.path || path.startsWith(`${mount.path}/`)) candidates.add(`${mirror}${path.slice(mount.path.length)}`);
  }
  return candidates.size === 1 ? [...candidates][0]! : null;
}
