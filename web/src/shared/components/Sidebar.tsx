
import { useState, useEffect } from "react";
import { cn } from "@/shared/utils/cn";
import { APP_CONFIG } from "@/shared/constants/config";
import Button from "./Button";
import AnthropicSpike from "./AnthropicSpike";
import { ConfirmModal } from "./Modal";
import React from "react";

/**
 * Shared style strings for sidebar nav items. The active treatment uses a
 * coral left-rail marker against a cream `surface-card` pill — the
 * Claude editorial signal of the current section without painting the
 * full row coral.
 */
const NAV_ITEM_BASE =
  "relative flex items-center gap-3 pl-4 pr-3 py-1.5 rounded-mini-md transition-colors group";
const NAV_ITEM_ACTIVE =
  "bg-surface-card text-ink font-medium before:content-[''] before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-[3px] before:rounded-r-full before:bg-brand-coral";
const NAV_ITEM_INACTIVE = "text-body hover:bg-surface-soft hover:text-ink";

interface NavItem {
  href: string;
  label: string;
  icon: string;
}

const navItems: NavItem[] = [
  { href: "/dashboard/endpoint", label: "Endpoint", icon: "api" },
  { href: "/dashboard/providers", label: "Providers", icon: "dns" },
  { href: "/dashboard/db-backups", label: "DB Backups", icon: "backup" },
  { href: "/dashboard/logs", label: "Application Logs", icon: "receipt_long" },
  { href: "/dashboard/quota", label: "Quota Tracker", icon: "data_usage" },
];

const debugItems: NavItem[] = [
  { href: "/dashboard/console-log", label: "Console Log", icon: "terminal" },
  { href: "/dashboard/translator", label: "Translator", icon: "translate" },
];

const systemItems: NavItem[] = [
  { href: "/dashboard/proxy-pools", label: "Proxy Pools", icon: "lan" },
  { href: "/dashboard/skills", label: "Skills", icon: "extension" },
];

const footerItems: NavItem[] = [
  { href: "/dashboard/profile", label: "Settings", icon: "settings" },
];

interface SidebarProps {
  onClose?: () => void;
}

export default function Sidebar({ onClose }: SidebarProps) {
  const [pathname, setPathname] = useState("");
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    setMounted(true);
    setPathname(window.location.pathname);
  }, []);

  const [showShutdownModal, setShowShutdownModal] = useState(false);
  const [isShuttingDown, setIsShuttingDown] = useState(false);
  const [isDisconnected, setIsDisconnected] = useState(false);
  const [enableTranslator, setEnableTranslator] = useState(false);

  useEffect(() => {
    fetch("/api/settings")
      .then(res => res.json())
      .then(data => { if (data.enableTranslator) setEnableTranslator(true); })
      .catch(() => {});
  }, []);

  const isActive = (href: string) => {
    if (href === "/dashboard/endpoint") {
      return pathname === "/dashboard" || pathname.startsWith("/dashboard/endpoint");
    }
    return pathname.startsWith(href);
  };

  const handleShutdown = async () => {
    setIsShuttingDown(true);
    try {
      await fetch("/api/shutdown", { method: "POST" });
    } catch (e) {
      // Expected to fail as server shuts down; ignore error
    }
    setIsShuttingDown(false);
    setShowShutdownModal(false);
    setIsDisconnected(true);
  };

  return (
    <>
      <aside className="flex w-72 flex-col border-r border-hairline-soft bg-canvas transition-colors duration-300 min-h-full">
        {/* Editorial wordmark — spike-mark glyph + serif headline */}
        <div className="px-6 pt-6 pb-4 flex flex-col gap-2">
          <a href="/dashboard" className="flex items-center gap-3 group">
            <div className="flex items-center justify-center size-9 rounded-mini-md bg-surface-card border border-hairline">
              <AnthropicSpike size={20} className="text-brand-coral" ariaLabel="OpenProxy mark" />
            </div>
            <div className="flex flex-col leading-tight">
              <h1 className="font-serif text-[22px] font-normal tracking-[-0.02em] text-ink">
                {APP_CONFIG.name}
              </h1>
              <span className="text-[11px] text-muted-soft tracking-wide">
                v{APP_CONFIG.version}
              </span>
            </div>
          </a>
        </div>

        {/* Navigation */}
        <nav className="flex-1 px-4 py-2 space-y-0.5 overflow-y-auto custom-scrollbar">
          {navItems.map((item) => (
            <a
              key={item.href}
              href={item.href}
              onClick={onClose}
              className={cn(
                NAV_ITEM_BASE,
                isActive(item.href) ? NAV_ITEM_ACTIVE : NAV_ITEM_INACTIVE,
              )}
            >
              <span
                className={cn(
                  "material-symbols-outlined text-[18px]",
                  isActive(item.href) ? "fill-1" : ""
                )}
              >
                {item.icon}
              </span>
              <span className="text-[13px]">{item.label}</span>
            </a>
          ))}

          {/* System section */}
          <div className="pt-4 mt-2 space-y-0.5">
            <p className="px-4 type-caption-uppercase text-muted-soft mb-2">
              System
            </p>

            {systemItems.map((item) => (
              <a
                key={item.href}
                href={item.href}
                onClick={onClose}
                className={cn(
                  NAV_ITEM_BASE,
                  isActive(item.href) ? NAV_ITEM_ACTIVE : NAV_ITEM_INACTIVE,
                )}
              >
                <span
                  className={cn(
                    "material-symbols-outlined text-[18px]",
                    isActive(item.href) ? "fill-1" : ""
                  )}
                >
                  {item.icon}
                </span>
                <span className="text-[13px]">{item.label}</span>
              </a>
            ))}

            {/* Debug items (inside System section, before Profile) */}
            {debugItems.map((item) => {
              const show = item.href !== "/dashboard/translator" || enableTranslator;
              return show ? (
                <a
                  key={item.href}
                  href={item.href}
                  onClick={onClose}
                  className={cn(
                    NAV_ITEM_BASE,
                    isActive(item.href) ? NAV_ITEM_ACTIVE : NAV_ITEM_INACTIVE,
                  )}
                >
                  <span
                    className={cn(
                      "material-symbols-outlined text-[18px]",
                      isActive(item.href) ? "fill-1" : ""
                    )}
                  >
                    {item.icon}
                  </span>
                  <span className="text-[13px]">{item.label}</span>
                </a>
              ) : null;
            })}

            {/* Settings is already in footerItems — avoid duplicate Profile entry (9router parity). */}
          </div>
        </nav>

        {/* Footer section */}
        <div className="p-3 border-t border-hairline-soft space-y-1">
          {footerItems.map((item) => (
            <a
              key={item.href}
              href={item.href}
              onClick={onClose}
              className={cn(
                NAV_ITEM_BASE,
                isActive(item.href) ? NAV_ITEM_ACTIVE : NAV_ITEM_INACTIVE,
              )}
            >
              <span
                className={cn(
                  "material-symbols-outlined text-[18px]",
                  isActive(item.href) ? "fill-1" : ""
                )}
              >
                {item.icon}
              </span>
              <span className="text-[13px]">{item.label}</span>
            </a>
          ))}
          {/* Shutdown button */}
          <Button
            variant="secondary"
            fullWidth
            icon="power_settings_new"
            onClick={() => setShowShutdownModal(true)}
            className="text-[color:var(--color-danger)] border-[color:var(--color-danger)]/30 hover:bg-[color:var(--color-danger)]/10 hover:border-[color:var(--color-danger)]/50"
          >
            Shutdown
          </Button>
        </div>
      </aside>

      {/* Shutdown Confirmation Modal */}
      <ConfirmModal
        isOpen={showShutdownModal}
        onClose={() => setShowShutdownModal(false)}
        onConfirm={handleShutdown}
        title="Close Proxy"
        message="Are you sure you want to close the proxy server?"
        confirmText="Close"
        cancelText="Cancel"
        variant="danger"
        loading={isShuttingDown}
      />

      {/* Disconnected Overlay */}
      {isDisconnected && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm p-6">
          <div className="text-center p-8">
            <div className="flex items-center justify-center size-16 rounded-full bg-red-500/20 text-red-500 mx-auto mb-4">
              <span className="material-symbols-outlined text-[32px]">power_off</span>
            </div>
            <h2 className="text-xl font-semibold text-white mb-2">Server Disconnected</h2>
            <p className="text-text-muted mb-6">The proxy server has been stopped.</p>
            <Button variant="secondary" onClick={() => globalThis.location.reload()}>
              Reload Page
            </Button>
          </div>
        </div>
      )}
    </>
  );
}
