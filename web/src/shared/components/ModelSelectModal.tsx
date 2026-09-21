
import { useState, useMemo, useEffect, useRef } from "react";
import Modal from "./Modal";
import Button from "./Button";
import CapacityBadges from "./CapacityBadges";
import { useModelCaps } from "@/shared/hooks/useModelCaps";
import { getModelsByProviderId, useEnsureCatalog } from "@/shared/constants/models";
import { OAUTH_PROVIDERS, APIKEY_PROVIDERS, FREE_PROVIDERS, FREE_TIER_PROVIDERS, AI_PROVIDERS, isOpenAICompatibleProvider, isAnthropicCompatibleProvider, getProviderAlias } from "@/shared/constants/providers";
import { buildAvailableModels, fetchLiveModels, useFavorites, type LiveModel } from "@/shared/models/availableModels";
import React from "react";
import { useCatalogStore } from "@/store/catalogStore";

interface Model {
  id: string;
  name: string;
  value: string;
  type?: string;
  isPlaceholder?: boolean;
  isCustom?: boolean;
}

interface ModelGroup {
  name: string;
  alias: string;
  color: string;
  models: Model[];
  isCustom?: boolean;
  hasModels?: boolean;
}

interface ActiveProvider {
  provider: string;
  [key: string]: any;
}

interface ProviderNode {
  id: string;
  name?: string;
  prefix?: string;
}

interface CustomModel {
  id: string;
  name?: string;
  providerAlias: string;
}

// Provider order: OAuth first, then Free Tier, then API Key (matches dashboard/providers)
const PROVIDER_ORDER = [
  ...Object.keys(OAUTH_PROVIDERS),
  ...Object.keys(FREE_PROVIDERS),
  ...Object.keys(FREE_TIER_PROVIDERS),
  ...Object.keys(APIKEY_PROVIDERS),
];

// Providers that need no auth — always show in model selector
const NO_AUTH_PROVIDER_IDS = Object.keys(FREE_PROVIDERS).filter(id => FREE_PROVIDERS[id].noAuth);

interface ModelSelectModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (model: Model) => void;
  selectedModel?: string | string[];
  activeProviders?: ActiveProvider[];
  title?: string;
  modelAliases?: Record<string, string>;
  // When false, picking a model does not close the modal; the user must press
  // Done. Useful when the parent uses onSelect to toggle multiple entries.
  closeOnSelect?: boolean;
  // Multi-select mode: rows render a checkbox, clicking a row toggles
  // selection (instead of opening), and the footer shows "Apply N".
  selectionMode?: "single" | "multi";
  // Called with the selected model ids when the user presses "Apply N".
  onSelectIds?: (ids: string[]) => void;
}

export default function ModelSelectModal({
  isOpen,
  onClose,
  onSelect,
  selectedModel,
  activeProviders = [],
  title = "Select Model",
  modelAliases = {},
  closeOnSelect = true,
  selectionMode = "single",
  onSelectIds,
}: ModelSelectModalProps) {
  useEnsureCatalog();
  const reloadCatalog = useCatalogStore((state) => state.reload);
  const modelsByAlias = useCatalogStore((state) => state.modelsByAlias);
  const { getCaps } = useModelCaps();
  const [searchQuery, setSearchQuery] = useState("");
  const [providerNodes, setProviderNodes] = useState<ProviderNode[]>([]);
  const [customModels, setCustomModels] = useState<CustomModel[]>([]);
  const [disabledMap, setDisabledMap] = useState<Record<string, string[]>>({});
  const [liveModelsByAlias, setLiveModelsByAlias] = useState<Record<string, LiveModel[]>>({});
  const [freeOnlyByAlias, setFreeOnlyByAlias] = useState<Record<string, boolean>>({});
  const listRef = useRef<HTMLDivElement>(null);

  // Shared favorites (star) store — same cache the provider page uses.
  const { isFavorite, toggleFavorite } = useFavorites();

  // Multi-select state (modal-scoped; the modal is the single owner since it
  // spans many providers at once).
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const toggleSelect = (id: string) =>
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]
    );
  useEffect(() => {
    if (isOpen) {
      setSelectedIds([]);
      void reloadCatalog();
    }
  }, [isOpen, reloadCatalog]);

  const fetchProviderNodes = async () => {
    try {
      const res = await fetch("/api/provider-nodes");
      if (!res.ok) throw new Error(`Failed to fetch provider nodes: ${res.status}`);
      const data = await res.json();
      setProviderNodes(data.nodes || []);
    } catch (error) {
      console.error("Error fetching provider nodes:", error);
      setProviderNodes([]);
    }
  };

  useEffect(() => {
    if (isOpen) fetchProviderNodes();
  }, [isOpen]);

  const fetchCustomModels = async () => {
    try {
      const res = await fetch("/api/models/custom");
      if (!res.ok) throw new Error(`Failed to fetch custom models: ${res.status}`);
      const data = await res.json();
      setCustomModels(data.models || []);
    } catch (error) {
      console.error("Error fetching custom models:", error);
      setCustomModels([]);
    }
  };

  useEffect(() => {
    if (isOpen) fetchCustomModels();
  }, [isOpen]);

  const fetchDisabledMap = async () => {
    try {
      const res = await fetch("/api/models/disabled", { cache: "no-store" });
      if (!res.ok) throw new Error(`Failed to fetch disabled: ${res.status}`);
      const data = await res.json();
      if (data.disabled && typeof data.disabled === "object") setDisabledMap(data.disabled);
      else if (Array.isArray(data.ids)) setDisabledMap({});
      else setDisabledMap({});
    } catch {
      setDisabledMap({});
    }
  };

  useEffect(() => {
    if (isOpen) fetchDisabledMap();
  }, [isOpen]);

  // Fetch live model lists (kilo free-models, opencode-zen / openrouter /
  // opencode fetchers) + per-provider freeOnly filters so the modal's groups
  // match the provider page's Available Models exactly.
  useEffect(() => {
    if (!isOpen) return;

    const liveCapable = Object.entries(AI_PROVIDERS)
      .filter(([id, p]) => id === "kilocode" || !!p.modelsFetcher)
      .map(([id]) => id);

    Promise.all(
      liveCapable.map(async (id) => {
        const alias = getProviderAlias(id);
        const models = await fetchLiveModels(id, alias);
        return [alias, models] as const;
      })
    )
      .then((entries) => {
        const map: Record<string, LiveModel[]> = {};
        for (const [alias, models] of entries) map[alias] = models;
        setLiveModelsByAlias(map);
      })
      .catch(() => {});

    fetch("/api/providers/filters", { cache: "no-store" })
      .then((res) => (res.ok ? res.json() : null))
      .then((data) => {
        const filters = (data && data.filters) || {};
        const map: Record<string, boolean> = {};
        for (const [alias, entry] of Object.entries(filters)) {
          const freeOnly = (entry as { freeOnly?: boolean } | null)?.freeOnly;
          if (typeof freeOnly === "boolean") map[alias] = freeOnly;
        }
        setFreeOnlyByAlias(map);
      })
      .catch(() => {});
  }, [isOpen]);

  const allProviders = useMemo(() => ({ ...OAUTH_PROVIDERS, ...FREE_PROVIDERS, ...FREE_TIER_PROVIDERS, ...APIKEY_PROVIDERS }), []);

  // Group models by provider with priority order
  const groupedModels = useMemo(() => {
    const groups: Record<string, ModelGroup> = {};

    const isDisabled = (alias: string, modelId: string) => {
      const arr = disabledMap[alias];
      return Array.isArray(arr) && arr.includes(modelId);
    };

    const activeConnectionIds = activeProviders.map(p => p.provider);

    const providerIdsToShow = new Set([
      ...activeConnectionIds,
      ...NO_AUTH_PROVIDER_IDS,
    ]);

    const sortedProviderIds = [...providerIdsToShow].sort((a, b) => {
      const indexA = PROVIDER_ORDER.indexOf(a);
      const indexB = PROVIDER_ORDER.indexOf(b);
      return (indexA === -1 ? 999 : indexA) - (indexB === -1 ? 999 : indexB);
    });

    sortedProviderIds.forEach((providerId) => {
      const alias = getProviderAlias(providerId);
      const providerInfo = allProviders[providerId] || { name: providerId, color: "#666" };
      const isCustomProvider = isOpenAICompatibleProvider(providerId) || isAnthropicCompatibleProvider(providerId);

      if (providerInfo.passthroughModels) {
        const aliasModels = Object.entries(modelAliases)
          .filter(([, fullModel]) => fullModel.startsWith(`${alias}/`))
          .map(([aliasName, fullModel]) => ({
            id: fullModel.replace(`${alias}/`, ""),
            name: aliasName,
            value: fullModel,
          }));

        const built = buildAvailableModels({
          catalogModels: getModelsByProviderId(providerId) as any,
          liveModels: liveModelsByAlias[alias] || [],
          customModels: customModels as any,
          modelAliases,
          disabledIds: disabledMap[alias] || [],
          providerAlias: alias,
          type: "llm",
          freeOnly: freeOnlyByAlias[alias] || false,
        });
        const mapped = built.enabledRows.map((r) => ({
          id: r.id,
          name: r.name,
          value: r.fullModel,
          type: r.type,
          isFree: r.isFree,
          isCustom: r.source === "custom" || r.source === "legacyAlias",
        }));
        const combined = mapped.length > 0 ? mapped : aliasModels;

        if (combined.length > 0) {
          const matchedNode = providerNodes.find(node => node.id === providerId);
          const displayName = matchedNode?.name || providerInfo.name;
          groups[providerId] = { name: displayName, alias, color: providerInfo.color, models: combined };
        } else if ((providerInfo.serviceKinds || ["llm"]).includes("llm")) {
          const matchedNode = providerNodes.find(node => node.id === providerId);
          groups[providerId] = {
            name: matchedNode?.name || providerInfo.name,
            alias,
            color: providerInfo.color,
            models: [{ id: providerId, name: matchedNode?.name || providerInfo.name, value: alias }],
          };
        }
      } else if (isCustomProvider) {
        const connection = activeProviders.find(p => p.provider === providerId);
        const matchedNode = providerNodes.find(node => node.id === providerId);
        const displayName = connection?.name || matchedNode?.name || providerInfo.name;
        const nodePrefix = connection?.providerSpecificData?.prefix || matchedNode?.prefix || providerId;
        const nodeModels = Object.entries(modelAliases)
          .filter(([, fullModel]) => fullModel.startsWith(`${providerId}/`))
          .map(([aliasName, fullModel]) => ({
            id: fullModel.replace(`${providerId}/`, ""),
            name: aliasName,
            value: `${nodePrefix}/${fullModel.replace(`${providerId}/`, "")}`,
          }));
        const modelsToShow = nodeModels.length > 0 ? nodeModels : [{
          id: `__placeholder__${providerId}`,
          name: `${nodePrefix}/model-id`,
          value: `${nodePrefix}/model-id`,
          isPlaceholder: true,
        }];
        groups[providerId] = { name: displayName, alias: nodePrefix, color: providerInfo.color, models: modelsToShow, isCustom: true, hasModels: nodeModels.length > 0 };
      } else {
        const built = buildAvailableModels({
          catalogModels: getModelsByProviderId(providerId) as any,
          liveModels: liveModelsByAlias[alias] || [],
          customModels: customModels as any,
          modelAliases,
          disabledIds: disabledMap[alias] || [],
          providerAlias: alias,
          type: "llm",
          freeOnly: freeOnlyByAlias[alias] || false,
        });
        let allModels = built.enabledRows.map((r) => ({
          id: r.id,
          name: r.name,
          value: r.fullModel,
          type: r.type,
          isFree: r.isFree,
          isCustom: r.source === "custom" || r.source === "legacyAlias",
        }));

        if (providerId !== "codex" && providerId !== "a6api" && allModels.length === 0 && (providerInfo.serviceKinds || ["llm"]).includes("llm")) {
          allModels = [{ id: providerId, name: providerInfo.name, value: alias }];
        }

        if (allModels.length > 0) {
          groups[providerId] = { name: providerInfo.name, alias, color: providerInfo.color, models: allModels };
        }
      }
    });

    return groups;
  }, [activeProviders, modelAliases, allProviders, providerNodes, customModels, disabledMap, liveModelsByAlias, freeOnlyByAlias, modelsByAlias]);

  // Filter models by search query
  const filteredGroups = useMemo(() => {
    if (!searchQuery.trim()) return groupedModels;

    const query = searchQuery.toLowerCase();
    const filtered: Record<string, ModelGroup> = {};

    Object.entries(groupedModels).forEach(([providerId, group]) => {
      const matchedModels = group.models.filter(
        (m) =>
          m.name.toLowerCase().includes(query) ||
          m.id.toLowerCase().includes(query)
      );

      const providerNameMatches = group.name.toLowerCase().includes(query);

      if (matchedModels.length > 0 || providerNameMatches) {
        filtered[providerId] = {
          ...group,
          models: matchedModels,
        };
      }
    });

    return filtered;
  }, [groupedModels, searchQuery]);

  const handleSelect = (model: Model) => {
    onSelect(model);
    if (closeOnSelect) {
      onClose();
      setSearchQuery("");
    }
  };

  const handleApplyMulti = () => {
    if (onSelectIds) onSelectIds(selectedIds);
    onClose();
    setSearchQuery("");
    setSelectedIds([]);
  };

  return (
    <Modal
      isOpen={isOpen}
      onClose={() => {
        onClose();
        setSearchQuery("");
      }}
      title={title}
      size="md"
      className="p-4!"
      footer={
        selectionMode === "multi" ? (
          <div className="flex w-full items-center justify-between gap-2">
            <span className="text-xs text-text-muted">
              {selectedIds.length} selected
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  onClose();
                  setSearchQuery("");
                }}
              >
                Close
              </Button>
              <Button
                size="sm"
                onClick={handleApplyMulti}
                disabled={selectedIds.length === 0}
              >
                Apply {selectedIds.length}
              </Button>
            </div>
          </div>
        ) : !closeOnSelect ? (
          <Button
            onClick={() => {
              onClose();
              setSearchQuery("");
            }}
            fullWidth
          >
            Done
          </Button>
        ) : null
      }
    >
      {/* Search - compact */}
      <div className="mb-3">
        <div className="relative">
          <span className="material-symbols-outlined absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted text-[16px]">
            search
          </span>
          <input
            type="text"
            placeholder="Search..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-8 pr-3 py-1.5 bg-surface border border-border rounded text-xs focus:outline-none focus:ring-1 focus:ring-primary/50"
          />
        </div>
      </div>

      {/* Provider outline - quick jump when many providers */}
      {Object.keys(filteredGroups).length > 1 && (
        <div className="flex gap-1.5 overflow-x-auto pb-2 -mx-1 px-1 scrollbar-thin">
          {Object.entries(filteredGroups).map(([pid, grp]) => (
            <button
              key={`outline-${pid}`}
              onClick={() => {
                const el = document.getElementById(`provider-section-${pid}`);
                const container = listRef.current;
                if (el && container) container.scrollTo({ top: el.offsetTop - container.offsetTop - 8, behavior: "smooth" });
                else el?.scrollIntoView({ behavior: "smooth", block: "start" });
              }}
              className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full border text-[11px] font-medium bg-surface border-border hover:border-primary/50 hover:bg-primary/5 transition-colors"
              title={`Jump to ${grp.name}`}
            >
              <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: grp.color }} />
              {grp.alias}
              <span className="opacity-60">({grp.models.length})</span>
            </button>
          ))}
        </div>
      )}

      {/* Models grouped by provider - compact */}
      <div ref={listRef} className="max-h-[400px] overflow-y-auto space-y-3 scroll-smooth">
        {/* Provider models */}
        {Object.entries(filteredGroups).map(([providerId, group]) => (
          <div key={providerId} id={`provider-section-${providerId}`}>
            {/* Provider header */}
            <div className="flex items-center gap-1.5 mb-1.5 sticky top-0 bg-surface py-0.5">
              <div
                className="w-2 h-2 rounded-full"
                style={{ backgroundColor: group.color }}
              />
              <span className="text-xs font-medium text-primary">
                {group.name}
              </span>
              <span className="text-[10px] text-text-muted">
                ({group.models.length})
              </span>
            </div>

            <div className="flex flex-wrap gap-1.5">
              {group.models.map((model) => {
                const isPlaceholder = model.isPlaceholder;
                const isLlm = model.type === "llm";
                const favAlias = getProviderAlias(providerId);
                const fav = isLlm ? isFavorite(favAlias, model.id) : false;
                const isMulti = selectionMode === "multi";
                const isMultiSelected = isMulti && selectedIds.includes(model.value);
                const isSingleSelected = !isMulti && (Array.isArray(selectedModel)
                  ? selectedModel.includes(model.value)
                  : selectedModel === model.value);
                const rowClick = () => {
                  if (isMulti) toggleSelect(model.value);
                  else handleSelect(model);
                };
                return (
                  <div
                    key={model.value}
                    onClick={rowClick}
                    title={isPlaceholder ? "Select to pre-fill, then edit model ID in the input" : undefined}
                    className={`
                      inline-flex items-center gap-1 px-2 py-1 rounded-xl text-xs font-medium transition-all border hover:cursor-pointer
                      ${isPlaceholder
                        ? "border-dashed border-border text-text-muted hover:border-primary/50 hover:text-primary bg-surface italic"
                        : isMultiSelected
                          ? "border-primary bg-primary/10 text-text-main"
                          : isSingleSelected
                            ? "bg-primary text-white border-primary"
                            : "bg-surface border-border text-text-main hover:border-primary/50 hover:bg-primary/5"
                      }
                    `}
                  >
                    {isLlm && (
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          toggleFavorite(favAlias, model.id);
                        }}
                        className="shrink-0 rounded p-0.5 -ml-1 hover:bg-black/5 dark:hover:bg-white/5"
                        title={fav ? "Remove from favorites" : "Add to favorites"}
                        aria-label={fav ? "Remove from favorites" : "Add to favorites"}
                      >
                        <span className={`material-symbols-outlined text-[14px] ${fav ? "text-yellow-400" : "text-text-muted"}`}>
                          {fav ? "star" : "star_outline"}
                        </span>
                      </button>
                    )}
                    {isMulti && (
                      <input
                        type="checkbox"
                        checked={isMultiSelected}
                        onChange={() => toggleSelect(model.value)}
                        onClick={(e) => e.stopPropagation()}
                        className="h-3.5 w-3.5 rounded border-gray-300 text-primary focus:ring-primary shrink-0"
                        aria-label={`Select ${model.name}`}
                      />
                    )}
                    {isPlaceholder ? (
                      <span className="flex items-center gap-1">
                        <span className="material-symbols-outlined text-[11px]">edit</span>
                        {model.name}
                      </span>
                    ) : model.isCustom ? (
                      <span className="flex items-center gap-1">
                        {model.name}
                        <span className="text-[9px] opacity-60 font-normal">custom</span>
                        <CapacityBadges caps={getCaps(model.value)} size={12} colorOverride={isMultiSelected || isSingleSelected ? "text-white/80" : undefined} />
                      </span>
                    ) : (
                      <span className="flex items-center gap-1">
                        {model.name}
                        <CapacityBadges caps={getCaps(model.value)} size={12} colorOverride={isMultiSelected || isSingleSelected ? "text-white/80" : undefined} />
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        ))}

        {Object.keys(filteredGroups).length === 0 && (
          <div className="text-center py-4 text-text-muted">
            <span className="material-symbols-outlined text-2xl mb-1 block">
              search_off
            </span>
            <p className="text-xs">No models found</p>
          </div>
        )}
      </div>
    </Modal>
  );
}
