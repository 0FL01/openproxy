import { create } from "zustand";
import { useEffect } from "react";

// Catalog returned by GET /api/catalog (mirrors src/core/model/provider_catalog.json).
export interface CatalogModel {
  id: string;
  name?: string;
  kind: string;
  contextWindow?: number | null;
  capabilities?: string[];
  reasoningEfforts?: string[] | null;
}

export interface XaiCatalogDiagnostic {
  status: "ready" | "stale" | "unavailable";
  warning?: string;
}

interface CatalogModelsEntry {
  alias: string;
  models: CatalogModel[];
}

interface CatalogResponse {
  providerIdToAlias?: Record<string, string>;
  providerModels?: CatalogModelsEntry[];
  xaiCatalog?: XaiCatalogDiagnostic;
}

interface CatalogState {
  loaded: boolean;
  loading: boolean;
  error: string | null;
  xaiCatalog?: XaiCatalogDiagnostic;
  modelsByAlias: Record<string, CatalogModel[]>;
  providerIdToAlias: Record<string, string>;
  load: () => Promise<void>;
  reload: () => Promise<void>;
}

export const useCatalogStore = create<CatalogState>((set, get) => {
  let pending: Promise<void> | null = null;
  let reloadQueued = false;

  const reload = (): Promise<void> => {
    if (pending) {
      // Explicit invalidation must not be lost to an older in-flight snapshot.
      // All callers await the entire coalesced refresh, including this follow-up.
      reloadQueued = true;
      return pending;
    }
    pending = Promise.resolve().then(async () => {
      try {
        for (;;) {
          try {
            const res = await fetch("/api/catalog", { cache: "no-store" });
            if (!res.ok) throw new Error(`Failed to load model catalog (${res.status})`);
            const data: CatalogResponse = await res.json();
            const modelsByAlias: Record<string, CatalogModel[]> = {};
            for (const entry of data.providerModels || []) {
              if (entry?.alias) modelsByAlias[entry.alias] = entry.models || [];
            }
            set({
              loaded: true,
              error: null,
              modelsByAlias,
              providerIdToAlias: data.providerIdToAlias || {},
              xaiCatalog: data.xaiCatalog,
            });
          } catch (error) {
            // Preserve last-good data and expose errors without rejecting the
            // fire-and-forget initial load (or existing mutation callers).
            set({ error: error instanceof Error ? error.message : "Failed to load model catalog" });
          }
          if (!reloadQueued) break;
          reloadQueued = false;
        }
      } finally {
        pending = null;
        set({ loading: false });
      }
    });
    set({ loading: true, error: null });
    return pending;
  };

  return {
    loaded: false,
    loading: false,
    error: null,
    modelsByAlias: {},
    providerIdToAlias: {},
    load: () => {
      if (pending) return pending;
      return get().loaded ? Promise.resolve() : reload();
    },
    reload,
  };
});

// Kick off the fetch on module import in the browser.
if (typeof window !== "undefined") {
  void useCatalogStore.getState().load();
}

/**
 * React hook that ensures the catalog is loaded and re-renders the caller
 * when it arrives. Returns true once data is available.
 */
export function useEnsureCatalog(): boolean {
  const loaded = useCatalogStore((s) => s.loaded);
  useEffect(() => {
    void useCatalogStore.getState().load();
  }, []);
  return loaded;
}
