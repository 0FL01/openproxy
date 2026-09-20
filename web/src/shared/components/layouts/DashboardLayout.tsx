
import { useState, useEffect } from "react";
import { useNotificationStore } from "@/store/notificationStore";
import Sidebar from "../Sidebar";
import Header from "../Header";
import React from "react";

interface DashboardLayoutProps {
  children: React.ReactNode;
}

interface AuthStatus {
  requireLogin: boolean;
  authenticated: boolean;
}

type AuthState = "checking" | "ready" | "error";

function requestUrl(input: RequestInfo | URL): URL | null {
  try {
    const value = input instanceof Request ? input.url : input.toString();
    return new URL(value, window.location.origin);
  } catch {
    return null;
  }
}

function getToastStyle(type: string) {
  if (type === "success") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-success-text",
      iconColor: "text-success-text",
      titleColor: "text-ink",
      icon: "check_circle",
    };
  }
  if (type === "error") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-brand-coral",
      iconColor: "text-brand-coral",
      titleColor: "text-ink",
      icon: "error",
    };
  }
  if (type === "warning") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-accent-amber",
      iconColor: "text-accent-amber",
      titleColor: "text-ink",
      icon: "warning",
    };
  }
  return {
    wrapper: "bg-surface border border-hairline before:bg-brand-blue-deep",
    iconColor: "text-brand-blue-deep",
    titleColor: "text-ink",
    icon: "info",
  };
}

export default function DashboardLayout({ children }: DashboardLayoutProps) {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [pathname, setPathname] = useState("");
  const [authState, setAuthState] = useState<AuthState>("checking");
  const [authAttempt, setAuthAttempt] = useState(0);

  useEffect(() => {
    setPathname(window.location.pathname);
    const originalFetch = window.fetch;
    const callOriginalFetch = originalFetch.bind(window) as typeof window.fetch;
    let active = true;
    let redirecting = false;
    let statusController: AbortController | null = null;
    let statusCheck: Promise<void> | null = null;

    const checkSession = async (initial: boolean): Promise<void> => {
      statusController = new AbortController();
      try {
        const response = await callOriginalFetch("/api/auth/status", {
          cache: "no-store",
          credentials: "same-origin",
          signal: statusController.signal,
        });
        if (!response.ok) throw new Error(`Server returned ${response.status}`);

        const data = (await response.json()) as Partial<AuthStatus>;
        if (
          typeof data.requireLogin !== "boolean" ||
          typeof data.authenticated !== "boolean"
        ) {
          throw new Error("Invalid auth status response");
        }

        if (data.requireLogin && !data.authenticated) {
          if (!redirecting) {
            redirecting = true;
            window.location.replace("/login");
          }
          return;
        }

        if (active && initial) {
          document.body.classList.add("dashboard-ready");
          setAuthState("ready");
        }
      } catch (error) {
        if (active && initial && !(error instanceof DOMException && error.name === "AbortError")) {
          document.body.classList.add("dashboard-ready");
          setAuthState("error");
        }
      } finally {
        statusController = null;
      }
    };

    const ensureSession = (initial = false): Promise<void> => {
      if (!statusCheck) {
        statusCheck = checkSession(initial).finally(() => {
          statusCheck = null;
        });
      }
      return statusCheck;
    };

    const observedFetch: typeof window.fetch = async (input, init) => {
      const response = await callOriginalFetch(input, init);
      const url = requestUrl(input);
      if (
        active &&
        response.status === 401 &&
        url?.origin === window.location.origin &&
        url.pathname.startsWith("/api/") &&
        url.pathname !== "/api/auth/status"
      ) {
        void ensureSession();
      }
      return response;
    };

    window.fetch = observedFetch;
    void ensureSession(true);

    return () => {
      active = false;
      statusController?.abort();
      if (window.fetch === observedFetch) window.fetch = originalFetch;
      document.body.classList.remove("dashboard-ready");
    };
  }, [authAttempt]);

  const notifications = useNotificationStore((state) => state.notifications);
  const removeNotification = useNotificationStore((state) => state.removeNotification);

  if (authState !== "ready") {
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-canvas px-4">
        {authState === "checking" ? (
          <div className="text-center">
            <div className="inline-block h-8 w-8 animate-spin rounded-full border-2 border-brand-coral border-t-transparent" />
            <p className="mt-4 text-[14px] text-muted">Checking session…</p>
          </div>
        ) : (
          <div className="max-w-sm text-center">
            <span className="material-symbols-outlined text-[32px] text-brand-coral">cloud_off</span>
            <h1 className="mt-3 font-serif text-2xl text-ink">Unable to verify session</h1>
            <p className="mt-2 text-sm text-muted">Check that OpenProxy is running and try again.</p>
            <button
              type="button"
              onClick={() => {
                setAuthState("checking");
                setAuthAttempt((attempt) => attempt + 1);
              }}
              className="mt-5 rounded-mini-md bg-brand-coral px-4 py-2 text-sm font-medium text-white hover:opacity-90"
            >
              Retry
            </button>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="flex h-screen w-full overflow-hidden bg-canvas">
      <div className="fixed top-4 right-4 z-[80] flex w-[min(92vw,380px)] flex-col gap-2">
        {notifications.map((n) => {
          const style = getToastStyle(n.type);
          return (
            <div
              key={n.id}
              className={`relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] ${style.wrapper}`}
            >
              <div className="flex items-start gap-2.5">
                <span className={`material-symbols-outlined text-[20px] leading-5 ${style.iconColor}`}>{style.icon}</span>
                <div className="min-w-0 flex-1">
                  {n.title ? <p className={`text-[13px] font-semibold mb-0.5 ${style.titleColor}`}>{n.title}</p> : null}
                  <p className="text-[12px] leading-snug text-text-muted whitespace-pre-wrap break-words">{n.message}</p>
                </div>
                {n.dismissible ? (
                  <button
                    type="button"
                    onClick={() => removeNotification(n.id)}
                    className="text-text-muted hover:text-ink transition-colors"
                    aria-label="Dismiss notification"
                  >
                    <span className="material-symbols-outlined text-[16px]">close</span>
                  </button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
      {/* Mobile sidebar overlay */}
      {sidebarOpen && (
        <div
          className="fixed inset-0 z-40 bg-black/20 lg:hidden"
          onClick={() => setSidebarOpen(false)}
        />
      )}

      {/* Sidebar - Desktop */}
      <div className="hidden lg:contents">
        <Sidebar />
      </div>

      {/* Sidebar - Mobile */}
      <div
        className={`fixed inset-y-0 left-0 z-50 transform lg:hidden transition-transform duration-300 ease-in-out ${
          sidebarOpen ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <Sidebar onClose={() => setSidebarOpen(false)} />
      </div>

      {/* Main content */}
      <main className="flex flex-col flex-1 h-full min-w-0 relative transition-colors duration-300 isolate">
        {/* Faint grid background */}
        <div className="landing-grid absolute inset-0 pointer-events-none -z-10" aria-hidden="true" />
        <Header key={pathname} onMenuClick={() => setSidebarOpen(true)} />
        <div className="flex-1 overflow-y-auto custom-scrollbar p-6 lg:p-10">
          <div className="max-w-7xl mx-auto">{children}</div>
        </div>
      </main>
    </div>
  );
}
