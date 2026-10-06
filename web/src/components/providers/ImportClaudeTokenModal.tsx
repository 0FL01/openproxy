import { useState } from "react";
import { Button, Modal } from "@/shared/components";

interface ImportClaudeTokenModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSuccess?: () => void;
}

interface ImportResult {
  success: boolean;
  connection?: { id: string; email?: string | null; name?: string | null };
  error?: string;
}

export default function ImportClaudeTokenModal({ isOpen, onClose, onSuccess }: ImportClaudeTokenModalProps): React.ReactNode {
  const [token, setToken] = useState("");
  const [name, setName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<ImportResult | null>(null);

  const handleClose = () => {
    if (submitting) return;
    setToken("");
    setName("");
    setError("");
    setResult(null);
    onClose();
  };

  const handleSubmit = async () => {
    setError("");
    setResult(null);

    const trimmed = token.trim();
    if (!trimmed) {
      setError("Paste the OAuth token first");
      return;
    }

    setSubmitting(true);
    try {
      const res = await fetch("/api/oauth/claude/import-token", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          accessToken: trimmed,
          ...(name.trim() ? { name: name.trim() } : {}),
        }),
      });
      const data: ImportResult = await res.json();
      if (!res.ok) {
        setError((data as ImportResult)?.error || `Request failed: ${res.status}`);
        return;
      }
      setResult(data);
      if (typeof onSuccess === "function") onSuccess();
    } catch (err) {
      setError((err as Error).message || "Request failed");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal isOpen={isOpen} title="Import Claude Setup Token" onClose={handleClose}>
      <div className="flex flex-col gap-4">
        <p className="text-xs text-text-muted">
          Create a long-lived (1-year) token with <code className="font-mono">claude setup-token</code> and
          paste it below. The token is validated against your Anthropic account before saving.
        </p>

        <label className="flex flex-col gap-1 text-xs font-medium">
          OAuth token
          <input
            type="text"
            className="w-full rounded border border-accent/30 bg-sidebar px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-primary"
            placeholder="sk-ant-oat01-…"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            disabled={submitting}
            autoComplete="off"
            spellCheck={false}
          />
        </label>

        <label className="flex flex-col gap-1 text-xs font-medium">
          Display name <span className="text-text-muted font-normal">(optional)</span>
          <input
            type="text"
            className="w-full rounded border border-accent/30 bg-sidebar px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-primary"
            placeholder="Work laptop setup token"
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={submitting}
          />
        </label>

        {error && <p className="text-xs text-red-500 break-words">{error}</p>}

        {result?.success && (
          <p className="text-sm font-medium text-green-600">
            Imported{result.connection?.email ? ` as ${result.connection.email}` : ""}.
          </p>
        )}

        <div className="flex gap-2">
          <Button onClick={handleSubmit} disabled={submitting || !token.trim()}>
            {submitting ? "Validating…" : "Import Token"}
          </Button>
          <Button onClick={handleClose} variant="ghost">Close</Button>
        </div>
      </div>
    </Modal>
  );
}
