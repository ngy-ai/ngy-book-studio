// Optional DOM regression gate: node --test src/ui/pdf_reader/annotations.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
//
// The product renders several PDF.js text layers at once now, so this gate drives
// the bridge with synthetic pages: every mounted page keeps its own notes and
// marks, a click resolves the page that owns the mark, and a draft pins the page
// it was authored on.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const source = fs.readFileSync(path.join(__dirname, "annotations.js"), "utf8");

const line = (left, top, text) =>
  `<span style="left:${left}px;top:${top}px">${text}</span>`;
const pageSection = (page, lines) =>
  `<section class="pdf-page" data-page="${page}" data-rendered="true">` +
  `<canvas class="page-canvas"></canvas>` +
  `<div class="textLayer" aria-label="第 ${page} 页文本">${lines}</div></section>`;

const fixture = `<!doctype html><html><head><style>
  :root{background:#242424}
  body{margin:0;font:16px/1.5 Arial}
  .pdf-page{position:relative;width:420px;height:420px;margin:0 auto 18px;background:#fff;overflow:hidden}
  .textLayer{position:absolute;inset:0}
  .textLayer span{position:absolute;color:transparent;white-space:pre}
  .page-canvas{position:absolute;inset:0;background:#fff}
</style></head><body>
<main id="pages">
${pageSection(1, line(20, 40, "第一页第一行") + line(20, 80, "第一页第二行"))}
${pageSection(2, line(20, 40, "第二页第一行") + line(20, 80, "第二页第二行"))}
${pageSection(3, line(20, 40, "第三页正文"))}
</main>
</body></html>`;

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.MOYE_TEST_CHROMIUM ? { executablePath: process.env.MOYE_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function pageWithFixture() {
  const page = await browser.newPage({ viewport: { width: 1000, height: 1200 } });
  await page.route("http://moyepdf.viewer/**", (route) => route.fulfill({
    status: 200, contentType: "text/html; charset=utf-8", body: fixture,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    const originalAttach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function(options) {
      const result = originalAttach.call(this, options);
      if (this.localName === "moye-reader-notes") window.__notesRoot = result;
      return result;
    };
    ${source}
  ` });
  await page.goto("http://moyepdf.viewer/viewer.html");
  await page.waitForFunction(() => !!window.__notesRoot);
  const configured = await page.evaluate(() => window.moyeAnnotations.configure({
    session: "doc-session", revision: 0, notes_enabled: true,
  }));
  assert.equal(configured, true);
  await page.evaluate(() => window.moyeAnnotations.setCurrentPage(1));
  return page;
}

async function messages(page, action) {
  return page.evaluate((action) => window.__messages.filter((message) => message.action === action), action);
}

async function waitForMessage(page, action) {
  await page.waitForFunction((action) => window.__messages.some((message) => message.action === action), action);
  return (await messages(page, action)).at(-1);
}

// Declares the mounted window and answers the page listing like the host does.
async function loadPages(page, notesByPage) {
  await page.evaluate(() => window.moyeAnnotations.setPages([1, 2, 3]));
  const declared = await waitForMessage(page, "pages_rendered");
  assert.deepEqual(declared.pages, [1, 2, 3]);
  assert.equal(declared.page, 1);
  const listing = await waitForMessage(page, "list");
  assert.deepEqual(listing.pages, [1, 2, 3]);
  return page.evaluate(({ listing, notesByPage }) => window.moyeAnnotations.result({
    session: listing.session,
    revision: listing.revision,
    request_id: listing.request_id,
    ok: true,
    pages: [1, 2, 3].map((page) => ({
      page,
      revision: 0,
      enabled: true,
      notes: notesByPage[page] || [],
    })),
  }), { listing, notesByPage });
}

const mark = (id, kind, quote, start, end) => ({
  id, kind, anchor: { quote, start, end }, content: null, stale: false,
});
const thought = (id, quote, start, end, content) => ({
  id, kind: "human_comment", anchor: { quote, start, end }, content, stale: false,
});

async function selectLine(page, pageNumber, index) {
  await page.evaluate(({ pageNumber, index }) => {
    const layer = document.querySelector(`.pdf-page[data-page="${pageNumber}"] .textLayer`);
    const range = document.createRange();
    range.selectNodeContents(layer.children[index]);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
  }, { pageNumber, index });
  await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
}

async function clickLine(page, pageNumber, index) {
  const point = await page.evaluate(({ pageNumber, index }) => {
    const layer = document.querySelector(`.pdf-page[data-page="${pageNumber}"] .textLayer`);
    const rect = layer.children[index].getBoundingClientRect();
    window.getSelection().removeAllRanges();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  }, { pageNumber, index });
  await page.mouse.click(point.x, point.y);
}

test("every mounted page keeps its own notes, marks and text layer", async () => {
  const page = await pageWithFixture();
  try {
    const beforeHtml = await page.evaluate(() =>
      document.querySelector('.pdf-page[data-page="2"] .textLayer').innerHTML);
    assert.equal(await loadPages(page, {
      1: [mark("mark-1", "highlight", "第一页第一行", 0, 6)],
      2: [thought("thought-2", "第二页第二行", 6, 12, "第二页的想法")],
      3: [],
    }), true);
    await page.waitForFunction(() => window.__notesRoot.querySelectorAll(".mark").length === 2);
    const kinds = await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".mark")]
      .map((node) => node.className).sort());
    assert.deepEqual(kinds, ["mark highlight", "mark human_comment"]);
    // The marks never rewrite the rendered page text.
    assert.equal(await page.evaluate(() =>
      document.querySelector('.pdf-page[data-page="2"] .textLayer').innerHTML), beforeHtml);
    // A page that is not mounted is not declared, so the host stops serving it.
    await page.evaluate(() => window.moyeAnnotations.setPages([1, 3]));
    const narrowed = await waitForMessage(page, "pages_rendered");
    assert.deepEqual(narrowed.pages, [1, 3]);
    await page.waitForFunction(() => window.__notesRoot.querySelectorAll(".mark").length === 1);
  } finally { await page.close(); }
});

test("clicking a mark opens the notes of the page that owns it", async () => {
  const page = await pageWithFixture();
  try {
    await loadPages(page, {
      1: [mark("mark-1", "highlight", "第一页第一行", 0, 6)],
      2: [thought("thought-2", "第二页第二行", 6, 12, "第二页的想法")],
      3: [],
    });
    await page.waitForFunction(() => window.__notesRoot.querySelectorAll(".mark").length === 2);

    await clickLine(page, 2, 1);
    await page.waitForFunction(() => !window.__notesRoot.querySelector(".drawer").hidden);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer-title").textContent),
      "划线相关笔记");
    assert.deepEqual(await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".note[data-note-id]")]
      .map((node) => node.dataset.noteId)), ["thought-2"]);

    await page.evaluate(() => window.__notesRoot.querySelector(".drawer .close").click());
    await page.waitForFunction(() => window.__notesRoot.querySelector(".drawer").hidden);

    // A mark without any thought becomes a real selection instead of an empty
    // drawer, and the toolbar belongs to that page.
    await clickLine(page, 1, 0);
    await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer").hidden), true);
    assert.equal(await page.evaluate(() => window.getSelection().toString()), "第一页第一行");
  } finally { await page.close(); }
});

test("page-scoped requests carry the page they were authored on", async () => {
  const page = await pageWithFixture();
  try {
    await loadPages(page, {
      1: [],
      2: [thought("thought-2", "第二页第二行", 6, 12, "第二页的想法")],
      3: [],
    });
    await selectLine(page, 2, 1);
    await page.evaluate(() => window.__notesRoot.querySelector('[data-action="highlight"]').click());
    const highlight = await waitForMessage(page, "highlight");
    assert.equal(highlight.page, 2);
    assert.deepEqual(highlight.anchor, { quote: "第二页第二行", start: 6, end: 12 });

    await selectLine(page, 3, 0);
    await page.evaluate(() => window.__notesRoot.querySelector('[data-action="underline"]').click());
    const underline = await waitForMessage(page, "underline");
    assert.equal(underline.page, 3);
    assert.deepEqual(underline.anchor, { quote: "第三页正文", start: 0, end: 5 });
  } finally { await page.close(); }
});

test("an unsaved draft pins its page and blocks the other page's notes", async () => {
  const page = await pageWithFixture();
  try {
    await loadPages(page, { 1: [], 2: [], 3: [] });
    await selectLine(page, 3, 0);
    await page.evaluate(() => window.__notesRoot.querySelector('[data-action="human_comment"]').click());
    const draft = await waitForMessage(page, "draft_changed");
    assert.equal(draft.page, 3);
    assert.equal(draft.dirty, true);
    assert.equal(await page.evaluate(() => window.moyeAnnotations.lockedPage()), 3);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".editor").hidden), false);
    // Scrolling elsewhere never moves the pinned page while the draft is open.
    await page.evaluate(() => window.moyeAnnotations.setCurrentPage(1));
    assert.equal(await page.evaluate(() => window.moyeAnnotations.lockedPage()), 3);
    // Cancelling releases the pin and reports the cleared draft.
    await page.evaluate(() => window.__notesRoot.querySelector(".editor .cancel").click());
    await page.waitForFunction(() => window.moyeAnnotations.lockedPage() === null);
    const cleared = (await messages(page, "draft_changed")).at(-1);
    assert.equal(cleared.dirty, false);
    assert.equal(cleared.page, 3);
  } finally { await page.close(); }
});
