// Optional DOM regression gate: node --test src/ui/pdf_reader/read_aloud.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const readAloudSource = fs.readFileSync(path.join(__dirname, "read_aloud.js"), "utf8");

// 合成文字层：PDF.js 的 span 是绝对定位的行片段（行末一个 <br>），段落靠行距
// 区分；未绘制的页（data-rendered=false）没有文字层。
const PAGE_STYLE = `<style>
body{margin:0}
.pdf-page{position:relative;width:600px;height:1200px}
.textLayer{position:absolute;inset:0}
.textLayer span{position:absolute;left:0;white-space:pre;font:16px/20px Arial}
</style>`;

const fixture = `<!DOCTYPE html><html><head>${PAGE_STYLE}</head><body>
<section class="pdf-page" data-page="1" data-rendered="true"><div class="textLayer">
<span id="l1" style="top:20px">First line of paragraph one.</span><br>
<span id="l2" style="top:40px">Second line continues.</span><br>
<span id="l3" style="top:100px">Another paragraph starts.</span><br>
<span id="l4" style="top:120px">with a continuation.</span><br>
</div></section>
<section class="pdf-page" data-page="2" data-rendered="false"></section>
</body></html>`;

const markedFixture = `<!DOCTYPE html><html><head>${PAGE_STYLE}</head><body>
<section class="pdf-page" data-page="1" data-rendered="true"><div class="textLayer">
<span class="markedContent" style="top:20px"><span id="m1" style="top:0">Wrapped text.</span></span><br>
<span class="markedContent" style="top:40px" aria-hidden="true"><span id="m2" style="top:0">Artifact text.</span></span><br>
</div></section>
</body></html>`;

const longFixture = `<!DOCTYPE html><html><head>${PAGE_STYLE}</head><body>
<section class="pdf-page" data-page="1" data-rendered="true"><div class="textLayer">
<span id="long" style="top:20px">${"第一句。第二句！".repeat(160)}</span><br>
</div></section>
</body></html>`;

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function openPage(html = fixture) {
  const page = await browser.newPage({ viewport: { width: 800, height: 400 } });
  await page.route("http://ngypdf.local/**", (route) => route.fulfill({
    status: 200,
    contentType: "text/html; charset=utf-8",
    body: html,
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    ${readAloudSource}
  ` });
  await page.goto("http://ngypdf.local/viewer.html");
  return page;
}

test("text-layer lines merge into paragraphs by line spacing", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.ngyPdfReadAloud.startFromViewport();
      const message = window.__messages.find((entry) => entry.type === "read_aloud_start");
      return {
        start: message ? message.start_block : null,
        blocks: message ? message.blocks.map((block) => block.text) : [],
        ids: message ? message.blocks.map((block) => block.id) : [],
      };
    });
    // 行距 0 的两行合成一段、间隔 40px 的行开新段；未绘制的第 2 页不参与。
    assert.deepEqual(state.blocks, [
      "First line of paragraph one. Second line continues.",
      "Another paragraph starts. with a continuation.",
    ]);
    assert.deepEqual(state.ids, ["0", "1"]);
    assert.equal(state.start, "0");
  } finally {
    await page.close();
  }
});

test("marked content wraps are collected and unrendered pages are ignored", async () => {
  const page = await openPage(markedFixture);
  try {
    const blocks = await page.evaluate(() => {
      window.ngyPdfReadAloud.startFromViewport();
      const message = window.__messages.find((entry) => entry.type === "read_aloud_start");
      return message ? message.blocks.map((block) => block.text) : [];
    });
    // markedContent 只是包装：里面的文字仍要收集（递归），否则整块会漏掉。
    assert.deepEqual(blocks, ["Wrapped text. Artifact text."]);
  } finally {
    await page.close();
  }
});

test("highlight marks only the active paragraph spans and clear removes it", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.ngyPdfReadAloud.startFromViewport();
      window.ngyPdfReadAloud.highlight("1");
      const active = Array.from(document.querySelectorAll(".ngy-read-aloud-active"))
        .map((node) => node.id)
        .sort();
      window.ngyPdfReadAloud.clear();
      return { active, after: document.querySelectorAll(".ngy-read-aloud-active").length };
    });
    // 第二段由 l3 与 l4 两行组成：高亮要覆盖该段的全部 span。
    assert.deepEqual(state.active, ["l3", "l4"]);
    assert.equal(state.after, 0);
  } finally {
    await page.close();
  }
});

test("a long paragraph splits at sentence boundaries without losing text", async () => {
  const page = await openPage(longFixture);
  try {
    const state = await page.evaluate(() => {
      const source = document.getElementById("long").textContent;
      window.ngyPdfReadAloud.startFromViewport();
      const message = window.__messages.find((entry) => entry.type === "read_aloud_start");
      return { source, blocks: message ? message.blocks.map((block) => block.text) : [] };
    });
    assert.ok(state.blocks.length > 1, `expected a split, got ${state.blocks.length}`);
    assert.ok(state.blocks.every((block) => block.length <= 480), "every block stays bounded");
    assert.equal(state.blocks.join(""), state.source);
  } finally {
    await page.close();
  }
});

test("startFromViewport skips paragraphs that scrolled above the viewport", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.scrollTo(0, 80);
      window.__messages.length = 0;
      window.ngyPdfReadAloud.startFromViewport();
      const message = window.__messages.find((entry) => entry.type === "read_aloud_start");
      return message ? message.start_block : null;
    });
    // 第一段整体滚到视口上方：起点应是第二段，而不是回到文档开头。
    assert.equal(state, "1");
  } finally {
    await page.close();
  }
});
