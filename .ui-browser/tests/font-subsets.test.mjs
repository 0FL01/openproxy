import assert from 'node:assert/strict';
import { test } from 'node:test';
import { chromium } from '/opt/ui-browser/node_modules/playwright/index.mjs';

test('fixture serves actual Latin and Cyrillic Inter glyphs offline', { timeout: 30000 }, async () => {
  const browser = await chromium.launch({
    executablePath: '/opt/chrome-linux64/chrome',
    chromiumSandbox: true,
    headless: true,
  });
  try {
    const context = await browser.newContext({ serviceWorkers: 'block' });
    const blocked = [];
    await context.route('**/*', (route) => {
      const url = new URL(route.request().url());
      if (url.origin === 'https://app:4623') return route.continue();
      blocked.push(url.origin);
      return route.abort('blockedbyclient');
    });
    const page = await context.newPage();
    const responses = new Map();
    page.on('response', (response) => responses.set(new URL(response.url()).pathname, response.status()));
    await page.goto('https://app:4623/login');
    await page.setContent(`
      <link rel="stylesheet" href="/_astro/__ui_browser/fonts.css">
      <span id="latin" style="font:400 24px Inter,sans-serif">A</span>
      <span id="cyrillic" style="font:400 24px Inter,sans-serif">Ж</span>
    `);
    await page.evaluate(() => document.fonts.ready);
    for (const file of ['inter.woff2', 'inter-cyrillic.woff2']) {
      assert.equal(responses.get(`/_astro/__ui_browser/fonts/${file}`), 200);
    }
    const cdp = await context.newCDPSession(page);
    await cdp.send('DOM.enable');
    await cdp.send('CSS.enable');
    const { root } = await cdp.send('DOM.getDocument');
    for (const selector of ['#latin', '#cyrillic']) {
      const { nodeId } = await cdp.send('DOM.querySelector', { nodeId: root.nodeId, selector });
      const { fonts } = await cdp.send('CSS.getPlatformFontsForNode', { nodeId });
      assert(fonts.some((font) => font.familyName === 'Inter' && font.isCustomFont && font.glyphCount > 0));
    }
    assert.deepEqual(blocked, []);
    await context.close();
  } finally {
    await browser.close();
  }
});
