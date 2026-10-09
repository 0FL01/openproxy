import Drawer from "@/shared/components/Drawer";
import type { ApplicationLog } from "./types";

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
