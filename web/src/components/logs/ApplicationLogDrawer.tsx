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
            <Field label="Request ID" value={log.requestId} />
            <Field label="Route" value={log.route} />
            <Field label="Model" value={log.model} />
            <Field label="Provider" value={log.provider} />
            <Field label="API key" value={log.apiKeyName} />
            <Field label="API key ID" value={log.apiKeyId} />
            <Field label="Started" value={new Date(log.timestamp).toLocaleString()} />
            <Field label="Duration" value={`${log.durationMs} ms`} />
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
