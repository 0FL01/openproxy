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
const sites = JSON.parse(fs.readFileSync(path.join(root, "frontend/src/browser/provider_sites.json")));
const aliases = JSON.parse(fs.readFileSync(path.join(root, "frontend/src/browser/provider_aliases.json")));
const detailMetadata = JSON.parse(fs.readFileSync(path.join(root, "frontend/src/browser/provider_detail_metadata.json")));
const authModes = JSON.parse(fs.readFileSync(path.join(root, "frontend/src/browser/provider_auth_modes.json")));
const builtIcons = path.join(root, "frontend/target/dx/openproxy-dashboard/release/web/public/providers");
const groups = { oauth: reference.OAUTH_PROVIDERS, free: reference.FREE_PROVIDERS, freeTier: reference.FREE_TIER_PROVIDERS, apiKey: reference.APIKEY_PROVIDERS };
const normalize = p => ({ id: p.id, name: p.name, color: p.color, textIcon: p.textIcon || "", priority: p.priority ?? 999, noAuth: !!p.noAuth, freeTier: !!p.freeTier, rateLimit: p.rateLimit || "" });
const ids = new Set(["oai-r", "oai-cc", "anthropic-m"]);
const expectedSites = {};
for (const [group, providers] of Object.entries(groups)) {
  assert.deepEqual(snapshot[group].map(p => p.id), Object.keys(providers), `${group} registry order`);
  for (const actual of snapshot[group]) {
    const p = providers[actual.id];
    assert.equal(!!p.hidden, false, `${group}/${p.id} needs hidden-filter metadata`);
    assert.equal((p.serviceKinds || ["llm"]).includes("llm"), true, `${group}/${p.id} needs service-kind metadata`);
    assert.equal(!!p.deprecated, false, `${group}/${p.id} needs deprecated metadata`);
    assert.notEqual(p.authType, "cookie", `${group}/${p.id} needs cookie metadata`);
    const expected = { ...p, freeTier: reference.FREE_TIER_SET.has(p.id), rateLimit: reference.AI_PROVIDERS[p.id]?.freeTierInfo?.rateLimit };
    assert.deepEqual(normalize(actual), normalize(expected), `${group}/${p.id}`);
    if (group === "freeTier" || group === "apiKey") {
      const url = p.notice?.apiKeyUrl || p.website;
      if (url) {
        const site = { url, kind: p.notice?.apiKeyUrl ? "key" : "site" };
        if (expectedSites[p.id]) assert.deepEqual(site, expectedSites[p.id], `${p.id} group-specific site`);
        expectedSites[p.id] = site;
      }
    }
    ids.add(p.id);
  }
}
assert.equal(Object.keys(reference.WEB_COOKIE_PROVIDERS).length, 0, "cookie providers require list support");
assert.deepEqual(sites, expectedSites, "new-provider links");
const expectedAliases = Object.fromEntries(Object.entries(reference.AI_PROVIDERS).filter(([id, provider]) => provider.alias && provider.alias !== id).map(([id, provider]) => [id, provider.alias]));
assert.deepEqual(aliases, expectedAliases, "detail storage aliases");
assert.deepEqual(authModes, JSON.parse(JSON.stringify(Object.fromEntries(Object.entries(reference.AI_PROVIDERS).filter(([, provider]) => provider.authModes).map(([id, provider]) => [id, provider.authModes])))), "detail auth modes");
const expectedDetailMetadata = {};
for (const [id, provider] of Object.entries(reference.AI_PROVIDERS)) {
  const detail = reference.OAUTH_PROVIDERS[id] || reference.APIKEY_PROVIDERS[id] || reference.FREE_PROVIDERS[id] || reference.FREE_TIER_PROVIDERS[id];
  const entry = JSON.parse(JSON.stringify({ freeTierInfo: provider.freeTierInfo, noticeText: detail.notice?.text, noticeUrl: detail.notice?.apiKeyUrl }));
  if (entry.freeTierInfo || entry.noticeText) expectedDetailMetadata[id] = entry;
}
assert.deepEqual(detailMetadata, expectedDetailMetadata, "detail free-tier/notice metadata");
// The changing search count is deliberately excluded from the DOM translator;
// none of today's catalogs translates either of its two literal text runs.
const catalogs = path.join(root, "frontend/public/i18n/literals");
for (const file of fs.readdirSync(catalogs).filter(name => name.endsWith(".json"))) {
  const catalog = JSON.parse(fs.readFileSync(path.join(catalogs, file)));
  assert.equal(Object.hasOwn(catalog, "of") || Object.hasOwn(catalog, "providers"), false, `${file} requires a translated search counter`);
}
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
