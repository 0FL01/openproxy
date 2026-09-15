import Drawer from "@/shared/components/Drawer";
import type { ApplicationLog } from "./types";
import { inputTokens, outputTokens } from "./types";

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
            <Field label="Request ID" value={log.id} />
            <Field label="Correlation ID" value={log.correlationId} />
            <Field label="API key" value={log.apiKeyName} />
            <Field label="Endpoint" value={log.endpoint} />
            <Field label="Requested model" value={log.requestedModel} />
            <Field label="Actual model" value={log.model} />
            <Field label="Provider" value={log.provider} />
            <Field label="Account" value={log.account ?? log.connectionId} />
            <Field label="Started" value={new Date(log.timestamp).toLocaleString()} />
            <Field label="Duration" value={`${log.latency?.total ?? 0} ms`} />
          </div>

          <div className="rounded-lg border border-border bg-bg-subtle p-4">
            <h3 className="mb-3 text-sm font-semibold text-text-main">Tokens and cost</h3>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
              <Field label="Input" value={inputTokens(log.tokens).toLocaleString()} />
              <Field label="Output" value={outputTokens(log.tokens).toLocaleString()} />
              <Field label="Cache read" value={(log.tokens?.cache_read_input_tokens ?? 0).toLocaleString()} />
              <Field label="Cost" value={`$${(log.cost ?? 0).toFixed(6)}`} />
            </div>
          </div>

          {log.error && (
            <div className="rounded-lg border border-error/25 bg-error/5 p-4">
              <h3 className="mb-2 text-sm font-semibold text-error">Error</h3>
              <pre className="whitespace-pre-wrap break-words text-xs text-text-main">{log.error}</pre>
            </div>
          )}
        </div>
      )}
    </Drawer>
  );
}
