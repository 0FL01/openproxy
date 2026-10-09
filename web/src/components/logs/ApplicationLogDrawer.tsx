import Drawer from "@/shared/components/Drawer";
import type { ApplicationLog, StreamTrace } from "./types";

interface Props {
  log: ApplicationLog | null;
  onClose: () => void;
}

function Field({ label, value }: { label: string; value?: string | number | null }) {
  return (
    <div className="min-w-0">
      <div className="text-xs font-medium uppercase tracking-wide text-text-muted">{label}</div>
      <div className="mt-1 break-all font-mono text-sm text-text-main">{value ?? "—"}</div>
    </div>
  );
}

function CountsGrid({ title, counts }: { title: string; counts?: Record<string, number> }) {
  const entries = Object.entries(counts ?? {});
  if (entries.length === 0) return null;
  return (
    <div>
      <div className="text-xs font-medium uppercase tracking-wide text-text-muted">{title}</div>
      <div className="mt-1 grid grid-cols-1 gap-1 sm:grid-cols-2">
        {entries.map(([key, count]) => (
          <div key={key} className="flex items-baseline justify-between gap-2 font-mono text-xs text-text-main">
            <span className="break-all">{key}</span>
            <span className="text-text-muted">×{count}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function StreamTraceBlock({ trace }: { trace: StreamTrace }) {
  return (
    <details className="rounded-lg border border-border bg-bg-subtle p-4">
      <summary className="cursor-pointer text-sm font-semibold text-text-main">Stream trace</summary>
      <div className="mt-3 space-y-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Field label="Stop reason" value={trace.stopReason} />
          <Field label="Finish reason" value={trace.finishReason} />
          <Field label="Completed" value={trace.completedCount} />
          <Field label="Errors" value={trace.errorCount} />
          <Field label="After completed" value={trace.framesAfterCompleted} />
          <Field label="[DONE] sent" value={trace.doneSent ? "yes" : "no"} />
          <Field label="Overflowed" value={trace.overflowed ?? 0} />
          {trace.toolNames && trace.toolNames.length > 0 && (
            <Field label="Tool calls" value={trace.toolNames.join(", ")} />
          )}
        </div>
        <CountsGrid title="Upstream events" counts={trace.upstreamEvents} />
        <CountsGrid title="Emitted events" counts={trace.emittedEvents} />
        <CountsGrid title="Item types" counts={trace.itemTypes} />
        <div>
          <div className="text-xs font-medium uppercase tracking-wide text-text-muted">Event order</div>
          <ol className="mt-1 space-y-0.5 font-mono text-xs text-text-main">
            {trace.entries.map((entry, index) => (
              <li key={`${index}-${entry}`} className="break-all">
                <span className="text-text-muted">{String(index + 1).padStart(2, "0")}</span> {entry}
              </li>
            ))}
          </ol>
        </div>
      </div>
    </details>
  );
}

export default function ApplicationLogDrawer({ log, onClose }: Props) {
  return (
    <Drawer isOpen={Boolean(log)} onClose={onClose} title="Request details" width="lg">
      {log && (
        <div className="space-y-6">
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            <Field label="Status" value={log.status.toUpperCase()} />
            <Field label="HTTP status" value={log.statusCode} />
            {log.errorKind && <Field label="Error kind" value={log.errorKind} />}
            {log.errorCode && <Field label="Error code" value={log.errorCode} />}
            <Field label="Request ID" value={log.requestId} />
            <Field label="Route" value={log.route} />
            <Field label="Model" value={log.model} />
            <Field label="Provider" value={log.provider} />
            <Field label="API key" value={log.apiKeyName} />
            <Field label="API key ID" value={log.apiKeyId} />
            <Field label="Started" value={new Date(log.timestamp).toLocaleString()} />
            <Field label="Duration" value={`${log.durationMs} ms`} />
          </div>

          {log.errorMessage && (
            <div className="rounded-lg border border-error/25 bg-error/5 p-4">
              <div className="text-xs font-medium uppercase tracking-wide text-text-muted">Error message</div>
              <p className="mt-1 whitespace-pre-wrap break-words font-mono text-sm text-text-main">{log.errorMessage}</p>
            </div>
          )}

          {log.streamTrace && <StreamTraceBlock trace={log.streamTrace} />}

          <div className="rounded-lg border border-border bg-bg-subtle p-4">
            <h3 className="mb-3 text-sm font-semibold text-text-main">Observed throughput</h3>
            {log.status === "success" && log.tokensPerSecond != null && log.generatedOutputTokens != null && log.upstreamDurationMs != null ? (
              <>
                <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
                  <Field label="Generated output" value={log.generatedOutputTokens.toLocaleString()} />
                  <Field label="Upstream time" value={`${log.upstreamDurationMs} ms`} />
                  <Field label="TPS" value={log.tokensPerSecond.toFixed(1)} />
                </div>
                <p className="mt-3 break-all font-mono text-xs text-text-muted">{log.generatedOutputTokens} ÷ ({log.upstreamDurationMs} / 1000) = {log.tokensPerSecond.toFixed(1)} tokens/s</p>
              </>
            ) : <p className="text-sm text-text-muted">— · Final original usage and completion timing are unavailable.</p>}
            <p className="mt-3 text-xs text-text-muted">Original provider-generated output divided by observed upstream time. Includes network, initial wait and downstream backpressure; this is observed throughput, not GPU decode speed.</p>
          </div>

          <div className="rounded-lg border border-border bg-bg-subtle p-4">
            <h3 className="mb-3 text-sm font-semibold text-text-main">Tokens</h3>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
              <Field label="Input" value={(log.inputTokens ?? 0).toLocaleString()} />
              <Field label="Cache Read" value={log.cachedTokens?.toLocaleString() ?? null} />
              <Field label="Output" value={(log.outputTokens ?? 0).toLocaleString()} />
            </div>
          </div>
        </div>
      )}
    </Drawer>
  );
}
