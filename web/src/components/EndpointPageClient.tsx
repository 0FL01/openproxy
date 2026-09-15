"use client";

import { useState, useEffect } from "react";
import type { ChangeEvent, ReactNode, MouseEvent } from "react";
import { Card, Button, Input, Modal, CardSkeleton, Toggle } from "@/shared/components";
import { ConfirmModal } from "@/shared/components/Modal";
import { useNotificationStore } from "@/store/notificationStore";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import CacheStatsCard from "@/components/CacheStatsCard";

interface ApiKey {
  id: string;
  name: string;
  key: string;
  createdAt: string;
  isActive?: boolean;
  monthlyBudgetUsd?: number | null;
}

interface EndpointRowProps {
  label: string;
  url: string;
  copyId: string;
  copied: string | null;
  onCopy: (url: string, id: string) => void;
  badge?: string;
  actions?: ReactNode;
}

interface SecurityAction {
  label: string;
  href: string;
}

interface SecurityWarningProps {
  message: string;
  action?: SecurityAction;
}

interface APIPageClientProps {
  machineId: string;
}

export default function APIPageClient({ machineId }: APIPageClientProps) {
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [showAddModal, setShowAddModal] = useState<boolean>(false);
  const [newKeyName, setNewKeyName] = useState<string>("");
  const [newKeyBudget, setNewKeyBudget] = useState<string>("");
  const [createdKey, setCreatedKey] = useState<string | null>(null);

  const [requireApiKey, setRequireApiKey] = useState<boolean>(true);
  const [requireLogin, setRequireLogin] = useState<boolean>(true);
  const [hasPassword, setHasPassword] = useState<boolean>(true);
  // True when the dashboard is opened via a non-loopback host (LAN).
  const [isRemoteHost, setIsRemoteHost] = useState<boolean>(false);

  // API key visibility toggle state
  const [visibleKeys, setVisibleKeys] = useState<Set<string>>(new Set());

  // Delete / pause confirmation targets (ConfirmModal replaces the old
  // browser confirm() and matches the rest of the dashboard chrome).
  const [deleteKeyTarget, setDeleteKeyTarget] = useState<ApiKey | null>(null);
  const [pauseKeyTarget, setPauseKeyTarget] = useState<ApiKey | null>(null);

  const notify = useNotificationStore();
  const { copied, copy } = useCopyToClipboard();

  useEffect(() => {
    fetchData();
    loadSettings();
  }, []);

  // Detect non-loopback access so we can warn when API key is not required.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const host = (window.location.hostname || "").toLowerCase();
    const loopback =
      host === "localhost" ||
      host === "127.0.0.1" ||
      host === "[::1]" ||
      host === "::1" ||
      host.endsWith(".localhost");
    setIsRemoteHost(!loopback);
  }, []);

  const loadSettings = async (): Promise<void> => {
    try {
      const settingsRes = await fetch("/api/settings");
      if (settingsRes.ok) {
        const data = await settingsRes.json();
        setRequireApiKey(data.requireApiKey !== false);
        setRequireLogin(data.requireLogin !== false);
        setHasPassword(data.hasPassword || false);
      }
    } catch (error) {
      console.log("Error loading settings:", error);
    }
  };

  const handleRequireApiKey = async (value: boolean): Promise<void> => {
    try {
      const res = await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ requireApiKey: value }),
      });
      if (!res.ok) throw new Error("Failed to save API key requirement");
      setRequireApiKey(value);
    } catch (error) {
      console.log("Error updating requireApiKey:", error);
      notify.error(error instanceof Error ? error.message : "Failed to save API key requirement");
    }
  };

  const fetchData = async (): Promise<void> => {
    try {
      const keysRes = await fetch("/api/keys");
      const keysData = await keysRes.json();
      if (keysRes.ok) {
        let keys = keysData.keys || [];
        // 9router parity: auto-provision a default key for first-time users so
        // the endpoint works out of the box.
        if (keys.length === 0) {
          try {
            const createRes = await fetch("/api/keys", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ name: "Default Key" }),
            });
            if (createRes.ok) {
              const refetch = await fetch("/api/keys");
              const refetchData = await refetch.json();
              if (refetch.ok) keys = refetchData.keys || [];
            }
          } catch {
            /* fall through to empty render */
          }
        }
        setKeys(keys);
      }
    } catch (error) {
      console.log("Error fetching data:", error);
    } finally {
      setLoading(false);
    }
  };

  const isLoginUnsafe = !requireLogin || !hasPassword;

  const handleCreateKey = async (): Promise<void> => {
    if (!newKeyName.trim()) return;

    const budget = parseFloat(newKeyBudget);
    const body: Record<string, unknown> = { name: newKeyName };
    if (!Number.isNaN(budget) && budget > 0) {
      body.monthlyBudgetUsd = budget;
    }

    try {
      const res = await fetch("/api/keys", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const data = await res.json();

      if (res.ok) {
        setCreatedKey(data.key);
        await fetchData();
        setNewKeyName("");
        setNewKeyBudget("");
        setShowAddModal(false);
      }
    } catch (error) {
      console.log("Error creating key:", error);
    }
  };

  const deleteKey = async (id: string): Promise<void> => {
    try {
      const res = await fetch(`/api/keys/${id}`, { method: "DELETE" });
      if (res.ok) {
        setKeys((prev) => prev.filter((k) => k.id !== id));
        // Clean up visibility state
        setVisibleKeys((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
        notify.success("API key deleted");
      } else {
        notify.error("Failed to delete API key");
      }
    } catch (error) {
      console.log("Error deleting key:", error);
      notify.error("Failed to delete API key");
    }
  };

  const handleToggleKey = async (id: string, isActive: boolean): Promise<void> => {
    try {
      const res = await fetch(`/api/keys/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ isActive }),
      });
      if (res.ok) {
        setKeys(prev => prev.map(k => k.id === id ? { ...k, isActive } : k));
      }
    } catch (error) {
      console.log("Error toggling key:", error);
    }
  };

  const maskKey = (fullKey: string): string => {
    if (!fullKey) return "";
    return fullKey.length > 8 ? fullKey.slice(0, 8) + "..." : fullKey;
  };

  const toggleKeyVisibility = (keyId: string): void => {
    setVisibleKeys(prev => {
      const next = new Set(prev);
      if (next.has(keyId)) next.delete(keyId);
      else next.add(keyId);
      return next;
    });
  };

  const [baseUrl, setBaseUrl] = useState<string>("/v1");

  // Hydration fix: Only access window on client side
  useEffect(() => {
    if (typeof window !== "undefined") {
      setBaseUrl(`${window.location.origin}/v1`);
    }
  }, []);

  if (loading) {
    return (
      <div className="flex flex-col gap-8">
        <CardSkeleton />
        <CardSkeleton />
      </div>
    );
  }

  const currentEndpoint = baseUrl;

  return (
    <div className="flex flex-col gap-8">
      {/* Endpoint Card */}
      <Card>
        <h2 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <span className="material-symbols-outlined text-primary">api</span>
          API Endpoint
        </h2>

        {/* Endpoint rows */}
        <div className="flex flex-col gap-2">
          {/* Local */}
          <EndpointRow
            label="Local"
            url={currentEndpoint}
            copyId="local_url"
            copied={copied}
            onCopy={copy}
          />
        </div>

        {/* Security warnings: missing login/password, exposed endpoint without key */}
        {(isLoginUnsafe || !requireApiKey) && (
          <div className="mt-4 flex flex-col gap-2">
            {isLoginUnsafe && (
              <SecurityWarning
                message={
                  !requireLogin
                    ? "Require login is disabled — enable it and set a password to protect your dashboard."
                    : "Dashboard password is not set — set a strong password in Settings."
                }
                action={{ label: "Open Settings", href: "/dashboard/profile" }}
              />
            )}
            {isRemoteHost && !requireApiKey && (
              <SecurityWarning
                message="Endpoint is exposed without an API key."
                action={{ label: "Enable", href: "#require-api-key" }}
              />
            )}
            {!requireApiKey && !isRemoteHost && (
              <SecurityWarning
                message="Require API key is disabled — your endpoint is accessible without authentication."
                action={{ label: "Enable", href: "#require-api-key" }}
              />
            )}
          </div>
        )}
      </Card>

      {/* Response Cache hit-rate */}
      <CacheStatsCard />

      {/* API Keys */}
      <Card id="require-api-key">
        <div className="flex items-center justify-between mb-4">
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <span className="material-symbols-outlined text-primary">vpn_key</span>
            API Keys
          </h2>
          <Button icon="add" onClick={() => setShowAddModal(true)}>
            Create Key
          </Button>
        </div>

        <div className="flex items-center justify-between pb-4 mb-4 border-b border-border">
          <div>
            <p className="font-medium">Require API key</p>
            <p className="text-sm text-text-muted">
              Requests without a valid key will be rejected
            </p>
          </div>
          <Toggle
            checked={requireApiKey}
            onChange={() => handleRequireApiKey(!requireApiKey)}
          />
        </div>

        {keys.length === 0 ? (
          <div className="text-center py-12">
            <div className="inline-flex items-center justify-center w-16 h-16 rounded-full bg-primary/10 text-primary mb-4">
              <span className="material-symbols-outlined text-[32px]">vpn_key</span>
            </div>
            <p className="text-text-main font-medium mb-1">No API keys yet</p>
            <p className="text-sm text-text-muted mb-4">Create your first API key to get started</p>
            <Button icon="add" onClick={() => setShowAddModal(true)}>
              Create Key
            </Button>
          </div>
        ) : (
          <div className="flex flex-col">
            {keys.map((key) => (
              <div
                key={key.id}
                className={`group flex items-center justify-between py-3 border-b border-black/[0.03] dark:border-white/[0.03] last:border-b-0 ${key.isActive === false ? "opacity-60" : ""}`}
              >
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium">{key.name}</p>
                  <div className="flex items-center gap-2 mt-1">
                    <code className="text-xs text-text-muted font-mono">
                      {visibleKeys.has(key.id) ? key.key : maskKey(key.key)}
                    </code>
                    <button
                      onClick={() => toggleKeyVisibility(key.id)}
                      className="p-1 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary opacity-0 group-hover:opacity-100 transition-all"
                      title={visibleKeys.has(key.id) ? "Hide key" : "Show key"}
                    >
                      <span className="material-symbols-outlined text-[14px]">
                        {visibleKeys.has(key.id) ? "visibility_off" : "visibility"}
                      </span>
                    </button>
                    <button
                      onClick={() => copy(key.key, key.id)}
                      className="p-1 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary opacity-0 group-hover:opacity-100 transition-all"
                    >
                      <span className="material-symbols-outlined text-[14px]">
                        {copied === key.id ? "check" : "content_copy"}
                      </span>
                    </button>
                  </div>
                  <p className="text-xs text-text-muted mt-1">
                    Created {new Date(key.createdAt).toLocaleDateString()}
                    {typeof key.monthlyBudgetUsd === "number" && (
                      <> · Budget ${key.monthlyBudgetUsd.toFixed(2)}/mo</>
                    )}
                  </p>
                  {key.isActive === false && (
                    <p className="text-xs text-orange-500 mt-1">Paused</p>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <Toggle
                    size="sm"
                    checked={key.isActive ?? true}
                    onChange={(checked: boolean) => {
                      if (key.isActive && !checked) {
                        setPauseKeyTarget(key);
                      } else {
                        handleToggleKey(key.id, checked);
                      }
                    }}
                    title={key.isActive ? "Pause key" : "Resume key"}
                  />
                  <button
                    onClick={() => setDeleteKeyTarget(key)}
                    className="p-2 hover:bg-red-500/10 rounded text-red-500 opacity-0 group-hover:opacity-100 transition-all"
                  >
                    <span className="material-symbols-outlined text-[18px]">delete</span>
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>

      {/* Add Key Modal */}
      <Modal
        isOpen={showAddModal}
        title="Create API Key"
        onClose={() => {
          setShowAddModal(false);
          setNewKeyName("");
          setNewKeyBudget("");
        }}
      >
        <div className="flex flex-col gap-4">
          <Input
            label="Key Name"
            value={newKeyName}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setNewKeyName(e.target.value)}
            placeholder="Production Key"
          />
          <Input
            label="Monthly Budget (USD, optional)"
            type="number"
            min="0"
            step="0.01"
            value={newKeyBudget}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setNewKeyBudget(e.target.value)}
            placeholder="e.g. 10.00"
          />
          <div className="flex gap-2">
            <Button onClick={handleCreateKey} fullWidth disabled={!newKeyName.trim()}>
              Create
            </Button>
            <Button
              onClick={() => {
                setShowAddModal(false);
                setNewKeyName("");
              }}
              variant="ghost"
              fullWidth
            >
              Cancel
            </Button>
          </div>
        </div>
      </Modal>

      {/* Created Key Modal */}
      <Modal
        isOpen={!!createdKey}
        title="API Key Created"
        onClose={() => setCreatedKey(null)}
      >
        <div className="flex flex-col gap-4">
          <div className="bg-yellow-50 dark:bg-yellow-900/20 border border-yellow-200 dark:border-yellow-800 rounded-lg p-4">
            <p className="text-sm text-yellow-800 dark:text-yellow-200 mb-2 font-medium">
              Save this key now!
            </p>
            <p className="text-sm text-yellow-700 dark:text-yellow-300">
              This is the only time you will see this key. Store it securely.
            </p>
          </div>
          <div className="flex gap-2">
            <Input
              value={createdKey || ""}
              readOnly
              className="flex-1 font-mono text-sm"
            />
            <Button
              variant="secondary"
              icon={copied === "created_key" ? "check" : "content_copy"}
              onClick={() => copy(createdKey || "", "created_key")}
            >
              {copied === "created_key" ? "Copied!" : "Copy"}
            </Button>
          </div>
          <Button onClick={() => setCreatedKey(null)} fullWidth>
            Done
          </Button>
        </div>
      </Modal>

      <ConfirmModal
        isOpen={!!deleteKeyTarget}
        onClose={() => setDeleteKeyTarget(null)}
        onConfirm={async () => {
          if (!deleteKeyTarget) return;
          await deleteKey(deleteKeyTarget.id);
          setDeleteKeyTarget(null);
        }}
        title="Delete API key"
        message={deleteKeyTarget ? <>Delete API key <code>{deleteKeyTarget.name}</code>? This cannot be undone.</> : null}
        confirmText="Delete"
        variant="danger"
      />

      <ConfirmModal
        isOpen={!!pauseKeyTarget}
        onClose={() => setPauseKeyTarget(null)}
        onConfirm={async () => {
          if (!pauseKeyTarget) return;
          await handleToggleKey(pauseKeyTarget.id, false);
          setPauseKeyTarget(null);
        }}
        title="Pause API key"
        message={pauseKeyTarget ? <>Pause API key <code>{pauseKeyTarget.name}</code>? This key will stop working immediately but can be resumed later.</> : null}
        confirmText="Pause"
        variant="danger"
      />
    </div>
  );
}

/** Reusable endpoint row component */
function EndpointRow({ label, url, copyId, copied, onCopy, badge, actions }: EndpointRowProps): ReactNode {
  return (
    <div className="flex items-center gap-2">
      <span className={`text-xs font-mono px-1.5 py-0.5 rounded shrink-0 min-w-[88px] text-center ${
          (badge === "CF" || badge === "TS") ? "bg-primary/10 text-primary" : "bg-surface-2 text-text-muted"
        }`}>{label}</span>
      <Input value={url} readOnly className="flex-1 font-mono text-sm" />
      <button
        onClick={() => onCopy(url, copyId)}
        className="p-2 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary transition-colors shrink-0"
      >
        <span className="material-symbols-outlined text-[18px]">{copied === copyId ? "check" : "content_copy"}</span>
      </button>
      {actions}
    </div>
  );
}

/** Security warning banner with optional action link */
function SecurityWarning({ message, action }: SecurityWarningProps): ReactNode {
  return (
    <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20 text-amber-700 dark:text-amber-400">
      <span className="material-symbols-outlined text-[16px] shrink-0 mt-0.5">warning</span>
      <p className="text-xs flex-1">{message}</p>
      {action && (
        <a
          href={action.href}
          className="text-xs font-medium underline shrink-0 hover:opacity-80"
          onClick={action.href.startsWith("#") ? (e: MouseEvent<HTMLAnchorElement>) => {
            e.preventDefault();
            document.getElementById(action.href.slice(1))?.scrollIntoView({ behavior: "smooth" });
          } : undefined}
        >
          {action.label}
        </a>
      )}
    </div>
  );
}
