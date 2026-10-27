// Synthetic same-origin fixture only; no OpenProxy instance or provider traffic.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(process.env.TRANSLATOR_PUBLIC_DIR || path.join(__dirname, "../target/dx/openproxy-dashboard/release/web/public"));
const reference = process.env.TRANSLATOR_REFERENCE === "1";
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".ttf": "font/ttf", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const requests = [];
let releaseStream;
const source = { model: "fixture/model", messages: [{ role: "user", content: "Hello" }] };

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname.startsWith("/api/")) {
    let raw = ""; for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    requests.push({ method: req.method, path: url.pathname, query: url.search, body });
    const reply = (status, value) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(value)); };
    if (url.pathname === "/api/auth/status") return reply(200, { requireLogin: true, authenticated: true });
    if (url.pathname === "/api/settings") return reply(200, { requireLogin: true, hasPassword: true, enableTranslator: true });
    if (url.pathname === "/api/build") return reply(200, { commit: "synthetic" });
    if (url.pathname === "/api/translator/load") return reply(200, { success: true, content: JSON.stringify(source) });
    if (url.pathname === "/api/translator/save") return reply(200, { success: true });
    if (url.pathname === "/api/translator/translate") {
      const result = body.step === 1 ? { provider: "fixture", model: "model", sourceFormat: "openai", targetFormat: "claude" }
        : body.step === 2 ? { body: { messages: source.messages } }
        : { url: "https://fixture.invalid", headers: {}, body: { prompt: "Hello" } };
      return reply(200, { success: true, result });
    }
    if (url.pathname === "/api/translator/send") {
      res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
      res.write(Buffer.from([0x6f, 0x6b, 0x20, 0xe2]));
      releaseStream = () => res.end(Buffer.from([0x82, 0xac, 0x20, 0x64, 0x6f, 0x6e, 0x65]));
      return;
    }
    return reply(404, {});
  }
  const relative = url.pathname === "/dashboard/translator" ? (reference ? "dashboard/translator.html" : "index.html") : url.pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) return res.writeHead(403).end();
  try { const content = await fs.readFile(file); res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" }); res.end(content); }
  catch { res.writeHead(404).end(); }
});

async function waitFor(predicate) {
  const deadline = Date.now() + 5000;
  while (!predicate()) { if (Date.now() > deadline) throw new Error("synthetic translator action timed out"); await new Promise(resolve => setTimeout(resolve, 10)); }
}

async function main() {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({ executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true });
  try {
    const context = await browser.newContext();
    await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin });
    const page = await context.newPage();
    const errors = []; page.on("pageerror", error => errors.push(error.stack || String(error)));
    await context.route("**/*", async route => {
      const url = new URL(route.request().url());
      const prefix = "/npm/monaco-editor@0.55.1/min/vs/";
      if (reference && url.origin === "https://cdn.jsdelivr.net" && url.pathname.startsWith(prefix) && !url.search) {
        const root = path.resolve(process.env.TRANSLATOR_MONACO_DIR);
        const file = path.resolve(root, url.pathname.slice(prefix.length));
        if (!file.startsWith(root + path.sep)) return route.abort();
        try { return route.fulfill({ status: 200, contentType: mime[path.extname(file)] || "application/octet-stream", body: await fs.readFile(file) }); }
        catch { return route.abort(); }
      }
      return url.origin === origin ? route.continue() : route.abort();
    });
    await page.goto(`${origin}/dashboard/translator`);
    await page.getByRole("heading", { name: "Translator Debug" }).waitFor();
    const editor = () => page.getByLabel("Translator editor");
    const settleReferenceJson = () => page.waitForFunction(async () => {
      try {
        const model = window.monaco?.editor.getEditors()[0]?.getModel();
        if (!model || !window.monaco?.languages.json?.getWorker) return false;
        const worker = await (await window.monaco.languages.json.getWorker())(model.uri);
        await worker.doValidation(model.uri.toString());
        return true;
      } catch { return false; }
    });
    if (!reference) {
      await page.getByRole("button", { name: /Load$/ }).click();
      await page.waitForFunction(value => document.querySelector('textarea[aria-label="Translator editor"]')?.value === value, JSON.stringify(source));
      await page.getByText("provider:", { exact: false }).waitFor();
      await page.getByRole("button", { name: /Format$/ }).click();
      assert.equal(await editor().inputValue(), JSON.stringify(source, null, 2));
      await page.getByRole("button", { name: /Copy$/ }).click();
      await page.waitForFunction(async value => await navigator.clipboard.readText() === value, JSON.stringify(source, null, 2));

      await page.getByRole("button", { name: /OpenAI$/ }).click();
      await page.waitForFunction(() => document.querySelector('textarea[aria-label="Translator editor"]')?.value.includes('"messages"'));
      await page.getByRole("button", { name: /Target$/ }).last().click();
      await page.waitForFunction(() => document.querySelector('textarea[aria-label="Translator editor"]')?.value.includes('"prompt"'));
      await page.getByRole("button", { name: /Send$/ }).last().click();
      await waitFor(() => typeof releaseStream === "function");
      assert.match(await editor().inputValue(), /"prompt": "Hello"/, "target request remains visible until the response stream finishes");
      assert.equal(requests.some(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt"), false, "stream is not saved before it finishes");
      releaseStream();
      await page.waitForFunction(() => document.querySelector('textarea[aria-label="Translator editor"]')?.value === "ok € done");
      await waitFor(() => requests.some(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt"));
      assert.deepEqual(requests.find(request => request.path === "/api/translator/send").body, { provider: "fixture", model: "model", body: { prompt: "Hello" } });
      assert.equal(requests.find(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt").body.content, "ok € done");
    } else {
      await settleReferenceJson();
      await page.getByRole("button", { name: /Client Request/ }).click();
      await page.getByRole("button", { name: /Target Request/ }).click();
      await page.locator(".monaco-editor .view-lines").waitFor();
      await page.evaluate(() => window.monaco.editor.getEditors()[0].setValue(JSON.stringify({ provider: "fixture", model: "model", body: { prompt: "Hello" } })));
      await settleReferenceJson();
      await page.getByRole("button", { name: /Send$/ }).last().click();
      await waitFor(() => typeof releaseStream === "function");
      assert.match(await page.evaluate(() => window.monaco.editor.getEditors()[0].getValue()), /"prompt":"Hello"/, "target request remains visible until the response stream finishes");
      assert.equal(requests.some(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt"), false, "stream is not saved before it finishes");
      releaseStream();
      await page.waitForFunction(() => window.monaco.editor.getEditors()[0]?.getValue() === "ok € done");
      await waitFor(() => requests.some(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt"));
      assert.deepEqual(requests.find(request => request.path === "/api/translator/send").body, { provider: "fixture", model: "model", body: { prompt: "Hello" } });
      assert.equal(requests.find(request => request.path === "/api/translator/save" && request.body.file === "5_res_provider.txt").body.content, "ok € done");
    }

    if (reference) {
      await page.locator(".monaco-editor .view-lines").waitFor();
      await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("😺 a+b A+B"); editor.setPosition({ lineNumber: 1, column: 1 }); editor.focus(); });
      await page.keyboard.press("Control+f");
    } else {
      await editor().fill("😺 a+b A+B");
      await editor().press("Control+f");
    }
    const selection = () => reference ? page.evaluate(() => { const s = window.monaco.editor.getEditors()[0].getSelection(); return [s.startColumn - 1, s.endColumn - 1]; }) : editor().evaluate(input => [input.selectionStart, input.selectionEnd]);
    const find = page.getByRole("dialog", { name: "Find / Replace" }).getByLabel("Find", { exact: true });
    await find.fill("a+b");
    await page.getByText("1 of 2", { exact: true }).waitFor();
    assert.deepEqual(await selection(), [3, 6]);
    await find.press("Enter");
    await page.getByText("2 of 2", { exact: true }).waitFor();
    assert.deepEqual(await selection(), [7, 10]);
    await find.press("Shift+Enter");
    await page.getByText("1 of 2", { exact: true }).waitFor();
    await find.press("Escape");
    assert.equal(await page.getByRole("dialog", { name: "Find / Replace" }).isVisible(), false);
    assert.equal(reference ? await page.evaluate(() => window.monaco.editor.getEditors()[0].hasTextFocus()) : await editor().evaluate(input => document.activeElement === input), true);

    if (reference) {
      await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("needle needle"); editor.setPosition({ lineNumber: 1, column: 14 }); editor.focus(); });
      await page.keyboard.press("Control+f");
    } else {
      await editor().fill("needle needle");
      await editor().press("Control+f");
    }
    await find.waitFor();
    assert.equal(await find.inputValue(), "needle", "Find seeds the word at the cursor");
    await page.getByText("2 of 2", { exact: true }).waitFor();
    assert.deepEqual(await selection(), [13, 13], "opening Find preserves the cursor");
    await find.press("Escape");

    if (reference) {
      await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("Alpha alpha alphabet"); editor.setPosition({ lineNumber: 1, column: 1 }); editor.focus(); });
      await page.keyboard.press("Control+f");
    } else {
      await editor().fill("Alpha alpha alphabet");
      await editor().press("Control+f");
    }
    await find.fill("alpha");
    await page.getByText("1 of 3", { exact: true }).waitFor();
    await page.getByRole("checkbox", { name: "Match Case (Alt+C)" }).click();
    await page.getByText("1 of 2", { exact: true }).waitFor();
    await page.getByRole("checkbox", { name: "Match Whole Word (Alt+W)" }).click();
    await page.getByText("1 of 1", { exact: true }).waitFor();
    await page.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await find.fill("a[a-z]+");
    await page.getByText("1 of 2", { exact: true }).waitFor();
    for (const name of ["Match Case (Alt+C)", "Match Whole Word (Alt+W)", "Use Regular Expression (Alt+R)"]) await page.getByRole("checkbox", { name }).click();
    await find.press("Escape");

    if (reference) {
      await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("cat cat"); editor.setPosition({ lineNumber: 1, column: 1 }); editor.focus(); });
      await page.keyboard.press("Control+h");
    } else {
      await editor().fill("cat cat");
      await editor().press("Control+h");
    }
    await find.fill("cat");
    const replace = page.getByRole("dialog", { name: "Find / Replace" }).getByLabel("Replace", { exact: true });
    await replace.fill("dog");
    await page.getByRole("button", { name: /^Replace \(/ }).click();
    const value = () => reference ? page.evaluate(() => window.monaco.editor.getEditors()[0].getValue()) : editor().inputValue();
    await page.waitForFunction(({ reference }) => reference ? window.monaco.editor.getEditors()[0].getValue() === "dog cat" : document.querySelector('textarea[aria-label="Translator editor"]')?.value === "dog cat", { reference });
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "cat cat", "replace remains undoable");
    await replace.focus();
    await page.getByRole("button", { name: /^Replace All \(/ }).click();
    await page.waitForFunction(({ reference }) => reference ? window.monaco.editor.getEditors()[0].getValue() === "dog dog" : document.querySelector('textarea[aria-label="Translator editor"]')?.value === "dog dog", { reference });
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "cat cat", "replace-all remains undoable as one action");

    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setValue("red-12 blue-34"));
    else await editor().fill("red-12 blue-34");
    await find.fill("([a-z]+)-(\\d+)");
    await page.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await replace.fill("$2:$1");
    await page.getByRole("button", { name: /^Replace All \(/ }).click();
    await page.waitForFunction(({ reference }) => reference ? window.monaco.editor.getEditors()[0].getValue() === "12:red 34:blue" : document.querySelector('textarea[aria-label="Translator editor"]')?.value === "12:red 34:blue", { reference });
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "red-12 blue-34", "regex replace-all preserves captures and is undoable");
    await find.focus();
    await find.press("Escape");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("ab"); editor.setPosition({ lineNumber: 1, column: 3 }); editor.focus(); });
    else await editor().fill("ab");
    await page.keyboard.press("Tab");
    assert.equal(await value(), "ab  ", "Tab inserts spaces up to the next indentation stop");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "ab", "Tab insertion remains undoable");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setValue("one\ntwo"));
    else await editor().fill("one\ntwo");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setSelection(new window.monaco.Selection(1, 1, 2, 4)); editor.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(0, input.value.length));
    await page.keyboard.press("Tab");
    assert.equal(await value(), "    one\n    two", "Tab indents all selected lines");
    await page.keyboard.press("Shift+Tab");
    assert.equal(await value(), "one\ntwo", "Shift+Tab removes one indentation level");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "    one\n    two", "line unindent remains undoable");

    await page.getByRole("button", { name: /Provider Response/ }).click();
    await page.getByRole("button", { name: /Client Request/ }).click();
    if (reference) {
      await page.locator(".monaco-editor .view-lines").waitFor();
      await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("{}"); editor.setPosition({ lineNumber: 1, column: 2 }); editor.focus(); });
    } else {
      await editor().fill("{}");
      await editor().evaluate(input => input.setSelectionRange(1, 1));
    }
    await page.keyboard.press("Enter");
    assert.equal(await value(), "{\n    \n}", "Enter expands a JSON bracket pair with an indented line");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "{}", "JSON auto-indentation remains undoable");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue('"{}"'); editor.setPosition({ lineNumber: 1, column: 3 }); editor.focus(); });
    else { await editor().fill('"{}"'); await editor().evaluate(input => input.setSelectionRange(2, 2)); }
    await page.keyboard.press("Enter");
    assert.equal(await value(), '"{\n}"', "brackets inside a JSON string do not expand as a structural pair");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(""); editor.focus(); });
    else await editor().fill("");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("{");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await value(), "{}", "typing an opening JSON brace closes its pair");
    assert.deepEqual(await selection(), [1, 1]);
    await page.keyboard.type("}");
    assert.equal(await value(), "{}", "typing the auto-inserted closing brace moves over it");
    assert.deepEqual(await selection(), [2, 2]);
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "", "automatic brace pair is one undoable edit");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("{");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("x");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("}");
    assert.equal(await value(), "{x}", "automatic closing brace tracks text typed inside the pair");
    assert.deepEqual(await selection(), [3, 3]);
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(""); editor.focus(); });
    else await editor().fill("");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("{");
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Backspace");
    assert.equal(await value(), "", "Backspace removes both sides of an empty auto-closed pair");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue("word"); editor.setPosition({ lineNumber: 1, column: 1 }); editor.focus(); });
    else { await editor().fill("word"); await editor().evaluate(input => input.setSelectionRange(0, 0)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("{");
    assert.equal(await value(), "{word", "opening brace is not paired before an existing word");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue('"text"'); editor.setPosition({ lineNumber: 1, column: 4 }); editor.focus(); });
    else { await editor().fill('"text"'); await editor().evaluate(input => input.setSelectionRange(3, 3)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.type("{");
    assert.equal(await value(), '"te{xt"', "opening brace is not paired inside a JSON string");

    const numericJson = '{"probability":1.0,"limit":1e3,"ratio":-0.0}';
    if (reference) {
      await settleReferenceJson();
      await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), numericJson);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    } else await editor().fill(numericJson);
    await page.getByRole("button", { name: /Format$/ }).click();
    assert.equal(await value(), JSON.stringify(JSON.parse(numericJson), null, 2), "Format uses JavaScript JSON number semantics");
    assert.deepEqual(errors, []);
    console.log("synthetic translator flow passed");
  } finally { await browser.close(); server.close(); }
}

main().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
