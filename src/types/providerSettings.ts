export interface ProviderSettingsSpec {
  provider: string;
  settings: { scopes: { id: string; label: string; path: string; base: string; format: string; editable: boolean }[] };
}
export interface ProviderSettingsSelection {
  sourceId: string;
  provider: string;
  scope: string;
  projectPath: string | null;
}
export interface ProviderSettingsState {
  online: boolean;
  configuration_required?: boolean;
  live: boolean;
  write_enabled: boolean;
  snapshot_at: string | null;
  revision: string | null;
  content: string | null;
  format: string;
  origin_path: string | null;
  reason: string | null;
}
