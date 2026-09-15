import { getMitmConfig, startMitm } from "@/shared/utils/backendApi";

/**
 * Browser-side app bootstrap for the Astro + Rust dashboard.
 *
 * 9router ran this as a Next.js server singleton (watchdog, network monitor,
 * quota auto-ping scheduler). OpenProxy owns process supervision in Rust, so
 * this client path only:
 *   1. Resumes MITM when settings say it should be on
 *
 * Full long-running watchdog + auto-ping scheduler belong in the Rust server.
 * Do not reintroduce Node globals.
 */

const STARTUP_DEFER_MS = 1500;
interface AppSingleton {
  initialized: boolean;
  mitmStartInProgress: boolean;
}

function getSingleton(): AppSingleton {
  const g = globalThis as typeof globalThis & { __opAppSingleton?: AppSingleton };
  if (!g.__opAppSingleton) {
    g.__opAppSingleton = {
      initialized: false,
      mitmStartInProgress: false,
    };
  }
  return g.__opAppSingleton;
}

export async function initializeApp(): Promise<void> {
  // SSR / non-browser — nothing to do (Astro may evaluate modules at build).
  if (typeof window === "undefined") return;

  const g = getSingleton();
  if (g.initialized) return;
  g.initialized = true;

  // Defer heavy resume so the first paint / auth cookie settle first.
  window.setTimeout(() => {
    runClientStartup().catch((e) =>
      console.error("[InitApp] deferred startup failed:", (e as Error).message),
    );
  }, STARTUP_DEFER_MS);
}

async function runClientStartup(): Promise<void> {
  try {
    autoStartMitm().catch((e) =>
      console.log("[InitApp] MITM auto-start failed:", (e as Error).message),
    );
  } catch (error) {
    console.error("[InitApp] Error:", error);
  }
}

async function autoStartMitm(): Promise<void> {
  const g = getSingleton();
  if (g.mitmStartInProgress) return;
  g.mitmStartInProgress = true;
  try {
    const mitmConfig = await getMitmConfig();
    // OpenProxy: `enabled` means routes are configured (mitm_alias non-empty).
    // There is no separate settings.mitmEnabled flag; if routes exist, try start
    // (start is idempotent when already running).
    if (!mitmConfig.enabled) return;

    console.log("[InitApp] MITM routes configured, ensuring proxy is running...");
    await startMitm();
    console.log("[InitApp] MITM auto-start requested");
  } catch (err) {
    // MITM start may require local API-key / loopback privileges — best effort.
    console.log("[InitApp] MITM auto-start failed:", (err as Error).message);
  } finally {
    g.mitmStartInProgress = false;
  }
}

export default initializeApp;
