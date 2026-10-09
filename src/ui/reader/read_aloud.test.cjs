// Optional DOM regression gate: node --test src/ui/reader/read_aloud.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const readAloudSource = fs.readFileSync(path.join(__dirname, "read_aloud.js"), "utf8");
const translationsSource = fs.readFileSync(path.join(__dirname, "translations.js"), "utf8");

// 与译文门禁同一套夹具形状：真实章节是带命名空间的 XHTML。表格与代码块必须
// 拿不到朗读按钮，这是「不读代码与表格」的页面侧一半。
const fixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:22px/1.8 Arial;margin:50px}p{margin:15px 0}
</style></head><body>
<h1 id="heading">第一章</h1>
<p id="p1">Alpha paragraph.</p>
<p id="p2">Beta paragraph.</p>
<ul><li id="li1">List item one</li></ul>
<pre id="code"><code>let value = 1;</code></pre>
<table><tbody><tr><td id="cell">Cell text</td></tr></tbody></table>
<p id="p3">Gamma paragraph.</p>
</body></html>`;

const translationsPayload = {
  session: "read-aloud-session",
  revision: 1,
  displayMode: "translation-only",
  blocks: [
    { key: "p1", source: "Alpha paragraph.", segments: [{ source: "Alpha paragraph.", translated: "甲段落。" }] },
    { key: "p2", source: "Beta paragraph.", segments: [{ source: "Beta paragraph.", translated: "乙段落。" }] },
  ],
};

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function openPage() {
  const page = await browser.newPage({ viewport: { width: 1150, height: 900 } });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200,
    contentType: "application/xhtml+xml; charset=utf-8",
    body: fixture,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    ${translationsSource}
    ${readAloudSource}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.evaluate((payload) => window.ngyTranslations.configure(payload), translationsPayload);
  await page.waitForFunction(() => document.getElementById("p1").previousElementSibling
    && document.getElementById("p1").previousElementSibling.hasAttribute("data-ngy-translation"));
  await page.evaluate(() => window.ngyReadAloud.refresh());
  return page;
}

test("paragraphs carry a speaker button while code and table cells do not", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      const buttonOf = (id) => document.getElementById(id)
        .querySelector(":scope > .ngy-read-aloud-button");
      return {
        count: document.querySelectorAll(".ngy-read-aloud-button").length,
        heading: !!buttonOf("heading"),
        paragraph: !!buttonOf("p1"),
        listItem: !!buttonOf("li1"),
        code: !!document.getElementById("code").querySelector(".ngy-read-aloud-button"),
        cell: !!document.getElementById("cell").querySelector(".ngy-read-aloud-button"),
        // 译文层会给每个块复制一份同名的文本元素：不排除它就会多出按钮、把段落读两遍。
        nestedInLayer: !!document.querySelector("[data-ngy-translation] .ngy-read-aloud-button"),
      };
    });
    assert.equal(state.heading, true);
    assert.equal(state.paragraph, true);
    assert.equal(state.listItem, true);
    assert.equal(state.code, false, "代码块不朗读");
    assert.equal(state.cell, false, "表格单元格不朗读");
    assert.equal(state.nestedInLayer, false, "译文层内部不能出现朗读按钮");
    assert.equal(state.count, 5, "五个候选块各一个按钮");
  } finally { await page.close(); }
});

test("read aloud sends the displayed text and the block index it started from", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(async () => {
      document.getElementById("p1").querySelector(":scope > .ngy-read-aloud-button").click();
      // 点击即高亮，先看高亮再读消息。
      const highlighted = document.querySelectorAll(".ngy-read-aloud-active").length;
      const posted = window.__messages.filter((message) => message.type === "read_aloud_start");
      const last = posted[posted.length - 1];
      return {
        highlighted,
        postedCount: posted.length,
        startBlock: last?.start_block,
        texts: last?.blocks.map((block) => block.text) ?? [],
      };
    });
    assert.equal(state.postedCount, 1);
    // 候选块顺序是 heading, p1, p2, li1, p3：p1 的下标是 1。
    assert.equal(state.startBlock, "1");
    assert.equal(state.highlighted, 1, "点击后立刻高亮当前段");
    // 译文模式下 p1/p2 读译文，其余读原文——这就是「跟随显示方式」。
    assert.deepEqual(state.texts, ["第一章", "甲段落。", "乙段落。", "List item one", "Gamma paragraph."]);
  } finally { await page.close(); }
});

test("highlight only toggles a reversible class", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.ngyReadAloud.highlight("0");
      const afterHighlight = {
        count: document.querySelectorAll(".ngy-read-aloud-active").length,
        heading: document.getElementById("heading").classList.contains("ngy-read-aloud-active"),
        nodesUnchanged: document.getElementById("heading").textContent === "第一章",
      };
      window.ngyReadAloud.highlight("4");
      const moved = {
        heading: document.getElementById("heading").classList.contains("ngy-read-aloud-active"),
        paragraph: document.getElementById("p3").classList.contains("ngy-read-aloud-active"),
      };
      window.ngyReadAloud.clear();
      return {
        afterHighlight,
        moved,
        afterClear: document.querySelectorAll(".ngy-read-aloud-active").length,
      };
    });
    assert.equal(state.afterHighlight.count, 1);
    assert.equal(state.afterHighlight.heading, true);
    assert.equal(state.afterHighlight.nodesUnchanged, true, "高亮不改正文文本");
    assert.equal(state.moved.heading, false);
    assert.equal(state.moved.paragraph, true);
    assert.equal(state.afterClear, 0);
  } finally { await page.close(); }
});
