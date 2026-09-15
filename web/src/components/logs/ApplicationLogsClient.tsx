"use client";

import { useEffect, useState } from "react";
import Button from "@/shared/components/Button";
import Card from "@/shared/components/Card";
import Pagination from "@/shared/components/Pagination";
import ApplicationLogDrawer from "./ApplicationLogDrawer";
import type { ApplicationLog, LogsPayload } from "./types";
import { inputTokens, outputTokens } from "./types";

const REFRESH_MS = 10_000;

interface Filters {
  apiKeyId: string;
  status: string;
  provider: string;
  model: string;
  correlationId: string;
}

interface ApiKeyOption {
  id: string;
  name: string;
}

function statusClass(status: string) {
  if (status === "success") return "bg-success/10 text-success";
  if (status === "error") return "bg-error/10 text-error";
  if (status === "pending") return "bg-primary/10 text-primary animate-pulse";
  return "bg-warning/10 text-warning";
}

export default function ApplicationLogsClient() {
  const [logs, setLogs] = useState<ApplicationLog[]>([]);
  const [keys, setKeys] = useState<ApiKeyOption[]>([]);
  const [selected, setSelected] = useState<ApplicationLog | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [totalItems, setTotalItems] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [filters, setFilters] = useState<Filters>({ apiKeyId: "", status: "", provider: "", model: "", correlationId: "" });

  useEffect(() => {
    fetch("/api/keys")
      .then(async (response) => (response.ok ? response.json() : { keys: [] }))
      .then((data) => setKeys((data.keys ?? []).map(({ id, name }: ApiKeyOption) => ({ id, name }))))
      .catch(() => setKeys([]));
  }, []);

  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;

    const load = async (showLoading: boolean) => {
      if (document.visibilityState === "hidden") return;
      if (showLoading) setLoading(true);
      controller = new AbortController();
      const params = new URLSearchParams({ page: String(page), pageSize: String(pageSize) });
      Object.entries(filters).forEach(([key, value]) => value && params.set(key, value));
      try {
        const response = await fetch(`/api/usage/request-details?${params}`, {
          cache: "no-store",
          signal: controller.signal,
        });
        if (!response.ok) throw new Error(`Failed to load logs (${response.status})`);
        const data: LogsPayload = await response.json();
        if (!disposed) {
          setLogs(data.details ?? []);
          setTotalItems(data.pagination?.totalItems ?? 0);
          setError("");
        }
      } catch (cause) {
        if (!disposed && !(cause instanceof DOMException && cause.name === "AbortError")) {
          setError(cause instanceof Error ? cause.message : "Failed to load logs");
        }
      } finally {
        if (!disposed) {
          setLoading(false);
          if (page === 1) timer = setTimeout(() => load(false), REFRESH_MS);
        }
      }
    };

    const refreshOnFocus = () => setRefresh((value) => value + 1);
    const refreshWhenVisible = () => document.visibilityState === "visible" && refreshOnFocus();
    void load(true);
    window.addEventListener("focus", refreshOnFocus);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      disposed = true;
      controller?.abort();
      if (timer) clearTimeout(timer);
      window.removeEventListener("focus", refreshOnFocus);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  }, [filters, page, pageSize, refresh]);

  const updateFilter = (name: keyof Filters, value: string) => {
    setPage(1);
    setFilters((current) => ({ ...current, [name]: value }));
  };

  return (
    <div className="flex min-w-0 flex-col gap-5 px-1 sm:px-0">
      <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-center">
        <div>
          <h1 className="text-2xl font-semibold text-text-main">Application Logs</h1>
          <p className="text-sm text-text-muted">Provider attempts attributed to API keys · refreshes every 10 seconds</p>
        </div>
        <Button variant="outline" onClick={() => setRefresh((value) => value + 1)}>
          <span className="material-symbols-outlined text-[18px]">refresh</span> Refresh
        </Button>
      </div>

      <Card padding="md">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-5">
          <select value={filters.apiKeyId} onChange={(event) => updateFilter("apiKeyId", event.target.value)} className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main">
            <option value="">All API keys</option>
            {keys.map((key) => <option key={key.id} value={key.id}>{key.name}</option>)}
          </select>
          <select value={filters.status} onChange={(event) => updateFilter("status", event.target.value)} className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main">
            <option value="">All statuses</option>
            <option value="pending">Live</option><option value="success">Success</option>
            <option value="error">Error</option><option value="interrupted">Interrupted</option>
          </select>
          <input value={filters.provider} onChange={(event) => updateFilter("provider", event.target.value)} placeholder="Provider" className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main" />
          <input value={filters.model} onChange={(event) => updateFilter("model", event.target.value)} placeholder="Actual model" className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main" />
          <input value={filters.correlationId} onChange={(event) => updateFilter("correlationId", event.target.value)} placeholder="Correlation ID" className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main" />
        </div>
      </Card>

      {error && <div className="rounded-lg border border-error/25 bg-error/5 px-4 py-3 text-sm text-error">{error}</div>}

      <Card padding="none">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[1050px] text-sm">
            <thead><tr className="border-b border-border text-left text-xs uppercase tracking-wide text-text-muted">
              <th className="p-4">Status</th><th className="p-4">Model</th><th className="p-4">Provider</th>
              <th className="p-4">Account</th><th className="p-4">API key</th><th className="p-4 text-right">Tokens</th>
              <th className="p-4 text-right">Duration</th><th className="p-4">Time</th>
            </tr></thead>
            <tbody>
              {loading && logs.length === 0 ? <tr><td colSpan={8} className="p-10 text-center text-text-muted">Loading logs…</td></tr> :
               logs.length === 0 ? <tr><td colSpan={8} className="p-10 text-center text-text-muted">No application logs yet.</td></tr> :
               logs.map((log) => <tr key={log.id} onClick={() => setSelected(log)} className="cursor-pointer border-b border-border/60 transition-colors hover:bg-primary/5">
                 <td className="p-4"><span className={`rounded-full px-2 py-1 text-xs font-semibold ${statusClass(log.status)}`}>{log.status === "pending" ? "LIVE" : log.status.toUpperCase()}</span></td>
                 <td className="max-w-[240px] p-4"><div className="truncate font-mono text-text-main">{log.model}</div>{log.requestedModel && log.requestedModel !== log.model && <div className="truncate text-xs text-text-muted">{log.requestedModel}</div>}</td>
                 <td className="p-4 text-text-main">{log.provider || "—"}</td><td className="p-4 text-text-main">{log.account ?? log.connectionId ?? "—"}</td>
                 <td className="p-4 font-medium text-text-main">{log.apiKeyName ?? "—"}</td>
                 <td className="p-4 text-right font-mono text-text-main">{inputTokens(log.tokens).toLocaleString()} / {outputTokens(log.tokens).toLocaleString()}</td>
                 <td className="p-4 text-right font-mono text-text-main">{(log.latency?.total ?? 0).toLocaleString()} ms</td>
                 <td className="whitespace-nowrap p-4 text-text-muted">{new Date(log.timestamp).toLocaleString()}</td>
               </tr>)}
            </tbody>
          </table>
        </div>
        {!loading && totalItems > 0 && <Pagination currentPage={page} pageSize={pageSize} totalItems={totalItems} onPageChange={setPage} onPageSizeChange={(size) => { setPage(1); setPageSize(size); }} className="border-t border-border px-4" />}
      </Card>
      <ApplicationLogDrawer log={selected} onClose={() => setSelected(null)} />
    </div>
  );
}
