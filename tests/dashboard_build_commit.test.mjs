import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "../web/node_modules/typescript/lib/typescript.js";

const source = await readFile(new URL("../web/src/shared/utils/buildCommit.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
});
const { loadBuildCommit } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("dashboard reads the running backend commit without caching", async t => {
  const signal = new AbortController().signal;
  const commit = "ABCDEF12" + "0".repeat(32);
  t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.equal(url, "/api/build");
    assert.equal(options.signal, signal);
    assert.equal(options.cache, "no-store");
    return Response.json({ commit });
  });
  assert.equal(await loadBuildCommit(signal), commit.toLowerCase());
  t.mock.restoreAll();
  t.mock.method(globalThis, "fetch", async () => Response.json({ commit: "f".repeat(64) }));
  assert.equal(await loadBuildCommit(signal), "f".repeat(64));
});

test("unknown, malformed and failed metadata never becomes a fake version", async t => {
  const signal = new AbortController().signal;
  for (const data of [null, {}, { commit: null }, { commit: 123 }, { commit: "0.3.0" }, { commit: "35a88dc3" }, { commit: "z".repeat(40) }]) {
    t.mock.method(globalThis, "fetch", async () => Response.json(data));
    assert.equal(await loadBuildCommit(signal), null);
    t.mock.restoreAll();
  }
  t.mock.method(globalThis, "fetch", async () => new Response("unauthorized", { status: 401 }));
  assert.equal(await loadBuildCommit(signal), null);
  t.mock.restoreAll();
  t.mock.method(globalThis, "fetch", async () => { throw new Error("offline or aborted"); });
  assert.equal(await loadBuildCommit(signal), null);
});

test("wordmark uses eight bare characters, not the package version", async () => {
  const sidebar = await readFile(new URL("../web/src/shared/components/Sidebar.tsx", import.meta.url), "utf8");
  const config = await readFile(new URL("../web/src/shared/constants/config.ts", import.meta.url), "utf8");
  assert.ok(sidebar.includes('buildCommit.slice(0, 8)'));
  assert.ok(sidebar.includes('title={buildCommit ? `Commit: ${buildCommit}`'));
  assert.ok(sidebar.includes('href="/dashboard"'));
  assert.ok(sidebar.includes('controller.abort()'));
  assert.ok(!sidebar.includes("APP_CONFIG.version"));
  assert.ok(!config.includes("package.json"));
  assert.ok(!config.includes("version:"));
});
