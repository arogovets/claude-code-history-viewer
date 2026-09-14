/** Read-only collector metadata. Origin paths are display data, never local write targets. */
export interface SourceMount {
  path: string;
  mirror_path: string;
  kind: "file" | "directory";
  providers: string[];
  role: string | null;
  available: boolean | null;
}

export interface FilesystemSource {
  id: string;
  label: string;
  allow_settings_write?: boolean;
  current: string;
  origin: {
    ssh: string | null;
    home: string | null;
    paths: SourceMount[];
    project_roots: SourceMount[];
  };
  mounts: SourceMount[];
  last_collected_at: string | null;
}
