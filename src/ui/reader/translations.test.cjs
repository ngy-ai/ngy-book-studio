// Optional DOM regression gate: node --test src/ui/reader/translations.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const translationsSource = fs.readFileSync(path.join(__dirname, "translations.js"), "utf8");
const annotationsSource = fs.readFileSync(path.join(__dirname, "annotations.js"), "utf8");
const readerSource = fs.readFileSync(path.join(__dirname, "..", "reader.rs"), "utf8");
const selectionBridgeMatch = readerSource.match(/const READER_INITIALIZATION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
assert.ok(selectionBridgeMatch, "The DOM gate must load the product's actual reader selection bridge");
const selectionBridge = selectionBridgeMatch[1];

const fixture = `<!doctype html><html><head><style>
  body{font:22px/1.8 Arial;margin:50px;max-width:760px}p{margin:15px 0}
</style></head><body>
<h1 id="title">无译文标题</h1>
<p id="a">Alpha</p>
<p id="b">Alpha</p>
<p id="c">Beta</p>
<ul><li id="li"><p id="nested">Nested item</p></li></ul>
<blockquote id="quote">Quoted</blockquote>
<table><tbody><tr><td id="cell">Cell</td></tr></tbody></table>
<p id="after">Tail paragraph</p>
</body></html>`;

const payload = {
  session: "translation-session",
  revision: 1,
  blocks: [
    { key: "a", source: "Alpha", translated: "甲一" },
    { key: "b", source: "Alpha", translated: "甲二" },
    { key: "c", source: "Beta", translated: "乙" },
    { key: "nested", source: "Nested item", translated: "嵌套" },
    { key: "quote", source: "Quoted", translated: "引用" },
    { key: "cell", source: "Cell", translated: "单元格" },
    { key: "after", source: "Tail paragraph", translated: "尾段" },
  ],
};

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.MOYE_TEST_CHROMIUM ? { executablePath: process.env.MOYE_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function pageWithFixture() {
  const page = await browser.newPage({ viewport: { width: 1150, height: 900 } });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200, contentType: "text/html; charset=utf-8", body: fixture,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    const originalAttach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function(options) {
      const result = originalAttach.call(this, options);
      if (this.localName === 'moye-reader-notes') window.__notesRoot = result;
      return result;
    };
    ${selectionBridge}
    ${annotationsSource}
    ${translationsSource}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.evaluate(() => window.moyeAnnotations.configure({ session: "chapter-session", revision: 1, notes: [] }));
  await page.waitForFunction(() => !!window.__notesRoot);
  return page;
}

async function configure(page) {
  await page.evaluate((value) => window.moyeTranslations.configure(value), payload);
}

function translationText(page, id) {
  return page.evaluate((id) => {
    const node = document.getElementById(id).previousElementSibling;
    return node && node.hasAttribute("data-moye-translation")
      ? node.querySelector(".moye-translation-text").textContent
      : null;
  }, id);
}

test("translated blocks render bilingual above each matched paragraph", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    const state = await page.evaluate(() => {
      const titleSibling = document.getElementById("title").previousElementSibling;
      return {
        count: document.querySelectorAll("[data-moye-translation]").length,
        titleHasMark: !!(titleSibling && titleSibling.hasAttribute("data-moye-translation")),
        listItemHasMark: !!document.getElementById("li").querySelector("[data-moye-translation]"),
        cellFirstIsMark: !!document.getElementById("cell").firstElementChild
          && document.getElementById("cell").firstElementChild.hasAttribute("data-moye-translation"),
      };
    });
    assert.equal(state.count, 7, "one translation layer per matched block");
    assert.equal(await translationText(page, "a"), "甲一");
    assert.equal(await translationText(page, "b"), "甲二", "duplicate source text is disambiguated by document order");
    assert.equal(await translationText(page, "c"), "乙");
    assert.equal(await translationText(page, "nested"), "嵌套", "the inner paragraph, not its list item, is translated");
    assert.equal(await translationText(page, "quote"), "引用");
    assert.equal(await translationText(page, "after"), "尾段");
    assert.equal(state.titleHasMark, false, "blocks without a translation keep the original only");
    assert.equal(state.listItemHasMark, false, "a list item that only wraps a paragraph is not matched twice");
    assert.equal(state.cellFirstIsMark, true, "table cells receive the translation inside the cell");
  } finally { await page.close(); }
});

test("clicking a translation toggles only that paragraph between bilingual and translation-only", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    const point = await page.evaluate(() => {
      const text = document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-text");
      const rect = text.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    });
    await page.mouse.click(point.x, point.y);
    const collapsed = await page.evaluate(() => ({
      display: document.getElementById("a").style.display,
      divider: document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-divider").style.display,
      collapsed: document.getElementById("a").previousElementSibling
        .getAttribute("data-moye-collapsed"),
    }));
    assert.equal(collapsed.display, "none", "translation-only mode hides the original");
    assert.equal(collapsed.divider, "none");
    assert.equal(collapsed.collapsed, "1");

    await page.mouse.click(point.x, point.y);
    const restored = await page.evaluate(() => ({
      display: document.getElementById("a").style.display,
      divider: document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-divider").style.display,
      neighbour: document.getElementById("b").style.display,
    }));
    assert.equal(restored.display, "", "bilingual mode restores the original");
    assert.notEqual(restored.divider, "none");
    assert.equal(restored.neighbour, "", "the neighbouring paragraph is unaffected");
  } finally { await page.close(); }
});

test("translation nodes never become note text or reported selections", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);

    // A selection spanning two originals must exclude the translation between them.
    await page.evaluate(() => {
      const first = document.getElementById("a");
      const last = document.getElementById("b");
      const range = document.createRange();
      range.setStart(first.firstChild, 0);
      range.setEnd(last.firstChild, last.firstChild.length);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
    });
    await page.waitForTimeout(160);
    const reported = await page.evaluate(() =>
      window.__messages.filter((message) => message.type === "selection_changed").at(-1)?.selected_text);
    assert.equal(reported, "AlphaAlpha", "the selection bridge excludes translation text");

    // The notes toolbar anchors the same range to original book text only.
    const noteQuote = await page.evaluate(async () => {
      const first = document.getElementById("a");
      const last = document.getElementById("b");
      const range = document.createRange();
      range.setStart(first.firstChild, 0);
      range.setEnd(last.firstChild, last.firstChild.length);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      for (let i = 0; i < 50 && toolbar.hidden; i++) {
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      if (toolbar.hidden) return null;
      window.__notesRoot.querySelector('[data-action="highlight"]').click();
      return new Promise((resolve) => setTimeout(() => {
        const message = window.__messages.filter((item) => item.action === "highlight").at(-1);
        resolve(message ? message.anchor.quote : null);
      }, 80));
    });
    assert.equal(noteQuote, "AlphaAlpha", "annotation anchors exclude translation text");

    // Selecting only a translation offers no note at all.
    const hidden = await page.evaluate(async () => {
      const text = document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-text");
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(text);
      window.getSelection().addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      for (let i = 0; i < 50 && !toolbar.hidden; i++) {
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      return toolbar.hidden;
    });
    assert.equal(hidden, true, "translation text is not annotatable");
  } finally { await page.close(); }
});
