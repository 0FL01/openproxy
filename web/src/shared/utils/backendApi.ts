/**
 * Helper functions for calling the OpenProxy Rust backend API from the
 * Astro/React dashboard. Prefer relative `/api/*` URLs in the browser so
 * session cookies and the active host/port always match.
 */

function apiBase(): string {
  if (typeof window !== "undefined") {
    // Same-origin relative paths — preserves dashboard session cookie.
    return "";
  }
  return (
    process.env.NEXT_PUBLIC_BASE_URL ??
    process.env.BASE_URL ??
    "http://127.0.0.1:4623"
  );
}

function apiUrl(path: string): string {
  const base = apiBase();
  if (!base) return path;
  return `${base.replace(/\/$/, "")}${path}`;
}

async function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  return fetch(apiUrl(path), {
    ...init,
    headers,
    credentials: "same-origin",
  });
}

export interface Settings {
  cloudEnabled?: boolean;
  claudeAutoPing?: AutoPingConfig;
  codexAutoPing?: AutoPingConfig;
  glmAutoPing?: AutoPingConfig;
  [key: string]: unknown;
}

export interface AutoPingConfig {
  enabled: boolean;
  connections: Record<string, boolean>;
}

interface ApiKey {
  id: string;
  name: string;
  key: string;
  createdAt: string;
  [key: string]: unknown;
}

interface ConsoleLog {
  timestamp: string;
  level: string;
  message: string;
  [key: string]: unknown;
}

/**
 * Get settings from the Rust backend
 */
export async function getSettings(): Promise<Settings> {
  const response = await apiFetch("/api/settings");
  if (!response.ok) {
    throw new Error(`Failed to get settings: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Update settings in the Rust backend (partial PATCH).
 */
export async function updateSettings(settings: Partial<Settings>): Promise<Settings> {
  const response = await apiFetch("/api/settings", {
    method: "PATCH",
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    throw new Error(`Failed to update settings: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get API keys from the Rust backend
 */
export async function getApiKeys(): Promise<ApiKey[]> {
  const response = await apiFetch("/api/keys");
  if (!response.ok) {
    throw new Error(`Failed to get API keys: ${response.statusText}`);
  }
  const data = await response.json();
  return data.keys || [];
}

/**
 * Get console logs from Rust backend
 */
export async function getConsoleLogs(): Promise<ConsoleLog[]> {
  const response = await apiFetch("/api/observability/logs");
  if (!response.ok) {
    throw new Error(`Failed to get console logs: ${response.statusText}`);
  }
  const data = await response.json();
  return data.logs || [];
}

/**
 * Check if cloud sync is enabled
 */
export async function isCloudEnabled(): Promise<boolean> {
  const settings = await getSettings();
  return settings.cloudEnabled === true;
}
