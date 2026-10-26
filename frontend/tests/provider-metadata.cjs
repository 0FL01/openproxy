// Optional repository-local drift check; requires web's installed TypeScript.
// This test never runs in the application or contacts a backend.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const root = path.resolve(__dirname, "../..");
const ts = require(path.join(root, "web/node_modules/typescript"));
const source = path.join(root, "web/src/shared/constants/providers.ts");
const js = ts.transpileModule(fs.readFileSync(source, "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
}).outputText;
const reference = {};
vm.runInNewContext(js, { exports: reference, require }, { filename: source });
const snapshot = JSON.parse(fs.readFileSync(path.join(root, "frontend/src/browser/provider_data.json")));
const builtIcons = path.join(root, "frontend/target/dx/openproxy-dashboard/release/web/public/providers");
const groups = { oauth: reference.OAUTH_PROVIDERS, free: reference.FREE_PROVIDERS, freeTier: reference.FREE_TIER_PROVIDERS, apiKey: reference.APIKEY_PROVIDERS };
const normalize = p => ({ id: p.id, name: p.name, color: p.color, textIcon: p.textIcon || "", priority: p.priority ?? 999, noAuth: !!p.noAuth, freeTier: !!p.freeTier, rateLimit: p.rateLimit || "" });
const ids = new Set(["oai-r", "oai-cc", "anthropic-m"]);
for (const [group, providers] of Object.entries(groups)) {
  assert.deepEqual(snapshot[group].map(p => p.id), Object.keys(providers), `${group} registry order`);
  for (const actual of snapshot[group]) {
    const p = providers[actual.id];
    assert.equal(!!p.hidden, false, `${group}/${p.id} needs hidden-filter metadata`);
    assert.equal((p.serviceKinds || ["llm"]).includes("llm"), true, `${group}/${p.id} needs service-kind metadata`);
    const expected = { ...p, freeTier: reference.FREE_TIER_SET.has(p.id), rateLimit: reference.AI_PROVIDERS[p.id]?.freeTierInfo?.rateLimit };
    assert.deepEqual(normalize(actual), normalize(expected), `${group}/${p.id}`);
    ids.add(p.id);
  }
}
assert.equal(Object.keys(reference.WEB_COOKIE_PROVIDERS).length, 0, "cookie providers require list support");
for (const id of ids) {
  const referenceIcon = path.join(root, "web/public/providers", `${id}.png`);
  const candidateIcon = path.join(root, "frontend/public/providers", `${id}.png`);
  assert.equal(fs.existsSync(candidateIcon), fs.existsSync(referenceIcon), `${id} icon presence`);
  if (fs.existsSync(referenceIcon)) assert.deepEqual(fs.readFileSync(candidateIcon), fs.readFileSync(referenceIcon), `${id} icon bytes`);
}
const sourceIcons = path.join(root, "frontend/public/providers");
assert.deepEqual(fs.readdirSync(builtIcons).filter(name => name.endsWith(".png")).sort(), fs.readdirSync(sourceIcons).filter(name => name.endsWith(".png")).sort(), "built PNG set");
for (const name of fs.readdirSync(sourceIcons).filter(name => name.endsWith(".png"))) {
  assert.deepEqual(fs.readFileSync(path.join(builtIcons, name)), fs.readFileSync(path.join(sourceIcons, name)), `built ${name} bytes`);
}
console.log("provider metadata, reference icons, and built icon bytes match");
