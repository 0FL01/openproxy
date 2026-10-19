import type { CatalogModel } from "@/store/catalogStore";
import type { ModelCaps } from "@/shared/constants/models";

/** xAI metadata is exact and provider-scoped; absence means unknown, never a name guess. */
export function isXaiModelScope(provider: string | null | undefined, model: string): boolean {
  return provider === "xai" || (!provider && model.startsWith("xai/"));
}

export function findXaiCatalogModel(
  model: string,
  models: ReadonlyArray<CatalogModel>,
): CatalogModel | undefined {
  const id = model.startsWith("xai/") ? model.slice(4) : model;
  return models.find((entry) => entry.id === id && entry.kind === "llm");
}

export function xaiModelCaps(model: CatalogModel | undefined): ModelCaps {
  const caps: ModelCaps = {};
  if (model?.capabilities?.includes("vision")) caps.vision = true;
  if (model?.capabilities?.includes("reasoning")) caps.reasoning = true;
  return caps;
}
