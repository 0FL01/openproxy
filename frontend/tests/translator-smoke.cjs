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
    const value = () => reference ? page.evaluate(() => window.monaco.editor.getEditors()[0].getValue()) : editor().inputValue();
    const selection = () => reference ? page.evaluate(() => { const s = window.monaco.editor.getEditors()[0].getSelection(); return [s.startColumn - 1, s.endColumn - 1]; }) : editor().evaluate(input => [input.selectionStart, input.selectionEnd]);
    const settleReferenceJson = () => page.waitForFunction(async () => {
      try {
        const model = window.monaco?.editor.getEditors()[0]?.getModel();
        if (!model || !window.monaco?.languages.json?.getWorker) return false;
        const worker = await (await window.monaco.languages.json.getWorker())(model.uri);
        await worker.doValidation(model.uri.toString());
        return true;
      } catch { return false; }
    });
    if (reference) await page.locator(".monaco-editor .view-lines").waitFor();
    if (process.env.TRANSLATOR_WRAP_TOKENS_ONLY === "1") {
      const text = `{\n  "message": "${"x".repeat(200)}",\n  "count": 42\n}`;
      if (reference) { await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), text); await settleReferenceJson(); }
      else await editor().fill(text);
      for (const width of [1440, 390]) {
        await page.setViewportSize({ width, height: 900 });
        await page.waitForFunction(reference => {
          const spans = [...document.querySelectorAll(reference ? ".monaco-editor .view-lines .view-line span" : "pre > div > span")]
            .filter(span => !span.children.length && span.textContent.includes("x"));
          return spans.reduce((count, span) => count + [...span.textContent].filter(ch => ch === "x").length, 0) === 200;
        }, reference);
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const pieces = await page.evaluate(reference => [...document.querySelectorAll(reference ? ".monaco-editor .view-lines .view-line span" : "pre > div > span")]
          .filter(span => !span.children.length && span.textContent.includes("x"))
          .map(span => ({ text: span.textContent, rows: span.getClientRects().length })), reference);
        assert.ok(pieces.every(piece => piece.text.length <= 50 && piece.rows === 1), `long ASCII tokens are split within each visual line: ${JSON.stringify(pieces)}`);
      }
      assert.deepEqual(errors, []);
      console.log("synthetic translator wrapped-token smoke passed");
      return;
    }
    if (process.env.TRANSLATOR_SCROLL_ONLY === "1") {
      const text = JSON.stringify({ items: Array.from({ length: 80 }, (_, index) => `row${index} needle`) }, null, 2);
      if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), text);
      else await editor().fill(text);
      for (const width of [1280, 390]) {
        await page.setViewportSize({ width, height: 844 });
        const offset = text.indexOf("row75") + 3;
        if (reference) await page.evaluate(offset => {
          const e = window.monaco.editor.getEditors()[0], position = e.getModel().getPositionAt(offset);
          e.setPosition(position); e.revealPositionInCenter(position, 0); e.focus();
        }, offset);
        else {
          await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
          await editor().evaluate((input, offset) => {
            input.setSelectionRange(offset, offset);
            const line = input.value.slice(0, offset).split("\n").length - 1;
            input.scrollTop = input.parentElement.querySelector("pre").children[line].offsetTop - 150;
          }, offset);
        }
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const rect = await page.evaluate(reference => {
          const root = reference ? document.querySelector(".monaco-editor .view-lines") : document.querySelector('[aria-label="Translator editor"]').parentElement.querySelector("pre");
          const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
          let node;
          while ((node = walker.nextNode())) {
            const index = node.textContent.indexOf("row75");
            if (index < 0) continue;
            const range = document.createRange(); range.setStart(node, index); range.setEnd(node, index + 5);
            const r = range.getBoundingClientRect();
            range.setStart(node, index + 3); range.setEnd(node, index + 4);
            return { x: r.x, y: r.y, width: r.width, height: r.height, caretX: range.getBoundingClientRect().x };
          }
          throw new Error("Scrolled row is not rendered");
        }, reference);
        assert.ok(rect.y > 0 && rect.y + rect.height < 844, "a late document row remains in the visible editor viewport");
        const screenshot = await page.screenshot({ clip: { x: rect.x, y: rect.y, width: rect.width, height: rect.height } });
        assert.ok(await page.evaluate(async base64 => {
          const image = new Image(); image.src = `data:image/png;base64,${base64}`; await image.decode();
          const canvas = document.createElement("canvas"); canvas.width = image.width; canvas.height = image.height;
          const context = canvas.getContext("2d"); context.drawImage(image, 0, 0);
          const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
          for (let i = 0; i < pixels.length; i += 4) if (Math.max(pixels[i], pixels[i + 1], pixels[i + 2]) > 100) return true;
          return false;
        }, screenshot.toString("base64")), "the composited viewport paints text after scrolling, rather than clipping it away");
        await page.mouse.click(rect.caretX + 1, rect.y + rect.height / 2);
        const actualOffset = reference ? await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; return e.getModel().getOffsetAt(e.getPosition()); }) : await editor().evaluate(input => input.selectionStart);
        assert.equal(actualOffset, offset, "a click on the scrolled text maps to its document position after resizing");
      }
      assert.deepEqual(errors, []);
      console.log("synthetic translator long-document scroll smoke passed");
      return;
    }
    if (process.env.TRANSLATOR_BRACKETS_ONLY === "1") {
      const setText = async (index, text, offset) => {
        if (reference) await page.evaluate(({ index, text, offset }) => {
          const e = window.monaco.editor.getEditors()[index];
          e.setValue(text); e.setPosition(e.getModel().getPositionAt(offset)); e.focus();
        }, { index, text, offset });
        else {
          await editor().nth(index).fill(text);
          await editor().nth(index).evaluate((input, offset) => {
            input.setSelectionRange(offset, offset); input.dispatchEvent(new Event("select", { bubbles: true }));
          }, offset);
        }
      };
      const move = async (index, start, end = start) => {
        if (reference) await page.evaluate(({ index, start, end }) => {
          const e = window.monaco.editor.getEditors()[index], m = e.getModel();
          e.setSelection(window.monaco.Range.fromPositions(m.getPositionAt(start), m.getPositionAt(end)));
        }, { index, start, end });
        else await editor().nth(index).evaluate((input, { start, end }) => {
          input.setSelectionRange(start, end); input.dispatchEvent(new Event("select", { bubbles: true }));
        }, { start, end });
      };
      const expectBrackets = (index, expected) => page.waitForFunction(({ reference, index, expected }) => {
        if (reference) {
          const model = window.monaco.editor.getEditors()[index]?.getModel();
          return model && model.getAllDecorations().filter(d => d.options.description?.startsWith("bracket-match"))
            .map(d => model.getValueInRange(d.range)).join("") === expected;
        }
        const input = document.querySelectorAll('[aria-label="Translator editor"]')[index];
        return input && Array.from(input.parentElement.querySelectorAll("span.bracket-match"), span => span.textContent).join("") === expected;
      }, { reference, index, expected });

      const expectBracketRuler = (index, visible) => page.waitForFunction(({ reference, index, visible }) => {
        const root = reference
          ? window.monaco.editor.getEditors()[index]?.getDomNode()
          : document.querySelectorAll('[aria-label="Translator editor"]')[index]?.parentElement.parentElement;
        const canvas = root?.querySelector("canvas");
        if (!canvas) return false;
        const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data;
        const rows = new Set();
        for (let i = 0; i < pixels.length; i += 4) {
          if (pixels[i] === 160 && pixels[i + 1] === 160 && pixels[i + 2] === 160 && pixels[i + 3] === 255) rows.add(Math.floor(i / 4 / canvas.width));
        }
        return visible ? rows.has(20) && rows.has(72) && !rows.has(40) : rows.size === 0;
      }, { reference, index, visible });

      const rulerJson = '{\n  "a": [0,\n    1,\n    2,\n    3]\n}';
      await setText(0, rulerJson, rulerJson.indexOf("[") + 1);
      await expectBrackets(0, "[]");
      await expectBracketRuler(0, true);
      await move(0, rulerJson.indexOf("0") + 1);
      await expectBrackets(0, "[]");
      await expectBracketRuler(0, false); // enclosing pairs have no ruler markers
      await move(0, rulerJson.indexOf("[") + 1, rulerJson.indexOf("0") + 1);
      await expectBracketRuler(0, false); // selections clear bracket markers

      const json = '{"a":[1,2],"str":"😺{x}"}';
      await setText(0, json, json.indexOf("1") + 1);
      await expectBrackets(0, "[]");
      await move(0, json.indexOf("x"));
      await expectBrackets(0, "{}"); // braces inside JSON strings are not structural
      await move(0, json.indexOf("1"), json.indexOf("1") + 1);
      await expectBrackets(0, "");
      await move(0, json.indexOf("1") + 1);
      await page.keyboard.press("Control+f");
      const find = page.getByRole("dialog", { name: "Find / Replace" }).getByLabel("Find", { exact: true });
      await find.waitFor();
      await expectBrackets(0, "[]"); // Find is part of the same focused editor widget
      await find.fill("2");
      await expectBrackets(0, ""); // selecting a Find result removes the bracket decoration
      await page.waitForFunction(reference => reference || Array.from(document.querySelectorAll("pre span"), span => getComputedStyle(span).boxShadow).every(shadow => shadow === "none"), reference);
      await page.keyboard.press("Escape");
      await page.getByRole("button", { name: /folder_open Load/ }).first().focus();
      await expectBrackets(0, "");

      if (reference) await settleReferenceJson();
      await page.getByRole("button", { name: /Provider Response/ }).click();
      if (reference) await page.locator(".monaco-editor .view-lines").nth(1).waitFor();
      else await editor().nth(1).waitFor();
      const plain = "x { [a] } (y)";
      await setText(1, plain, plain.indexOf("a"));
      await expectBrackets(1, "[]");
      await move(1, plain.indexOf("y"));
      await expectBrackets(1, "()");
      assert.deepEqual(errors, []);
      console.log("synthetic translator brackets smoke passed");
      return;
    }
    if (process.env.TRANSLATOR_PRESERVE_CASE_ONLY === "1") {
      const initial = '{"one":"cat CAT Cat cAt"}';
      if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition({ lineNumber: 1, column: 1 }); e.focus(); }, initial);
      else { await editor().fill(initial); await editor().evaluate(input => input.setSelectionRange(0, 0)); }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("Control+h");
      const dialog = page.getByRole("dialog", { name: "Find / Replace" });
      const find = dialog.getByLabel("Find", { exact: true });
      const replace = dialog.getByLabel("Replace", { exact: true });
      const preserve = dialog.getByRole("checkbox", { name: /Preserve Case/ });
      await find.fill("cat");
      await page.setViewportSize({ width: 390, height: 844 });
      await preserve.waitFor({ state: "visible" });
      assert.equal(await replace.evaluate(input => getComputedStyle(input).whiteSpace), "nowrap", "the empty replacement placeholder stays on one line");
      await preserve.click();
      assert.equal(await preserve.getAttribute("aria-checked"), "true", "Preserve Case remains usable in the collapsed Replace panel");
      await preserve.click();
      assert.equal(await preserve.getAttribute("aria-checked"), "false");
      await page.setViewportSize({ width: 1440, height: 900 });
      await dialog.getByRole("checkbox", { name: /Match Case/ }).waitFor({ state: "visible" });
      await replace.fill("dog");
      await preserve.click();
      await replace.press("Alt+p");
      assert.equal(await preserve.getAttribute("aria-checked"), "false");
      await replace.press("Alt+p");
      assert.equal(await preserve.getAttribute("aria-checked"), "true");
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
      await page.keyboard.press("Alt+p");
      assert.equal(await preserve.getAttribute("aria-checked"), "false");
      await page.keyboard.press("Alt+p");
      assert.equal(await preserve.getAttribute("aria-checked"), "true");
      await dialog.getByRole("button", { name: /Replace All/ }).click();
      assert.equal(await value(), '{"one":"dog DOG Dog dog"}');
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
      await page.keyboard.press("Control+z");
      assert.equal(await value(), initial, "preserve-case replacement remains undoable");
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setSelection(new window.monaco.Selection(1, 9, 1, 12)));
      else await editor().evaluate(input => { input.setSelectionRange(8, 11); input.dispatchEvent(new Event("select", { bubbles: true })); });
      await page.getByText("1 of 4", { exact: true }).waitFor({ state: "attached" });
      await dialog.getByRole("button", { name: /^Replace \(/ }).click();
      if (reference) await page.waitForFunction(() => window.monaco.editor.getEditors()[0].getValue() === '{"one":"dog CAT Cat cAt"}');
      else await page.waitForFunction(() => document.querySelector('[aria-label="Translator editor"]')?.value === '{"one":"dog CAT Cat cAt"}');
      assert.equal(await value(), '{"one":"dog CAT Cat cAt"}');
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
      await page.keyboard.press("Control+z");
      assert.equal(await value(), initial);

      const separated = '{"one":"foo-BAR Foo-bar"}';
      if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), separated);
      else await editor().fill(separated);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await find.fill("foo-bar");
      await replace.fill("ping-pong");
      await dialog.getByRole("button", { name: /Replace All/ }).click();
      assert.equal(await value(), '{"one":"ping-PONG Ping-pong"}');

      const captures = '{"one":"cat CAT"}';
      if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), captures);
      else await editor().fill(captures);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await find.fill("(cat)");
      await dialog.getByRole("checkbox", { name: /Use Regular Expression/ }).click();
      await replace.fill("bird");
      await dialog.getByRole("button", { name: /Replace All/ }).click();
      assert.equal(await value(), '{"one":"bird BIRD"}', "static regex replacements preserve case");
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
      await page.keyboard.press("Control+z");
      assert.equal(await value(), captures);
      await replace.fill("$1lower");
      await page.getByText("1 of 2", { exact: true }).waitFor({ state: "attached" });
      await dialog.getByRole("button", { name: /Replace All/ }).click();
      assert.equal(await value(), '{"one":"catlower CATlower"}', "dynamic capture replacements do not apply preserve-case transformation");

      if (reference) await settleReferenceJson();
      await page.getByRole("button", { name: /Source Body/ }).click();
      if (reference) {
        await page.locator(".monaco-editor .view-lines").nth(1).waitFor();
        await page.evaluate(() => window.monaco.editor.getEditors()[1].focus());
      } else await editor().nth(1).focus();
      await page.keyboard.press("Control+h");
      const secondDialog = page.getByRole("dialog", { name: "Find / Replace" }).nth(1);
      const secondPreserve = secondDialog.getByRole("checkbox", { name: /Preserve Case/ });
      await secondPreserve.waitFor();
      assert.equal(await secondPreserve.getAttribute("aria-checked"), "true", "new editors inherit the last preserve-case setting");
      await page.getByRole("dialog", { name: "Find / Replace" }).first().getByRole("checkbox", { name: /Preserve Case/ }).click();
      assert.equal(await secondPreserve.getAttribute("aria-checked"), "true", "an already open editor retains its own setting");
      assert.deepEqual(errors, []);
      console.log("synthetic translator preserve-case smoke passed");
      return;
    }
    if (process.env.TRANSLATOR_FIND_VIEWZONE_ONLY === "1" || process.env.TRANSLATOR_FIND_PAINT_ONLY === "1") {
      const text = '{"message":"needle needle"}';
      const offset = text.indexOf("needle");
      if (reference) await page.evaluate(({ text, offset }) => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition(e.getModel().getPositionAt(offset)); e.focus(); }, { text, offset });
      else { await editor().fill(text); await editor().evaluate((input, offset) => input.setSelectionRange(offset, offset), offset); }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("Control+f");
      const dialog = page.getByRole("dialog", { name: "Find / Replace" });
      await dialog.getByLabel("Find", { exact: true }).waitFor();
      assert.deepEqual(await dialog.getByLabel("Find", { exact: true }).evaluate(input => [input.selectionStart, input.selectionEnd]), [0, 6], "the seeded search is selected on opening");
      await page.waitForFunction(reference => {
        const matches = [...document.querySelectorAll(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match")];
        return matches.length === 2 && matches.every(element => getComputedStyle(element).backgroundColor === "rgba(234, 92, 0, 0.33)");
      }, reference);
      if (process.env.TRANSLATOR_FIND_PAINT_ONLY === "1") {
        const optionPositions = await dialog.evaluate(root => {
          const left = root.getBoundingClientRect().left;
          return ["Match Case (Alt+C)", "Match Whole Word (Alt+W)", "Use Regular Expression (Alt+R)"]
            .map(name => Math.round(root.querySelector(`[aria-label="${name}"]`).getBoundingClientRect().left - left));
        });
        assert.deepEqual(optionPositions, [190, 212, 234], "Find options align with the reference panel");
        const scopeStyle = await dialog.getByRole("checkbox", { name: "Find in Selection (Alt+L)" }).evaluate(element => {
          const style = getComputedStyle(element);
          return [style.display, style.textAlign];
        });
        assert.deepEqual(scopeStyle, ["block", "center"], "Find in Selection uses the reference icon layout");
        const findRadius = await dialog.getByLabel("Find", { exact: true }).evaluate((input, reference) => {
          const box = input.closest(reference ? ".monaco-inputbox" : ".find-box");
          return getComputedStyle(box).borderRadius;
        }, reference);
        assert.equal(findRadius, "2px", "Find input has the reference rounded corners");
        const matchPoint = await page.evaluate(reference => {
          const match = document.querySelector(reference ? ".monaco-editor .view-overlays .findMatch" : ".find-match").getBoundingClientRect();
          return { x: Math.floor(match.left + 2), y: Math.floor(match.top + 1) };
        }, reference);
        const matchPixel = await page.screenshot({ animations: "disabled" });
        const matchColor = await page.evaluate(async ({ base64, point }) => {
          const image = new Image(); image.src = `data:image/png;base64,${base64}`; await image.decode();
          const canvas = document.createElement("canvas"); canvas.width = canvas.height = 1;
          const context = canvas.getContext("2d"); context.drawImage(image, point.x, point.y, 1, 1, 0, 0, 1, 1);
          return [...context.getImageData(0, 0, 1, 1).data].slice(0, 3);
        }, { base64: matchPixel.toString("base64"), point: matchPoint });
        assert.ok(matchColor[0] > matchColor[1] + 15 && matchColor[1] > matchColor[2] + 15, "the current-line border does not cover the Find match");
        const descenderVisible = async (text, offset) => {
          const glyph = await page.evaluate(({ reference, text, offset }) => {
            const line = reference ? document.querySelector(".monaco-editor .view-lines .view-line")
              : document.querySelector('textarea[aria-label="Translator editor"]').parentElement.querySelector("pre > div");
            const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
            let node;
            while ((node = walker.nextNode())) {
              const index = node.textContent.indexOf(text);
              if (index < 0) continue;
              const range = document.createRange(); range.setStart(node, index + offset); range.setEnd(node, index + offset + 1);
              const rect = range.getBoundingClientRect();
              return { x: Math.floor(rect.left), width: Math.ceil(rect.right) - Math.floor(rect.left), y: Math.floor(line.getBoundingClientRect().bottom) - 2 };
            }
            throw new Error("Synthetic glyph not rendered");
          }, { reference, text, offset });
          const image = await page.screenshot({ animations: "disabled" });
          return page.evaluate(async ({ base64, glyph }) => {
            const image = new Image(); image.src = `data:image/png;base64,${base64}`; await image.decode();
            const canvas = document.createElement("canvas"); canvas.width = glyph.width; canvas.height = 1;
            const context = canvas.getContext("2d"); context.drawImage(image, glyph.x, glyph.y, glyph.width, 1, 0, 0, glyph.width, 1);
            const pixels = context.getImageData(0, 0, glyph.width, 1).data;
            for (let i = 0; i < pixels.length; i += 4) if (Math.max(pixels[i], pixels[i + 1], pixels[i + 2]) > 55) return true;
            return false;
          }, { base64: image.toString("base64"), glyph });
        };
        assert.ok(await descenderVisible("message", 5), "the current-line border does not cover the JSON property descender");
        await dialog.getByRole("button", { name: "Toggle Replace" }).click();
        const preservePosition = await dialog.evaluate(root => Math.round(root.querySelector('[aria-label="Preserve Case (Alt+P)"]').getBoundingClientRect().left - root.getBoundingClientRect().left));
        assert.equal(preservePosition, 234, "Replace option aligns with the reference panel");
        await dialog.getByLabel("Find", { exact: true }).press("Escape");
        if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setValue('"g"'); e.setPosition({ lineNumber: 1, column: 4 }); e.focus(); });
        else { await editor().fill('"g"'); await editor().evaluate(input => { input.setSelectionRange(3, 3); input.dispatchEvent(new Event("select", { bubbles: true })); }); }
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        assert.ok(await descenderVisible("g", 0), "the current-line border does not cover undecorated text");
        assert.deepEqual(errors, []);
        console.log("synthetic translator Find paint smoke passed");
        return;
      }
      await dialog.getByLabel("Find", { exact: true }).press("Enter");
      await dialog.getByText("1 of 2", { exact: true }).waitFor();
      await page.waitForFunction(reference => {
        const matches = [...document.querySelectorAll(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match")];
        return matches.some(element => getComputedStyle(element).backgroundColor === "rgb(81, 92, 106)");
      }, reference);
      await dialog.getByLabel("Find", { exact: true }).press("Enter");
      await dialog.getByText("2 of 2", { exact: true }).waitFor();
      await page.waitForFunction(reference => {
        const line = document.querySelector(reference ? ".monaco-editor .view-overlays .rangeHighlight" : ".find-range-highlight");
        return line && getComputedStyle(line).backgroundColor === "rgba(255, 255, 255, 0.04)";
      }, reference);
      assert.equal(await page.evaluate(reference => {
        const root = reference ? document.querySelector(".monaco-editor") : document.querySelector('textarea[aria-label="Translator editor"]').parentElement;
        const line = root.querySelector(reference ? ".view-lines .view-line" : "pre > div").getBoundingClientRect();
        const matches = [...root.querySelectorAll(reference ? ".view-overlays .findMatch, .view-overlays .currentFindMatch" : ".find-match")];
        return matches.length === 2 && matches.every(element => { const rect = element.getBoundingClientRect(); return rect.top === line.top && rect.bottom === line.bottom; });
      }, reference), true, "Find match backgrounds fill the visual line height");
      const findMarker = ({ reference, present, y }) => {
        const canvas = reference ? document.querySelector(".monaco-editor canvas.decorationsOverviewRuler") : document.querySelector('textarea[aria-label="Translator editor"]').parentElement.parentElement.querySelector("canvas");
        if (!canvas) return false;
        const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data;
        const rows = [];
        for (let i = 0; i < pixels.length; i += 4) if (pixels[i] > 150 && pixels[i + 1] > 90 && pixels[i + 2] < 60 && pixels[i + 3] > 0) rows.push(Math.floor(i / 4 / canvas.width));
        return present ? rows.length > 0 && Math.min(...rows) === y[0] && Math.max(...rows) === y[1] : rows.length === 0;
      };
      await page.waitForFunction(findMarker, { reference, present: true, y: [34, 48] }, { timeout: 5000 });
      assert.deepEqual(await dialog.evaluate(element => [element.offsetWidth, element.offsetHeight]), [419, 33], "desktop Find panel matches the reference dimensions");
      assert.equal(await dialog.getByLabel("Find", { exact: true }).evaluate(element => element.offsetWidth), 167);
      const lineTop = ({ reference, expected }) => {
        const input = document.querySelector('textarea[aria-label="Translator editor"]');
        const root = reference ? document.querySelector(".monaco-editor") : input?.parentElement;
        const line = reference ? root?.querySelector(".view-lines .view-line") : root?.querySelector("pre > div");
        return root && line && Math.abs(line.getBoundingClientRect().top - root.getBoundingClientRect().top - expected) < 1;
      };
      await page.waitForFunction(lineTop, { reference, expected: 33 }, { timeout: 5000 });
      await dialog.getByRole("button", { name: "Toggle Replace" }).click();
      await page.waitForFunction(lineTop, { reference, expected: 62 }, { timeout: 5000 });
      assert.equal(await dialog.evaluate(element => element.offsetHeight), 62, "Replace expands the panel to the reference height");
      await dialog.getByLabel("Find", { exact: true }).press("Escape");
      await page.waitForFunction(lineTop, { reference, expected: 0 }, { timeout: 5000 });
      await page.waitForFunction(reference => !document.querySelector(reference ? ".monaco-editor .view-overlays .rangeHighlight" : ".find-range-highlight"), reference);
      await page.waitForFunction(findMarker, { reference, present: false });

      if (reference) await page.evaluate(({ offset }) => { const e = window.monaco.editor.getEditors()[0], model = e.getModel(), start = model.getPositionAt(offset), end = model.getPositionAt(offset + 6); e.setSelection(new window.monaco.Selection(start.lineNumber, start.column, end.lineNumber, end.column)); e.focus(); }, { offset });
      else { await editor().focus(); await editor().evaluate((input, offset) => { input.setSelectionRange(offset, offset + 6); input.dispatchEvent(new Event("select", { bubbles: true })); }, offset); }
      await page.keyboard.press("Control+f");
      await page.waitForFunction(reference => {
        const root = reference ? ".monaco-editor .view-overlays" : "pre";
        const matches = [...document.querySelectorAll(reference ? `${root} .findMatch, ${root} .currentFindMatch` : `${root} .find-match`)];
        return matches.length === 2 && matches.every(element => getComputedStyle(element).backgroundColor === "rgba(234, 92, 0, 0.33)")
          && !document.querySelector(reference ? `${root} .rangeHighlight` : `${root} .find-range-highlight`);
      }, reference);
      await dialog.getByLabel("Find", { exact: true }).press("Escape");

      const viewport = page.viewportSize();
      await page.setViewportSize({ width: 390, height: 844 });
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
      await page.keyboard.press("Control+f");
      await page.waitForFunction(() => document.querySelector('[role="dialog"][aria-label="Find / Replace"]')?.getBoundingClientRect().width === 170);
      assert.equal(await dialog.getByLabel("Find", { exact: true }).evaluate(element => element.offsetWidth), 56, "mobile Find input retains space for navigation");
      assert.equal(await dialog.locator('[aria-label="Match Case (Alt+C)"]').isVisible(), false, "mobile collapsed mode hides inline options");
      await dialog.getByLabel("Find", { exact: true }).press("Escape");
      const wrapped = '{"message":"' + "x".repeat(60) + " needle " + "x".repeat(90) + '"}';
      const wrappedOffset = wrapped.indexOf("needle");
      if (reference) await page.evaluate(({ text, offset }) => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition(e.getModel().getPositionAt(offset)); e.focus(); }, { text: wrapped, offset: wrappedOffset });
      else { await editor().fill(wrapped); await editor().evaluate((input, offset) => input.setSelectionRange(offset, offset), wrappedOffset); }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("Control+f");
      await dialog.getByLabel("Find", { exact: true }).press("Enter");
      await page.waitForFunction(reference => !!document.querySelector(reference ? ".monaco-editor .view-overlays .rangeHighlight" : ".find-range-highlight"), reference);
      const wrappedCoverage = await page.evaluate(reference => {
        const root = reference ? document.querySelector(".monaco-editor") : document.querySelector('textarea[aria-label="Translator editor"]').parentElement;
        const lines = [...root.querySelectorAll(reference ? ".view-lines .view-line" : "pre > div")].map(element => element.getBoundingClientRect());
        const ranges = [...root.querySelectorAll(reference ? ".view-overlays .rangeHighlight" : ".find-range-highlight")].map(element => element.getBoundingClientRect());
        return [ranges[0].top === lines[0].top, ranges.at(-1).bottom === lines.at(-1).bottom];
      }, reference);
      assert.deepEqual(wrappedCoverage, [true, true], "the range highlight covers the wrapped logical line");
      await page.waitForFunction(findMarker, { reference, present: true, y: [82, 96] });
      await dialog.getByLabel("Find", { exact: true }).press("Escape");
      await page.setViewportSize(viewport);

      const longText = JSON.stringify({ items: Array.from({ length: 80 }, (_, index) => `row${index} needle`) }, null, 2);
      const longOffset = longText.indexOf("needle", longText.indexOf("row40"));
      const longLine = longText.slice(0, longOffset).split("\n").length;
      if (reference) await page.evaluate(({ text, offset }) => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); const p = e.getModel().getPositionAt(offset); e.setPosition(p); e.revealPositionInCenter(p, 0); e.focus(); }, { text: longText, offset: longOffset });
      else {
        await editor().fill(longText);
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        await editor().evaluate((input, { offset, line }) => { input.setSelectionRange(offset, offset); input.scrollTop = input.parentElement.querySelector("pre").children[line - 1].offsetTop - 150; }, { offset: longOffset, line: longLine });
      }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const rowTop = ({ reference, offset, line }) => {
        if (reference) {
          const e = window.monaco.editor.getEditors()[0];
          return e.getScrolledVisiblePosition(e.getModel().getPositionAt(offset))?.top;
        }
        const input = document.querySelector('textarea[aria-label="Translator editor"]');
        return input.parentElement.querySelector("pre").children[line - 1].getBoundingClientRect().top - input.getBoundingClientRect().top;
      };
      const before = await page.evaluate(rowTop, { reference, offset: longOffset, line: longLine });
      assert.ok(before > 60 && before < 340, "the selected line starts within the scroll viewport");
      await page.keyboard.press("Control+f");
      await dialog.getByLabel("Find", { exact: true }).waitFor();
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.ok(Math.abs(await page.evaluate(rowTop, { reference, offset: longOffset, line: longLine }) - before) < 1, "opening Find preserves the scrolled position");
      await dialog.getByLabel("Find", { exact: true }).press("Escape");
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.ok(Math.abs(await page.evaluate(rowTop, { reference, offset: longOffset, line: longLine }) - before) < 1, "closing Find preserves the scrolled position");
      assert.deepEqual(errors, []);
      console.log("synthetic translator Find view-zone smoke passed");
      return;
    }
    if (process.env.TRANSLATOR_CURRENT_LINE_ONLY === "1") {
      await page.setViewportSize({ width: 390, height: 844 });
      const wrapped = `{"message":"${"x".repeat(200)}"}`;
      if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition(e.getModel().getPositionAt(70)); e.focus(); }, wrapped);
      else { await editor().fill(wrapped); await editor().evaluate(input => input.setSelectionRange(70, 70)); }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("ArrowRight");
      const currentLineTop = ({ reference, expected }) => {
        const input = document.querySelector('textarea[aria-label="Translator editor"]');
        const root = reference ? document.querySelector(".monaco-editor") : input?.parentElement;
        const base = reference ? root?.querySelector(".view-lines .view-line") : root?.querySelector("pre > div");
        const current = reference ? root?.querySelector(".view-overlays .current-line-exact")
          : [...(base?.children || [])].find(child => child.tagName === "DIV" && getComputedStyle(child).borderTopWidth === "2px");
        if (expected === null) return !current;
        return current && base && Math.abs(current.getBoundingClientRect().top - base.getBoundingClientRect().top - expected) < 1;
      };
      await page.waitForFunction(currentLineTop, { reference, expected: 48 });
      await page.setViewportSize({ width: 600, height: 844 });
      await page.waitForFunction(currentLineTop, { reference, expected: 16 });
      await page.keyboard.press("Control+Home");
      await page.waitForFunction(currentLineTop, { reference, expected: 0 });
      await page.keyboard.press("Shift+ArrowRight");
      await page.waitForFunction(currentLineTop, { reference, expected: null });
      await page.setViewportSize({ width: 390, height: 844 });
      const withGuide = `{\n  "message": "${"x".repeat(200)}",\n  "count": 42\n}`;
      if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition(e.getModel().getPositionAt(71)); e.focus(); }, withGuide);
      else { await editor().fill(withGuide); await editor().evaluate(input => { input.setSelectionRange(71, 71); input.dispatchEvent(new Event("select", { bubbles: true })); }); }
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const guidePoint = await (await page.waitForFunction(reference => {
        const input = document.querySelector('textarea[aria-label="Translator editor"]');
        const root = reference ? document.querySelector(".monaco-editor") : input?.parentElement;
        const text = reference ? root?.querySelector(".view-lines") : root?.querySelector("pre");
        const current = reference ? root?.querySelector(".view-overlays .current-line-exact")
          : [...(text?.querySelectorAll("div > div") || [])].find(child => getComputedStyle(child).borderTopWidth === "2px");
        return text && current && { x: Math.ceil(text.getBoundingClientRect().left), y: Math.floor(current.getBoundingClientRect().top + 8) };
      }, reference)).jsonValue();
      const png = await page.screenshot({ animations: "disabled" });
      const color = await page.evaluate(async ({ base64, point }) => {
        const image = new Image(); image.src = `data:image/png;base64,${base64}`; await image.decode();
        const canvas = document.createElement("canvas"); canvas.width = canvas.height = 1;
        const context = canvas.getContext("2d"); context.drawImage(image, point.x, point.y, 1, 1, 0, 0, 1, 1);
        return [...context.getImageData(0, 0, 1, 1).data].slice(0, 3);
      }, { base64: png.toString("base64"), point: guidePoint });
      assert.ok(color.every(channel => channel > 70), "the indentation guide remains visible over the current-line border");
      assert.deepEqual(errors, []);
      console.log("synthetic translator current-line smoke passed");
      return;
    }
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setValue("ab"); e.setPosition({ lineNumber: 1, column: 3 }); e.focus(); });
    else await editor().fill("ab");
    await page.keyboard.press("Control+m");
    await page.getByRole("alert").filter({ hasText: "Pressing Tab will now move focus to the next focusable element" }).waitFor();
    await page.keyboard.press("Tab");
    assert.match(await page.evaluate(() => document.activeElement?.textContent || ""), /Load/, "Tab moves focus out of the editor when tab focus mode is enabled");
    assert.equal(await value(), "ab", "moving focus does not edit the document");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+m");
    await page.getByRole("alert").filter({ hasText: "Pressing Tab will now insert the tab character" }).waitFor();
    await page.keyboard.press("Tab");
    assert.equal(await value(), "ab  ", "Tab inserts indentation after tab focus mode is disabled");
    if (process.env.TRANSLATOR_TAB_FOCUS_ONLY === "1") {
      await page.keyboard.press("Control+m");
      await page.getByRole("button", { name: /Source Body/ }).click();
      if (reference) {
        await page.locator(".monaco-editor .view-lines").nth(1).waitFor();
        await page.evaluate(() => { const e = window.monaco.editor.getEditors()[1]; e.setValue("second"); e.setPosition({ lineNumber: 1, column: 7 }); e.focus(); });
      } else await editor().last().fill("second");
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("Tab");
      assert.match(await page.evaluate(() => document.activeElement?.textContent || ""), /Load/, "tab focus mode is shared with another open stage");
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[1].focus()); else await editor().last().focus();
      await page.keyboard.press("Control+m");
      if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().first().focus();
      await page.keyboard.press("Tab");
      const firstValue = reference ? await page.evaluate(() => window.monaco.editor.getEditors()[0].getValue()) : await editor().first().inputValue();
      assert.equal(firstValue, "ab      ", "disabling tab focus in another stage restores indentation here");
      assert.deepEqual(errors, []);
      console.log("synthetic translator tab focus smoke passed");
      return;
    }
    const scopedText = "😺outside cat\ninside cat and cat\noutside cat";
    if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setSelection(new window.monaco.Selection(2, 1, 2, 19)); e.focus(); }, scopedText);
    else { await editor().fill(scopedText); await editor().evaluate(input => input.setSelectionRange(14, 32)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+h");
    const scopedDialog = page.getByRole("dialog", { name: "Find / Replace" });
    const scopeButton = scopedDialog.getByRole("checkbox", { name: "Find in Selection (Alt+L)" });
    await scopeButton.click();
    assert.equal(await scopeButton.getAttribute("aria-checked"), "true");
    await scopedDialog.getByLabel("Find", { exact: true }).fill("^inside");
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await scopedDialog.getByText("1 of 1", { exact: true }).waitFor();
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await scopedDialog.getByLabel("Find", { exact: true }).fill("cat");
    await scopedDialog.getByText("1 of 2", { exact: true }).waitFor();
    const highlights = () => reference ? page.locator(".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch") : page.locator(".find-match");
    await page.waitForFunction(({ reference, count }) => { const elements = [...document.querySelectorAll(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match")]; return reference ? elements.length === count : elements.map(element => element.textContent).join("") === "cat".repeat(count); }, { reference, count: 2 });
    assert.deepEqual((await highlights().evaluateAll(elements => [...new Set(elements.map(element => getComputedStyle(element).backgroundColor))])).sort(), ["rgb(81, 92, 106)", "rgba(234, 92, 0, 0.33)"], "scoped Find highlights current and other matches");
    if (!reference) assert.deepEqual(await highlights().evaluateAll(elements => ["rgb(81, 92, 106)", "rgba(234, 92, 0, 0.33)"].map(color => elements.filter(element => getComputedStyle(element).backgroundColor === color).map(element => element.textContent).join(""))), ["cat", "cat"], "the complete matching text is highlighted");
    await scopedDialog.getByLabel("Find", { exact: true }).press("Enter");
    await scopedDialog.getByText("2 of 2", { exact: true }).waitFor();
    await page.waitForFunction(reference => {
      const elements = [...document.querySelectorAll(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match")].sort((a, b) => a.getBoundingClientRect().left - b.getBoundingClientRect().left);
      return elements.length >= 2 && getComputedStyle(elements[0]).backgroundColor === "rgba(234, 92, 0, 0.33)" && getComputedStyle(elements.at(-1)).backgroundColor === "rgb(81, 92, 106)";
    }, reference);
    await scopedDialog.getByLabel("Find", { exact: true }).press("Shift+Enter");
    await scopedDialog.getByText("1 of 2", { exact: true }).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("doggo");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "😺outside cat\ninside doggo and doggo\noutside cat", "replace all affects only the selected range");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), scopedText, "scoped replacement remains undoable");
    await scopedDialog.getByText("1 of 2", { exact: true }).waitFor();
    await scopeButton.click();
    assert.equal(await scopeButton.getAttribute("aria-checked"), "false");
    await scopedDialog.getByText(/^[1-4] of 4$/).waitFor();
    await page.waitForFunction(({ reference, count }) => { const elements = [...document.querySelectorAll(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match")]; return reference ? elements.length === count : elements.map(element => element.textContent).join("") === "cat".repeat(count); }, { reference, count: 4 });
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await scopedDialog.getByLabel("Find", { exact: true }).fill("^inside");
    await scopedDialog.getByText("1 of 1", { exact: true }).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("center");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), scopedText.replace("inside", "center"), "an anchored expression replaces a match at a later line start");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), scopedText, "anchored replacement is undoable");
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await scopedDialog.getByRole("button", { name: "Close (Escape)" }).click();
    await page.waitForFunction(reference => !document.querySelector(reference ? ".monaco-editor .view-overlays .findMatch, .monaco-editor .view-overlays .currentFindMatch" : ".find-match"), reference);
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setSelection(new window.monaco.Selection(1, 1, 2, 19)); e.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(0, 32));
    await page.keyboard.press("Control+h");
    assert.equal(await scopeButton.getAttribute("aria-checked"), "false", "Find is not automatically scoped to a multi-line selection");
    await scopedDialog.getByLabel("Find", { exact: true }).press("Alt+l");
    assert.equal(await scopeButton.getAttribute("aria-checked"), "true");
    await scopedDialog.getByLabel("Find", { exact: true }).fill("cat");
    await scopedDialog.getByText(/^[1-3] of 3$/).waitFor();
    await scopeButton.click();
    const multilingualWords = "кот котик КОТ";
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), multilingualWords);
    else await editor().fill(multilingualWords);
    await scopedDialog.getByLabel("Find", { exact: true }).fill("кот");
    await scopedDialog.getByRole("checkbox", { name: "Match Whole Word (Alt+W)" }).click();
    await scopedDialog.getByText(/^[1-2] of 2$/).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("пёс");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "пёс котик пёс", "whole-word replacement preserves a longer Cyrillic word");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), multilingualWords, "whole-word replacement is undoable");
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    await scopedDialog.getByLabel("Find", { exact: true }).fill("(кот)");
    await scopedDialog.getByText(/^[1-2] of 2$/).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("$1!");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "кот! котик КОТ!", "whole-word regex replacement uses captures only for complete words");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), multilingualWords, "whole-word regex replacement is undoable");
    await scopedDialog.getByLabel("Find", { exact: true }).fill("кот|котик");
    await scopedDialog.getByText(/^[1-2] of 2$/).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("x");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "x котик x", "an invalid earlier alternative cannot turn into a different whole-word match");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), multilingualWords);
    await scopedDialog.getByLabel("Find", { exact: true }).fill("(кот)");
    await scopedDialog.getByText(/^[1-2] of 2$/).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("\\u$1$$:$0");
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "Кот$:кот котик КОТ$:КОТ", "regex replacement applies capture case, literal dollar and full-match substitution");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), multilingualWords);
    await scopedDialog.getByRole("checkbox", { name: "Match Whole Word (Alt+W)" }).click();
    await scopedDialog.getByText(/^[1-3] of 3$/).waitFor();
    await scopedDialog.getByRole("button", { name: /^Replace All \(/ }).click();
    assert.equal(await value(), "Кот$:кот Кот$:котик КОТ$:КОТ", "regex replacement syntax also applies without whole-word mode");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), multilingualWords);

    const contextualText = "precat cat";
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), contextualText);
    else await editor().fill(contextualText);
    await scopedDialog.getByLabel("Find", { exact: true }).fill("(?<=pre)(cat)");
    await scopedDialog.getByText("1 of 1", { exact: true }).waitFor();
    await scopedDialog.getByLabel("Replace", { exact: true }).fill("$1!");
    await scopedDialog.getByRole("button", { name: /^Replace \(/ }).click();
    assert.equal(await value(), "precat! cat", "single replacement retains the regex match's surrounding context");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].focus()); else await editor().focus();
    await page.keyboard.press("Control+z");
    assert.equal(await value(), contextualText, "contextual replacement is undoable");
    await scopedDialog.getByRole("checkbox", { name: "Use Regular Expression (Alt+R)" }).click();
    if (process.env.TRANSLATOR_FIND_SELECTION_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator scoped find smoke passed");
      return;
    }
    await scopedDialog.getByRole("button", { name: "Close (Escape)" }).click();
    const lines = "alpha\nbeta\ngamma";
    const lineSelection = () => reference ? page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; const s = editor.getSelection(); return [editor.getModel().getOffsetAt(s.getStartPosition()), editor.getModel().getOffsetAt(s.getEndPosition())]; }) : editor().evaluate(input => [input.selectionStart, input.selectionEnd]);
    const indentedLine = "alpha\n    beta\ngamma";
    if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition({ lineNumber: 2, column: 9 }); e.focus(); }, indentedLine);
    else { await editor().fill(indentedLine); await editor().evaluate(input => input.setSelectionRange(14, 14)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Home");
    assert.deepEqual(await lineSelection(), [10, 10], "Home moves to the first non-whitespace character");
    await page.keyboard.press("Home");
    assert.deepEqual(await lineSelection(), [6, 6], "a second Home moves to the beginning of the line");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setPosition({ lineNumber: 2, column: 9 }));
    else await editor().evaluate(input => input.setSelectionRange(14, 14));
    await page.keyboard.press("Shift+Home");
    assert.deepEqual(await lineSelection(), [10, 14], "Shift+Home selects back to the first non-whitespace character");
    await page.keyboard.press("Shift+Home");
    assert.deepEqual(await lineSelection(), [6, 14], "repeated Shift+Home extends the backward selection to the line start");
    const viewport = page.viewportSize();
    const wrappedHome = "  " + "x".repeat(200);
    await page.setViewportSize({ width: 390, height: 844 });
    if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition({ lineNumber: 1, column: 71 }); e.focus(); }, wrappedHome);
    else { await editor().fill(wrappedHome); await editor().evaluate(input => input.setSelectionRange(70, 70)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Home");
    assert.deepEqual(await lineSelection(), [62, 62], "Home first moves to the beginning of a wrapped visual line");
    await page.keyboard.press("Home");
    assert.deepEqual(await lineSelection(), [2, 2], "a second Home moves to the logical line's first non-whitespace character");
    await page.setViewportSize(viewport);
    if (process.env.TRANSLATOR_HOME_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator Home smoke passed");
      return;
    }
    if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition({ lineNumber: 2, column: 3 }); e.focus(); }, lines);
    else { await editor().fill(lines); await editor().evaluate(input => input.setSelectionRange(8, 8)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Alt+ArrowDown");
    assert.equal(await value(), "alpha\ngamma\nbeta", "Alt+Down moves the current line down");
    assert.deepEqual(await lineSelection(), [14, 14], "moving a line preserves the cursor column");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "moving a line down is undoable");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setPosition({ lineNumber: 2, column: 3 }));
    else await editor().evaluate(input => input.setSelectionRange(8, 8));
    await page.keyboard.press("Alt+ArrowUp");
    assert.equal(await value(), "beta\nalpha\ngamma", "Alt+Up moves the current line up");
    assert.deepEqual(await lineSelection(), [2, 2], "moving up preserves the cursor column");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "moving a line up is undoable");
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setSelection(new window.monaco.Selection(1, 1, 2, 1)); e.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(0, 6));
    await page.keyboard.press("Alt+ArrowDown");
    assert.equal(await value(), "beta\nalpha\ngamma", "a selection ending at the next line moves only the selected line");
    assert.deepEqual(await lineSelection(), [5, 11], "moving selected lines preserves the selection");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "moving selected lines is undoable");
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setSelection(new window.monaco.Selection(2, 1, 3, 1)); e.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(6, 11));
    await page.keyboard.press("Alt+ArrowDown");
    assert.equal(await value(), lines, "a selection reaching the last line cannot move down");
    assert.deepEqual(await lineSelection(), [6, 11], "an unmoved selection is retained");
    if (process.env.TRANSLATOR_MOVE_LINES_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator move lines smoke passed");
      return;
    }
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setPosition({ lineNumber: 2, column: 3 }));
    else await editor().evaluate(input => input.setSelectionRange(8, 8));
    await page.keyboard.press("Control+Alt+Shift+ArrowDown");
    assert.equal(await value(), "alpha\nbeta\nbeta\ngamma", "Copy Line Down duplicates the current line");
    assert.deepEqual(await lineSelection(), [13, 13], "copying down moves the caret with its line");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "copying a line down is undoable");
    if (reference) await page.evaluate(() => window.monaco.editor.getEditors()[0].setPosition({ lineNumber: 2, column: 3 }));
    else await editor().evaluate(input => input.setSelectionRange(8, 8));
    await page.keyboard.press("Control+Alt+Shift+ArrowUp");
    assert.equal(await value(), "alpha\nbeta\nbeta\ngamma", "Copy Line Up duplicates the current line");
    assert.deepEqual(await lineSelection(), [8, 8], "copying up keeps the caret in its line");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "copying a line up is undoable");
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setSelection(new window.monaco.Selection(2, 1, 3, 1)); e.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(6, 11));
    await page.keyboard.press("Control+Alt+Shift+ArrowDown");
    assert.equal(await value(), "alpha\nbeta\nbeta\ngamma", "a selection ending at the next line copies only the selected line");
    assert.deepEqual(await lineSelection(), [11, 16], "copying selected lines keeps the selection");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "copying selected lines is undoable");
    if (process.env.TRANSLATOR_COPY_LINES_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator copy lines smoke passed");
      return;
    }
    const cdp = await context.newCDPSession(page);
    for (const { text, start, end } of [
      { text: '{"text":""}', start: 9, end: 9 },
      { text: '{"text":"hello"}', start: 10, end: 13 },
    ]) {
      if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), text);
      else await editor().fill(text);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      if (reference) await page.evaluate(({ start, end }) => { const e = window.monaco.editor.getEditors()[0], m = e.getModel(); const a = m.getPositionAt(start), b = m.getPositionAt(end); e.setSelection(new window.monaco.Selection(a.lineNumber, a.column, b.lineNumber, b.column)); e.focus(); }, { start, end });
      else await editor().evaluate((input, { start, end }) => { input.setSelectionRange(start, end); input.focus(); }, { start, end });
      for (const composed of ["n", "に", "日本"]) {
        await cdp.send("Input.imeSetComposition", { text: composed, selectionStart: composed.length, selectionEnd: composed.length });
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        assert.equal(await value(), text.slice(0, start) + composed + text.slice(end), "composition updates the selected text");
      }
      await cdp.send("Input.insertText", { text: "日本" });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await page.keyboard.press("Control+z");
      assert.equal(await value(), text, "undo restores text before IME composition");
      assert.deepEqual(await selection(), [start, end], "undo restores the original composition selection");
      await page.keyboard.press("Control+Shift+z");
      assert.equal(await value(), text.slice(0, start) + "日本" + text.slice(end), "redo restores the composed text");
      await page.keyboard.press("Control+z");
      assert.deepEqual(await selection(), [start, end], "undo after redo restores the composition selection");
      await page.keyboard.type("x");
      assert.equal(await value(), text.slice(0, start) + "x" + text.slice(end), "typing after undo preserves surrounding text");
    }
    const compositionBase = '{"text":""}';
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), compositionBase);
    else await editor().fill(compositionBase);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setPosition({ lineNumber: 1, column: 10 }); e.focus(); });
    else await editor().evaluate(input => { input.setSelectionRange(9, 9); input.focus(); });
    for (const composed of ["猫", "犬"]) {
      await cdp.send("Input.imeSetComposition", { text: composed, selectionStart: composed.length, selectionEnd: composed.length });
      await cdp.send("Input.insertText", { text: composed });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    }
    assert.equal(await value(), '{"text":"猫犬"}', "consecutive IME compositions are committed");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), compositionBase, "one undo removes consecutive IME compositions");
    assert.deepEqual(await selection(), [9, 9], "grouped IME undo restores the initial cursor");
    await page.keyboard.press("Control+Shift+z");
    assert.equal(await value(), '{"text":"猫犬"}', "one redo restores the composition group");
    await cdp.send("Input.imeSetComposition", { text: "鳥", selectionStart: 1, selectionEnd: 1 });
    await cdp.send("Input.insertText", { text: "鳥" });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+z");
    assert.equal(await value(), '{"text":"猫犬"}', "a composition after redo is a separate undo group");
    for (const { arrows, composed, cursor } of [
      { arrows: ["ArrowLeft"], composed: '{"text":"犬猫"}', cursor: 9 },
      { arrows: ["ArrowLeft", "ArrowRight"], composed: '{"text":"猫犬"}', cursor: 10 },
      { arrows: null, composed: '{"text":"犬猫"}', cursor: 9 },
    ]) {
      if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), compositionBase);
      else await editor().fill(compositionBase);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setPosition({ lineNumber: 1, column: 10 }); e.focus(); });
      else await editor().evaluate(input => { input.setSelectionRange(9, 9); input.focus(); });
      await cdp.send("Input.imeSetComposition", { text: "猫", selectionStart: 1, selectionEnd: 1 });
      await cdp.send("Input.insertText", { text: "猫" });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      if (arrows) {
        for (const arrow of arrows) await page.keyboard.press(arrow);
      } else {
        const point = await page.evaluate(reference => {
          if (reference) {
            const editor = window.monaco.editor.getEditors()[0];
            const rect = editor.getDomNode().getBoundingClientRect();
            const position = editor.getScrolledVisiblePosition({ lineNumber: 1, column: 10 });
            return { x: rect.left + position.left + 1, y: rect.top + position.top + position.height / 2 };
          }
          const input = document.querySelector('textarea[aria-label="Translator editor"]');
          const rect = input.getBoundingClientRect(), style = getComputedStyle(input);
          const canvas = document.createElement("canvas"), context = canvas.getContext("2d");
          context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
          return { x: rect.left + parseFloat(style.paddingLeft) + context.measureText('{"text":"').width + 1, y: rect.top + 8 };
        }, reference);
        await page.mouse.click(point.x, point.y);
      }
      assert.deepEqual(await selection(), [cursor, cursor], "navigation places the IME cursor");
      await cdp.send("Input.imeSetComposition", { text: "犬", selectionStart: 1, selectionEnd: 1 });
      await cdp.send("Input.insertText", { text: "犬" });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await value(), composed);
      await page.keyboard.press("Control+z");
      assert.equal(await value(), '{"text":"猫"}', "moving the cursor separates IME undo groups");
      assert.deepEqual(await selection(), [cursor, cursor], "undo restores the cursor after navigation");
      await page.keyboard.press("Control+Shift+z");
      assert.equal(await value(), composed, "redo restores the later IME composition");
    }
    if (process.env.TRANSLATOR_IME_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator IME smoke passed");
      return;
    }
    if (reference) {
      await page.locator(".monaco-editor .view-lines").waitFor();
      await page.evaluate(() => window.monaco.editor.getEditors()[0].setValue("text"));
    } else {
      await editor().fill("text");
    }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setSelection(new window.monaco.Selection(1, 1, 1, 5)); editor.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(0, input.value.length));
    await page.keyboard.type("{");
    assert.equal(await value(), "{text}", "an opening brace surrounds the selected JSON text");
    assert.deepEqual(await selection(), [1, 5], "the surrounded text remains selected");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "text", "surrounding a selection is one undoable edit");
    if (process.env.TRANSLATOR_SELECTION_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator selection smoke passed");
      return;
    }
    if (reference) await page.evaluate(text => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(text); editor.setPosition({ lineNumber: 2, column: 3 }); editor.focus(); }, lines);
    else { await editor().fill(lines); await editor().evaluate(input => input.setSelectionRange(8, 8)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+l");
    assert.deepEqual(await lineSelection(), [6, 11], "Ctrl+L selects the current logical line including its newline");
    await page.keyboard.press("Control+l");
    assert.deepEqual(await lineSelection(), [6, 16], "repeating Ctrl+L extends the selection to the next line");
    if (process.env.TRANSLATOR_LINE_SELECTION_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator line selection smoke passed");
      return;
    }
    if (reference) await page.evaluate(text => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(text); editor.setPosition({ lineNumber: 2, column: 3 }); editor.focus(); }, lines);
    else { await editor().fill(lines); await editor().evaluate(input => input.setSelectionRange(8, 8)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+Shift+k");
    assert.equal(await value(), "alpha\ngamma", "Ctrl+Shift+K deletes the current line");
    assert.deepEqual(await lineSelection(), [8, 8], "deleting a line keeps the cursor column");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "line deletion is one undoable edit");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setPosition({ lineNumber: 3, column: 3 }); editor.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(13, 13));
    await page.keyboard.press("Control+Shift+k");
    assert.equal(await value(), "alpha\nbeta", "deleting the final line also removes its preceding newline");
    assert.deepEqual(await lineSelection(), [8, 8], "the cursor moves to the previous line at the same column");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "final-line deletion is undoable");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setSelection(new window.monaco.Selection(1, 1, 2, 1)); editor.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(0, 6));
    await page.keyboard.press("Control+Shift+k");
    assert.equal(await value(), "beta\ngamma", "a selection ending at the next line does not delete that line");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), lines, "deleting selected lines is undoable");
    if (process.env.TRANSLATOR_DELETE_LINE_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator line deletion smoke passed");
      return;
    }
    const uncommented = '{\n  "name": 1\n}';
    if (reference) await page.evaluate(text => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(text); editor.setPosition({ lineNumber: 2, column: 5 }); editor.focus(); }, uncommented);
    else { await editor().fill(uncommented); await editor().evaluate(input => input.setSelectionRange(6, 6)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+/");
    assert.equal(await value(), '{\n//   "name": 1\n}', "Ctrl+/ comments an indented JSON line");
    assert.deepEqual(await lineSelection(), [9, 9], "commenting keeps the cursor within the original text");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), uncommented, "commenting a line is one undoable edit");
    if (reference) await page.evaluate(() => { const editor = window.monaco.editor.getEditors()[0]; editor.setPosition({ lineNumber: 2, column: 1 }); editor.focus(); });
    else await editor().evaluate(input => input.setSelectionRange(2, 2));
    await page.keyboard.press("Control+/");
    assert.deepEqual(await lineSelection(), [5, 5], "a cursor at the insertion point follows the line comment");
    await page.keyboard.press("Control+z");
    const multipleLines = '{\n  "first": 1,\n\n    "second": 2\n}';
    const lastLineStart = multipleLines.indexOf("\n}") + 1;
    if (reference) await page.evaluate(({ text, end }) => { const editor = window.monaco.editor.getEditors()[0], model = editor.getModel(); editor.setValue(text); const startPosition = model.getPositionAt(4), endPosition = model.getPositionAt(end); editor.setSelection(new window.monaco.Selection(startPosition.lineNumber, startPosition.column, endPosition.lineNumber, endPosition.column)); editor.focus(); }, { text: multipleLines, end: lastLineStart });
    else { await editor().fill(multipleLines); await editor().evaluate((input, end) => input.setSelectionRange(4, end), lastLineStart); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+/");
    const commentedLines = '{\n//   "first": 1,\n\n//     "second": 2\n}';
    assert.equal(await value(), commentedLines, "line comments skip empty lines and a selection ending at the next line");
    assert.deepEqual(await lineSelection(), [7, lastLineStart + 6], "commenting preserves the selected text");
    await page.keyboard.press("Control+/");
    assert.equal(await value(), multipleLines, "Ctrl+/ removes existing line comments");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), commentedLines, "uncommenting is one undoable edit");
    const nestedComment = '{\n  "nested": {\n    "inside": true\n  },\n  "visible": 2\n}';
    if (reference) await page.evaluate(text => { const e = window.monaco.editor.getEditors()[0]; e.setValue(text); e.setPosition({ lineNumber: 2, column: 3 }); e.focus(); }, nestedComment);
    else { await editor().fill(nestedComment); await editor().evaluate(input => input.setSelectionRange(4, 4)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+/");
    assert.equal(await value(), '{\n//   "nested": {\n    "inside": true\n  },\n  "visible": 2\n}');
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const colors = await page.evaluate(reference => {
      const lines = reference
        ? [...document.querySelectorAll(".monaco-editor .view-lines .view-line")]
        : [...document.querySelector('textarea[aria-label="Translator editor"]').parentElement.querySelector("pre").children];
      const color = (line, value) => {
        const span = [...line.querySelectorAll("span")].find(el => !el.children.length && el.textContent.includes(value));
        return span && getComputedStyle(span).color;
      };
      return [color(lines.find(line => line.textContent.includes('"nested"')), '"nested"'), color(lines.find(line => line.textContent.trim() === "},"), "}")];
    }, reference);
    assert.deepEqual(colors, ["rgb(96, 139, 78)", "rgb(255, 215, 0)"], "a commented JSON line has comment color and does not change subsequent bracket depth");
    const blockComment = '{\n  /* { note\n     note */\n  "visible": {}\n}';
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), blockComment);
    else await editor().fill(blockComment);
    await page.waitForFunction(reference => {
      const root = reference ? document.querySelector(".monaco-editor .view-lines") : document.querySelector('textarea[aria-label="Translator editor"]')?.parentElement.querySelector("pre");
      return root?.textContent.includes("visible") && root.textContent.includes("note");
    }, reference);
    const blockColors = await page.evaluate(reference => {
      const lines = reference
        ? [...document.querySelectorAll(".monaco-editor .view-lines .view-line")]
        : [...document.querySelector('textarea[aria-label="Translator editor"]').parentElement.querySelector("pre").children];
      const content = element => element.textContent.replaceAll("\u00a0", " ");
      const color = (line, value) => {
        const span = [...line.querySelectorAll("span")].find(el => !el.children.length && content(el).includes(value));
        return span && getComputedStyle(span).color;
      };
      return [color(lines.find(line => content(line).includes("{ note")), "{ note"), color(lines.find(line => content(line).includes("note */")), "note */"), color(lines.find(line => content(line).includes('"visible"')), "{")];
    }, reference);
    assert.deepEqual(blockColors, ["rgb(96, 139, 78)", "rgb(96, 139, 78)", "rgb(218, 112, 214)"], "a multiline JSON comment keeps its color and does not change later bracket depth");
    if (process.env.TRANSLATOR_COMMENT_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator line comment smoke passed");
      return;
    }
    const blockText = '{"name": 1}';
    const blockEnd = blockText.length - 1;
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), blockText);
    else await editor().fill(blockText);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (reference) await page.evaluate(end => { const e = window.monaco.editor.getEditors()[0]; e.setSelection(new window.monaco.Selection(1, 2, 1, end + 1)); e.focus(); }, blockEnd);
    else await editor().evaluate((input, end) => input.setSelectionRange(1, end), blockEnd);
    await page.keyboard.press("Control+Shift+a");
    assert.equal(await value(), '{/* "name": 1 */}', "Ctrl+Shift+A wraps selected JSON in a block comment");
    assert.deepEqual(await lineSelection(), [4, blockEnd + 3], "block commenting retains the selected text");
    await page.keyboard.press("Control+Shift+a");
    assert.equal(await value(), blockText, "the shortcut removes the surrounding block comment");
    assert.deepEqual(await lineSelection(), [1, blockEnd], "removing a block comment retains the selection");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), '{/* "name": 1 */}', "removing a block comment is undoable");
    if (reference) await page.evaluate(() => { const e = window.monaco.editor.getEditors()[0]; e.setValue("[]"); e.setPosition({ lineNumber: 1, column: 2 }); e.focus(); });
    else { await editor().fill("[]"); await editor().evaluate(input => input.setSelectionRange(1, 1)); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Control+Shift+a");
    assert.equal(await value(), "[/*  */]", "block commenting at a caret inserts an empty comment");
    assert.deepEqual(await lineSelection(), [4, 4], "the caret remains inside the empty comment");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), "[]", "inserting an empty block comment is undoable");
    const multiBlock = '{\n  "first": 1,\n  "second": 2\n}';
    const multiEnd = multiBlock.indexOf("\n}");
    if (reference) await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), multiBlock);
    else await editor().fill(multiBlock);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (reference) await page.evaluate(end => { const e = window.monaco.editor.getEditors()[0], m = e.getModel(); const a = m.getPositionAt(2), b = m.getPositionAt(end); e.setSelection(new window.monaco.Selection(a.lineNumber, a.column, b.lineNumber, b.column)); e.focus(); }, multiEnd);
    else await editor().evaluate((input, end) => input.setSelectionRange(2, end), multiEnd);
    await page.keyboard.press("Control+Shift+a");
    const multiCommented = multiBlock.slice(0, 2) + "/* " + multiBlock.slice(2, multiEnd) + " */" + multiBlock.slice(multiEnd);
    assert.equal(await value(), multiCommented, "a block comment can span selected lines");
    await page.keyboard.press("Control+Shift+a");
    assert.equal(await value(), multiBlock, "a multiline block comment is removable");
    assert.deepEqual(await lineSelection(), [2, multiEnd], "removing a multiline comment retains the selection");
    if (process.env.TRANSLATOR_BLOCK_COMMENT_ONLY === "1") {
      assert.deepEqual(errors, []);
      console.log("synthetic translator block comment smoke passed");
      return;
    }
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
    const indentedJson = '{\n  "name": "value",\n}';
    const lineEnd = indentedJson.indexOf("\n}");
    if (reference) await page.evaluate(text => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(text); editor.setPosition({ lineNumber: 2, column: text.split("\n")[1].length + 1 }); editor.focus(); }, indentedJson);
    else { await editor().fill(indentedJson); await editor().evaluate((input, position) => input.setSelectionRange(position, position), lineEnd); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Enter");
    assert.equal(await value(), '{\n  "name": "value",\n  \n}', "Enter preserves the indentation of a JSON property");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), indentedJson, "newline indentation is one undoable edit");
    const nestedJson = '{\n  "outer": {}\n}';
    const nestedCursor = nestedJson.indexOf("{}") + 1;
    if (reference) await page.evaluate(({ text, cursor }) => { const editor = window.monaco.editor.getEditors()[0]; editor.setValue(text); editor.setPosition(editor.getModel().getPositionAt(cursor)); editor.focus(); }, { text: nestedJson, cursor: nestedCursor });
    else { await editor().fill(nestedJson); await editor().evaluate((input, position) => input.setSelectionRange(position, position), nestedCursor); }
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.keyboard.press("Enter");
    assert.equal(await value(), '{\n  "outer": {\n    \n  }\n}', "Enter indents nested JSON brackets to the next tab stop");
    await page.keyboard.press("Control+z");
    assert.equal(await value(), nestedJson, "nested bracket indentation is one undoable edit");
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

    const wrappedJson = `{\n  "message": "${"x".repeat(200)}",\n  "count": 42\n}`;
    if (reference) {
      await settleReferenceJson();
      await page.evaluate(text => window.monaco.editor.getEditors()[0].setValue(text), wrappedJson);
    } else await editor().fill(wrappedJson);
    const wrappedLayout = ({ reference, minGap }) => {
      let numbers, text;
      if (reference) {
        const root = document.querySelector(".monaco-editor");
        numbers = root && [...root.querySelectorAll(".margin-view-overlays .line-numbers")];
        text = root && [...root.querySelectorAll(".view-lines .view-line")].find(line => line.textContent.includes("count"));
      } else {
        const input = document.querySelector('textarea[aria-label="Translator editor"]');
        numbers = input && [...input.parentElement.previousElementSibling.children];
        text = input && [...input.parentElement.querySelector("pre").children][2];
      }
      const second = numbers?.find(line => line.textContent.trim() === "2");
      const third = numbers?.find(line => line.textContent.trim() === "3");
      if (!second || !third || !text) return null;
      const gap = third.getBoundingClientRect().top - second.getBoundingClientRect().top;
      const alignment = third.getBoundingClientRect().top - text.getBoundingClientRect().top;
      return gap > minGap && Math.abs(alignment) < 1 ? { gap, alignment } : null;
    };
    const desktopLayout = await page.waitForFunction(wrappedLayout, { reference, minGap: 16 });
    const desktopGap = (await desktopLayout.jsonValue()).gap;
    await page.setViewportSize({ width: 390, height: 844 });
    await page.waitForFunction(wrappedLayout, { reference, minGap: desktopGap });
    await (reference ? page.locator(".monaco-editor") : editor()).scrollIntoViewIfNeeded();
    const point = await page.evaluate(reference => {
      const root = reference ? document.querySelector(".monaco-editor") : document.querySelector('textarea[aria-label="Translator editor"]').parentElement;
      const numbers = reference ? [...root.querySelectorAll(".margin-view-overlays .line-numbers")] : [...root.previousElementSibling.children];
      const second = numbers.find(line => line.textContent.trim() === "2");
      const text = reference ? root.querySelector(".view-lines") : root.querySelector("pre");
      return { x: text.getBoundingClientRect().left + 29, y: second.getBoundingClientRect().top + 24 };
    }, reference);
    await page.mouse.click(point.x, point.y);
    const expectedOffset = wrappedJson.indexOf('"' + "x".repeat(200)) + 2;
    if (reference) {
      const position = await page.evaluate(() => window.monaco.editor.getEditors()[0].getPosition());
      assert.deepEqual([position.lineNumber, position.column], [2, expectedOffset - wrappedJson.indexOf("\n")]);
    } else {
      assert.deepEqual(await selection(), [expectedOffset, expectedOffset], "clicking a wrapped row positions the caret in the underlying text");
    }
    assert.deepEqual(errors, []);
    console.log("synthetic translator flow passed");
  } finally { await browser.close(); server.close(); }
}

main().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
