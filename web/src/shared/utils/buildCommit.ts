/** Read the running backend's identity, never the dashboard package version. */
export async function loadBuildCommit(signal: AbortSignal): Promise<string | null> {
  try {
    const response = await fetch("/api/build", { signal, cache: "no-store" });
    if (!response.ok) return null;
    const data: unknown = await response.json();
    if (!data || typeof data !== "object" || !("commit" in data)) return null;
    const commit = data.commit;
    return typeof commit === "string" && /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i.test(commit)
      ? commit.toLowerCase()
      : null;
  } catch {
    // Missing metadata, auth failure, offline backend and unmount are not versions.
    return null;
  }
}
