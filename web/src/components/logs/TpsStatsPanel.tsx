import { useEffect, useRef, useState } from "react";
import Card from "@/shared/components/Card";
import Pagination from "@/shared/components/Pagination";
import type { TpsStatsGroupId, TpsStatsPayload, TpsStatsWindow } from "./types";

const WINDOWS: TpsStatsWindow[] = ["1h", "6h", "24h"];
const REFRESH_MS = 60_000;
const INITIAL_DELAY_MS = 300;

interface TpsStatsPanelProps {
  apiKeyId: string;
  provider: string;
  model: string;
  refreshToken: number;
  hiddenProviders: TpsStatsGroupId[];
  onHiddenProvidersChange: (hiddenProviders: TpsStatsGroupId[]) => void;
}

interface RequestState {
  scope: string;
  data: TpsStatsPayload | null;
  loading: boolean;
  error: string;
}

interface ProviderOptionsState {
  baseScope: string;
  providers: TpsStatsGroupId[] | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isTimestamp(value: unknown): value is string {
  return typeof value === "string" && value.trim() !== "" && Number.isFinite(Date.parse(value));
}

function isGroupId(value: unknown): value is TpsStatsGroupId {
  return value === null || typeof value === "string";
}

function isTpsStatsPayload(value: unknown, page: number, pageSize: number): value is TpsStatsPayload {
  if (!isRecord(value) || !isTimestamp(value.asOf) || !isRecord(value.windows)
    || !Array.isArray(value.availableProviders) || !value.availableProviders.every(isGroupId)
    || !Array.isArray(value.groups) || !isRecord(value.pagination)) return false;
  const availableProviders = new Set<TpsStatsGroupId>(value.availableProviders);
  if (availableProviders.size !== value.availableProviders.length) return false;

  const bounds = value.windows;
  if (!WINDOWS.every((window) => {
    const range = bounds[window];
    return isRecord(range) && isTimestamp(range.start) && isTimestamp(range.end)
      && Date.parse(range.start) < Date.parse(range.end);
  })) return false;

  const pagination = value.pagination;
  if (!isCount(pagination.page) || pagination.page < 1 || pagination.page !== page
    || !isCount(pagination.pageSize) || pagination.pageSize < 1 || pagination.pageSize > 100
    || pagination.pageSize !== pageSize || !isCount(pagination.totalItems)
    || !isCount(pagination.totalPages) || value.groups.length > pageSize) return false;
  const expectedPages = Math.ceil(pagination.totalItems / pageSize);
  if (pagination.totalItems === 0 ? pagination.totalPages > 1 : pagination.totalPages !== expectedPages) return false;
  const expectedGroups = Math.min(pageSize, Math.max(0, pagination.totalItems - (page - 1) * pageSize));
  if (value.groups.length !== expectedGroups) return false;

  return value.groups.every((group: unknown) => {
    if (!isRecord(group) || !isGroupId(group.provider) || !isGroupId(group.model)
      || !availableProviders.has(group.provider) || !isRecord(group.windows)) return false;
    const observations = group.windows;
    return WINDOWS.every((window) => {
      const observation = observations[window];
      if (!isRecord(observation) || !isCount(observation.sampleCount)
        || !isCount(observation.successfulAttemptCount)
        || observation.sampleCount > observation.successfulAttemptCount) return false;
      return observation.medianTps === null
        ? observation.sampleCount === 0
        : typeof observation.medianTps === "number" && Number.isFinite(observation.medianTps)
          && observation.medianTps >= 0 && observation.sampleCount > 0;
    });
  });
}

function displayGroupId(value: TpsStatsGroupId) {
  if (value === null) return <span className="italic text-text-muted">Unknown</span>;
  if (value === "") return <span className="italic text-text-muted">Empty</span>;
  return value;
}

export default function TpsStatsPanel({ apiKeyId, provider, model, refreshToken, hiddenProviders, onHiddenProvidersChange }: TpsStatsPanelProps) {
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const baseScope = JSON.stringify([apiKeyId, provider, model]);
  const scope = JSON.stringify([apiKeyId, provider, model, hiddenProviders, page, pageSize]);
  const [requestState, setRequestState] = useState<RequestState>({ scope, data: null, loading: true, error: "" });
  const [providerOptions, setProviderOptions] = useState<ProviderOptionsState>({ baseScope, providers: null });
  const firstScope = useRef(true);
  const refreshRequest = useRef<(() => void) | null>(null);
  const previousRefresh = useRef(refreshToken);
  // Page/size/exclusion changes hide the old snapshot but retain choices for this base scope.
  const current = requestState.scope === scope
    ? requestState
    : { scope, data: null, loading: true, error: "" };
  const { data, loading, error } = current;
  const availableProviders = providerOptions.baseScope === baseScope ? providerOptions.providers : null;

  useEffect(() => {
    let disposed = false;
    let running = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;

    const clearTimer = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };

    const schedule = (delay: number) => {
      clearTimer();
      if (disposed || document.visibilityState !== "visible") return;
      timer = setTimeout(() => {
        timer = undefined;
        void load();
      }, delay);
    };

    const load = async () => {
      // Same-scope refresh signals share the running request, including its error outcome.
      if (disposed || running || document.visibilityState !== "visible") return;
      clearTimer();
      running = true;
      firstScope.current = false;
      const activeController = new AbortController();
      controller = activeController;
      let changingPage = false;
      setRequestState((currentState) => ({
        scope,
        data: currentState.scope === scope ? currentState.data : null,
        loading: true,
        error: "",
      }));
      const params = new URLSearchParams({ page: String(page), pageSize: String(pageSize) });
      Object.entries({ apiKeyId, provider, model }).forEach(([key, value]) => {
        const trimmed = value.trim();
        if (trimmed) params.set(key, trimmed);
      });
      if (hiddenProviders.length > 0) params.set("hiddenProviders", JSON.stringify(hiddenProviders));
      try {
        const response = await fetch(`/api/request-logs/tps-stats?${params}`, {
          cache: "no-store",
          signal: activeController.signal,
        });
        if (!response.ok) throw new Error(`Failed to load TPS statistics (${response.status})`);
        const payload: unknown = await response.json();
        if (disposed || activeController.signal.aborted) return;
        if (!isTpsStatsPayload(payload, page, pageSize)) throw new Error("Invalid TPS statistics response");
        setProviderOptions({ baseScope, providers: payload.availableProviders });
        // Only a successful, current response may correct a page after groups disappear.
        const lastPage = Math.max(1, payload.pagination.totalPages);
        if (page > lastPage || (payload.pagination.totalItems === 0 && page !== 1)) {
          changingPage = true;
          setPage(payload.pagination.totalItems === 0 ? 1 : lastPage);
          return;
        }
        setRequestState({ scope, data: payload, loading: false, error: "" });
      } catch (cause) {
        if (!disposed && !activeController.signal.aborted) {
          setRequestState((currentState) => ({
            scope,
            data: currentState.scope === scope ? currentState.data : null,
            loading: false,
            error: cause instanceof Error ? cause.message : "Failed to load TPS statistics",
          }));
        }
      } finally {
        if (!disposed) {
          running = false;
          controller = undefined;
          if (!changingPage) schedule(REFRESH_MS);
        }
      }
    };

    refreshRequest.current = () => { void load(); };
    schedule(firstScope.current ? INITIAL_DELAY_MS : 0);
    return () => {
      disposed = true;
      clearTimer();
      controller?.abort();
      refreshRequest.current = null;
    };
  }, [apiKeyId, provider, model, hiddenProviders, page, pageSize, baseScope, scope]);

  useEffect(() => {
    if (previousRefresh.current === refreshToken) return;
    previousRefresh.current = refreshToken;
    refreshRequest.current?.();
  }, [refreshToken]);

  return (
    <Card padding="none" role="region" aria-label="Median TPS statistics">
      <div className="border-b border-border p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-base font-semibold text-text-main">Median TPS by provider and model</h2>
          <div className="flex flex-wrap items-center gap-2 text-xs text-text-muted">
            {data && <span>As of <time dateTime={data.asOf} title={data.asOf}>{new Date(data.asOf).toLocaleString()}</time></span>}
            {loading && data && <span className="text-primary">Updating…</span>}
            {error && <span className="text-error">{data ? "Stale" : "Unavailable"}</span>}
          </div>
        </div>
        <p className="mt-1 text-xs font-medium text-text-muted">Success-only · saved attempts, not all traffic. Status filters apply only to raw logs.</p>
        <p className="mt-1 text-xs text-text-muted">Sorted by highest-priority available window: 1h → 6h → 24h, TPS descending; models without measurements last.</p>
        <p className="mt-1 text-xs text-text-muted">Rolling windows use attempt start time. TPS is observed request throughput, including network, initial wait and backpressure, not GPU speed or a comparable benchmark. Coverage: eligible TPS samples / saved successful attempts.</p>
      </div>
      <fieldset className="border-b border-border px-4 pb-4 pt-2">
        <legend className="pt-3 text-xs font-medium text-text-muted">Providers (TPS only)</legend>
        <div className="flex flex-wrap gap-x-4 gap-y-2">
          {availableProviders === null ? <p className="text-xs text-text-muted">Provider choices will appear after TPS statistics load.</p> :
            availableProviders.length === 0 ? <p className="text-xs text-text-muted">No matching providers.</p> :
            availableProviders.map((providerId) => <label key={JSON.stringify(providerId)} className="flex min-w-0 cursor-pointer items-center gap-2 text-sm text-text-main">
              <input type="checkbox" checked={!hiddenProviders.includes(providerId)} onChange={(event) => {
                setPage(1);
                onHiddenProvidersChange(event.target.checked
                  ? hiddenProviders.filter((id) => id !== providerId)
                  : [...hiddenProviders, providerId]);
              }} className="h-4 w-4 shrink-0 accent-primary" />
              <span className="break-all">{displayGroupId(providerId)}</span>
            </label>)}
        </div>
      </fieldset>
      {error && <div role="status" aria-live="polite" className="border-b border-error/25 bg-error/5 px-4 py-3 text-sm text-error">{error}{data && " · Showing the last successful snapshot."}</div>}
      <div className="overflow-x-auto">
        <table className="w-full min-w-[600px] text-sm" aria-label="Median TPS by provider and model">
          <thead>
            <tr className="border-b border-border text-left text-xs uppercase tracking-wide text-text-muted">
              <th scope="col" className="p-4">Provider</th>
              <th scope="col" className="p-4">Model</th>
              {WINDOWS.map((window) => <th key={window} scope="col" className="p-4 text-right" title={data ? `${data.windows[window].start} ≤ attempt start < ${data.windows[window].end}` : undefined}>{window}</th>)}
            </tr>
          </thead>
          <tbody>
            {!data ? <tr><td colSpan={5} className="p-8 text-center text-text-muted">{loading ? "Loading TPS statistics…" : "TPS statistics unavailable."}</td></tr> :
              data.groups.length === 0 ? <tr><td colSpan={5} className="p-8 text-center text-text-muted">{data.availableProviders.length > 0 ? "All matching providers are hidden" : "No saved successful attempts in the last 24 hours for these filters."}</td></tr> :
              data.groups.map((group) => (
                <tr key={JSON.stringify([group.provider, group.model])} className="border-b border-border/60">
                  <th scope="row" className="max-w-[200px] break-words p-4 text-left font-normal text-text-main">{displayGroupId(group.provider)}</th>
                  <td className="max-w-[320px] break-all p-4 font-mono text-text-main">{displayGroupId(group.model)}</td>
                  {WINDOWS.map((window) => {
                    const observation = group.windows[window];
                    return <td key={window} className="whitespace-nowrap p-4 text-right">
                      <div className="font-mono text-text-main">{observation.medianTps === null ? "—" : observation.medianTps.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1, useGrouping: false })}</div>
                      <div className="mt-1 font-mono text-xs text-text-muted" title="Eligible TPS samples / saved successful attempts">{observation.sampleCount}/{observation.successfulAttemptCount}</div>
                    </td>;
                  })}
                </tr>
              ))}
          </tbody>
        </table>
      </div>
      <nav aria-label="TPS statistics pagination">
        <Pagination currentPage={page} pageSize={pageSize} totalItems={data?.pagination.totalItems ?? 0} onPageChange={setPage} onPageSizeChange={(size) => { setPage(1); setPageSize(size); }} className="border-t border-border px-4" />
      </nav>
    </Card>
  );
}
