import { useEffect, useRef, useState } from "react";
import Modal from "@/shared/components/Modal";
import Button from "@/shared/components/Button";
import {
  ClaudeNativeLogin, nativeLoginInitialState, nativeLoginLinks,
} from "@/shared/utils/claudeNativeLogin";

interface ClaudeNativeLoginModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSuccess: (connectionId?: string) => Promise<void> | void;
}

export default function ClaudeNativeLoginModal({ isOpen, onClose, onSuccess }: ClaudeNativeLoginModalProps) {
  const [state, setState] = useState(nativeLoginInitialState);
  const [name, setName] = useState("");
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [copyError, setCopyError] = useState("");
  const controller = useRef<ClaudeNativeLogin | null>(null);
  const success = useRef(onSuccess);
  success.current = onSuccess;
  const content = useRef<HTMLDivElement>(null);
  const output = useRef<HTMLPreElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const previousFocus = document.activeElement as HTMLElement | null;
    const login = new ClaudeNativeLogin({ onState: setState, onSuccess: (id) => success.current(id) });
    controller.current = login;
    setState(nativeLoginInitialState());
    setName(""); setInput(""); setSending(false); setCopyError("");
    const dialog = content.current?.closest('[role="dialog"]') as HTMLElement | null;
    content.current?.querySelector<HTMLInputElement>("input")?.focus();
    const trapFocus = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || !dialog) return;
      const nodes = [...dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), a[href], [tabindex="0"]')];
      const first = nodes[0]; const last = nodes[nodes.length - 1];
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
        event.preventDefault(); last?.focus();
      } else if (!event.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) {
        event.preventDefault(); first?.focus();
      }
    };
    document.addEventListener("keydown", trapFocus);
    return () => {
      login.close();
      if (controller.current === login) controller.current = null;
      document.removeEventListener("keydown", trapFocus);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [isOpen]);

  useEffect(() => {
    if (output.current) output.current.scrollTop = output.current.scrollHeight;
  }, [state.output]);

  const close = () => { controller.current?.close(); onClose(); };
  const pending = state.status === "pending";
  const busy = pending || state.status === "starting";
  const send = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!pending || sending) return;
    const login = controller.current;
    setSending(true);
    const accepted = await login?.input(`${input}\n`);
    if (controller.current !== login) return;
    if (accepted) setInput("");
    setSending(false);
    inputRef.current?.focus();
  };

  return (
    <Modal isOpen={isOpen} title="Connect Claude Agent" onClose={close} size="xl">
      <div ref={content} className="flex flex-col gap-4">
        <p className="text-sm text-text-muted">
          This relays the original <code>claude auth login</code> running on the OpenProxy server.
          Follow its native instructions below. Sign in and enter your password only on Anthropic’s website,
          never here. No Claude installation is needed on your OpenCode client.
        </p>
        <label className="text-sm">
          Account name (optional)
          <input className="mt-1 w-full px-3 py-2 text-sm border border-border rounded-lg bg-background focus:outline-none focus:border-primary"
            value={name} onChange={(event) => setName(event.target.value)} disabled={busy || state.status === "success"}
            maxLength={120} autoComplete="off" placeholder="My Claude subscription" />
        </label>
        <p role="status" className="text-sm">
          {state.status === "ready" ? "Ready to start native login." : state.status === "starting" ? "Starting native login…"
            : pending ? "Native login is running. Follow its output; input is forwarded unchanged with Enter."
            : state.status === "success" ? "Native login and account verification succeeded."
            : state.status === "cancelled" ? "Native login was cancelled." : "Native login failed."}
        </p>
        {state.status !== "ready" && (
          <>
            <pre ref={output} role="log" aria-label="Native Claude login output" aria-live="polite" tabIndex={0}
              className="max-h-64 min-h-28 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-black/90 p-3 font-mono text-xs text-white">
              {state.output || "Waiting for native output…"}
            </pre>
            {state.truncated && <p className="text-xs text-text-muted">Showing the latest 64 KiB of native output.</p>}
          </>
        )}
        {pending && nativeLoginLinks(state.output).map((url, index) => (
          <div key={url} className="flex items-center gap-3 text-sm">
            <a href={url} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="text-primary underline">
              Open native HTTPS link {index + 1}
            </a>
            <Button variant="secondary" size="sm" onClick={async () => {
              try { await navigator.clipboard.writeText(url); setCopyError(""); }
              catch { setCopyError("Could not copy the link. Open it above or copy it from the native output."); }
            }}>Copy link</Button>
          </div>
        ))}
        {(state.error || copyError) && <p role="alert" className="break-words text-sm text-red-500">{state.error || copyError}</p>}
        {pending && (
          <form onSubmit={send} className="flex flex-col gap-2">
            <label htmlFor="claude-native-input" className="text-sm">Native login input</label>
            <input ref={inputRef} id="claude-native-input" value={input} onChange={(event) => setInput(event.target.value)}
              className="w-full px-3 py-2 text-sm border border-border rounded-lg bg-background focus:outline-none focus:border-primary"
              autoComplete="off" spellCheck={false} maxLength={8191} disabled={sending} aria-describedby="claude-native-input-help" />
            <p id="claude-native-input-help" className="text-xs text-text-muted">
              Only when the native process asks: enter its choice or paste the browser-returned code. Send also forwards an empty Enter. Do not enter passwords or tokens.
            </p>
            <Button type="submit" disabled={sending}>{sending ? "Sending…" : "Send Enter"}</Button>
          </form>
        )}
        <div className="flex gap-2">
          {!busy && state.status !== "success" && <Button onClick={() => { setInput(""); setCopyError(""); void controller.current?.start(name); }}>
            {state.status === "ready" ? "Start native login" : "Retry native login"}
          </Button>}
          <Button variant="ghost" onClick={close}>{state.status === "success" ? "Done" : busy ? "Cancel login" : "Close"}</Button>
        </div>
      </div>
    </Modal>
  );
}
