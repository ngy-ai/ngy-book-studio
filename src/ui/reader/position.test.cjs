// Optional DOM regression gate: node --test src/ui/reader/position.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const source = fs.readFileSync(path.join(__dirname, "position.js"), "utf8");

const paragraphs = Array.from({ length: 30 }, (_, index) =>
  `<p id="p${index + 1}">第 ${index + 1} 段文字，用来撑开页面并给折叠范围一个可靠的行盒。</p>`).join("");

const fixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:20px/1.8 Arial;margin:50px;max-width:760px}p{margin:18px 0}
</style></head><body><h1 id="heading">章内位置门禁</h1>${paragraphs}</body></html>`;

// 译文模式：译文层插在原文块前面，记录的位置必须仍然锚在原文上。
const translatedParagraphs = Array.from({ length: 30 }, (_, index) => {
  const paragraph =
    `<p id="p${index + 1}">第 ${index + 1} 段文字，用来撑开页面并给折叠范围一个可靠的行盒。</p>`;
  return index === 3
    ? `<div data-ngy-translation="" data-ngy-for="p4"><p>第四段的译文，不应进入位置锚点。</p></div>${paragraph}`
    : paragraph;
}).join("");
const translatedFixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:20px/1.8 Arial;margin:50px;max-width:760px}p{margin:18px 0}
</style></head><body><h1 id="heading">章内位置门禁</h1>${translatedParagraphs}</body></html>`;

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function openPage(html = fixture) {
  const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200,
    contentType: "application/xhtml+xml; charset=utf-8",
    body: html,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    ${source}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.waitForFunction(() => !!window.ngyPosition);
  return page;
}

test("a collapsed range reports the caret line box", async () => {
  const page = await openPage();
  try {
    const rect = await page.evaluate(() => {
      const node = document.getElementById("p5").firstChild;
      const range = document.createRange();
      range.setStart(node, 2);
      range.collapse(true);
      const box = range.getBoundingClientRect();
      return { width: box.width, height: box.height };
    });
    assert.ok(rect.height > 0, "插入点带行高，恢复才能对齐到行");
  } finally { await page.close(); }
});

test("the reported position tracks the first visible line and stays self-consistent", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.scrollTo(0, 0);
      const top = window.ngyPosition.current();
      window.scrollTo(0, 900);
      const middle = window.ngyPosition.current();
      const text = document.body.textContent.replace(/\s/gu, "");
      return { top, middle, contains: text.includes(middle.quote), scrollable: document.documentElement.scrollHeight };
    });
    assert.ok(state.top, "章首也要有位置");
    assert.ok(state.middle, "滚动后要有位置");
    assert.ok(state.scrollable > 1600, "夹具本身要能滚动");
    assert.equal(state.middle.end - state.middle.start, state.middle.quote.length,
      "引文长度与范围自洽");
    assert.ok(state.middle.start > state.top.start, "向下滚动后位置更靠后");
    assert.ok(state.contains, "引文来自正文");
  } finally { await page.close(); }
});

test("restore scrolls the saved position back to the viewport top", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.scrollTo(0, 900);
      const anchor = window.ngyPosition.current();
      window.scrollTo(0, 0);
      const restored = window.ngyPosition.restore(anchor);
      const after = window.ngyPosition.current();
      return { anchor, restored, after, scrollY: window.scrollY };
    });
    assert.equal(state.restored, true);
    assert.ok(state.scrollY > 700, "位置被滚回视口");
    assert.ok(Math.abs(state.after.start - state.anchor.start) <= 64,
      `恢复后仍是同一处：${state.after.start} vs ${state.anchor.start}`);
  } finally { await page.close(); }
});

test("a stale anchor never moves the page", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.scrollTo(0, 0);
      const restored = window.ngyPosition.restore({ quote: "不存在的引文", start: 0, end: 6 });
      return { restored, scrollY: window.scrollY };
    });
    assert.equal(state.restored, false);
    assert.equal(state.scrollY, 0);
  } finally { await page.close(); }
});

test("send posts one reading_position message and skips duplicates", async () => {
  const page = await openPage();
  try {
    const messages = await page.evaluate(() => {
      window.scrollTo(0, 0);
      window.ngyPosition.send();
      window.ngyPosition.send();
      window.scrollTo(0, 500);
      window.ngyPosition.send();
      return window.__messages.filter((message) => message.type === "reading_position");
    });
    assert.equal(messages.length, 2, "位置未变时不重发");
    for (const message of messages) {
      assert.equal(typeof message.anchor.quote, "string");
      assert.equal(message.anchor.end - message.anchor.start, message.anchor.quote.length);
    }
  } finally { await page.close(); }
});

test("the translation layer is never part of a recorded position", async () => {
  const page = await openPage(translatedFixture);
  try {
    const state = await page.evaluate(() => {
      const layer = document.querySelector("[data-ngy-translation]");
      // 真实的译文层会把原文块藏在译文下面：这里只验证位置映射回原文。
      window.ngyTranslations = {
        originalRange: (range) => {
          const parent = range.startContainer.parentElement;
          const layer = parent && parent.closest("[data-ngy-translation]");
          const original = layer && document.getElementById(layer.getAttribute("data-ngy-for"));
          if (!original) return null;
          const mapped = document.createRange();
          mapped.selectNodeContents(original);
          return mapped;
        },
      };
      window.scrollTo(0, layer.getBoundingClientRect().top + window.scrollY - 8);
      const anchor = window.ngyPosition.current();
      const text = document.body.textContent.replace(/\s/gu, "");
      return { anchor, contains: text.includes(anchor.quote) };
    });
    assert.ok(state.anchor, "译文层遮挡时也要有位置");
    assert.ok(state.anchor.quote.startsWith("第4段"), `位置落在原文块上：${state.anchor.quote}`);
    assert.ok(state.contains);
  } finally { await page.close(); }
});
