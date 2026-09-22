import { useCallback, useEffect, useRef, useState } from "react";
import { AUTO_PING_SETTINGS_KEYS } from "@/shared/constants/config";
import { updateSettings, type AutoPingConfig } from "@/shared/utils/backendApi";

export type AutoPingProvider = keyof typeof AUTO_PING_SETTINGS_KEYS;

type AutoPingConfigs = Partial<Record<AutoPingProvider, AutoPingConfig>>;

const EMPTY_CONFIG: AutoPingConfig = { enabled: false, connections: {} };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function configFromConnections(connections: Record<string, boolean>): AutoPingConfig {
  return {
    enabled: Object.values(connections).some(Boolean),
    connections,
  };
}

function normalizeConfig(value: unknown): {
  config: AutoPingConfig;
  needsMigration: boolean;
} {
  if (!isRecord(value)) {
    return { config: EMPTY_CONFIG, needsMigration: false };
  }

  if (isRecord(value.connections)) {
    const connections = Object.fromEntries(
      Object.entries(value.connections).filter(
        (entry): entry is [string, boolean] => typeof entry[1] === "boolean",
      ),
    );
    const config = configFromConnections(connections);
    const needsMigration =
      value.enabled !== config.enabled ||
      Object.keys(connections).length !== Object.keys(value.connections).length;
    return { config, needsMigration };
  }

  // Older quota UI builds wrote the connection map directly under the setting key.
  const connections = Object.fromEntries(
    Object.entries(value).filter(
      (entry): entry is [string, boolean] =>
        entry[0] !== "enabled" && typeof entry[1] === "boolean",
    ),
  );
  if (Object.keys(connections).length === 0) {
    return { config: EMPTY_CONFIG, needsMigration: false };
  }

  return { config: configFromConnections(connections), needsMigration: true };
}

/**
 * Shared auto-ping settings state for provider and quota dashboard pages.
 * The connections map is authoritative; `enabled` is kept as its derived UI field.
 */
export function useAutoPingSettings() {
  const [configs, setConfigs] = useState<AutoPingConfigs>({});
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState<Partial<Record<AutoPingProvider, boolean>>>({});
  const configsRef = useRef<AutoPingConfigs>({});
  const savingProvidersRef = useRef(new Set<AutoPingProvider>());

  const load = useCallback(async () => {
    setReady(false);
    setError(null);

    try {
      const response = await fetch("/api/settings", { cache: "no-store" });
      if (!response.ok) throw new Error("Failed to load auto-ping settings");
      const settings = await response.json();
      const nextConfigs: AutoPingConfigs = {};
      const migrations: Array<[AutoPingProvider, AutoPingConfig]> = [];

      for (const provider of Object.keys(AUTO_PING_SETTINGS_KEYS) as AutoPingProvider[]) {
        const key = AUTO_PING_SETTINGS_KEYS[provider];
        const normalized = normalizeConfig(settings[key]);
        nextConfigs[provider] = normalized.config;
        if (normalized.needsMigration) {
          migrations.push([provider, normalized.config]);
        }
      }

      // Repair flat values previously written by the quota page before showing them as active.
      for (const [provider, config] of migrations) {
        await updateSettings({ [AUTO_PING_SETTINGS_KEYS[provider]]: config });
      }

      configsRef.current = nextConfigs;
      setConfigs(nextConfigs);
      setReady(true);
    } catch (loadError) {
      setError(loadError instanceof Error ? loadError.message : "Failed to load auto-ping settings");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggleConnection = useCallback(
    async (provider: AutoPingProvider, connectionId: string, enabled: boolean) => {
      if (!ready) throw new Error("Auto-ping settings are not loaded");
      if (savingProvidersRef.current.has(provider)) return;

      savingProvidersRef.current.add(provider);
      setSaving((previous) => ({ ...previous, [provider]: true }));

      const previousConfig = configsRef.current[provider] || EMPTY_CONFIG;
      const nextConfig = configFromConnections({
        ...previousConfig.connections,
        [connectionId]: enabled,
      });
      const optimisticConfigs = { ...configsRef.current, [provider]: nextConfig };
      configsRef.current = optimisticConfigs;
      setConfigs(optimisticConfigs);

      try {
        await updateSettings({ [AUTO_PING_SETTINGS_KEYS[provider]]: nextConfig });
      } catch (saveError) {
        const rolledBackConfigs = { ...configsRef.current, [provider]: previousConfig };
        configsRef.current = rolledBackConfigs;
        setConfigs(rolledBackConfigs);
        throw saveError;
      } finally {
        savingProvidersRef.current.delete(provider);
        setSaving((previous) => ({ ...previous, [provider]: false }));
      }
    },
    [ready],
  );

  return { configs, ready, error, saving, toggleConnection };
}
