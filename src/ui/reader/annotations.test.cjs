// Optional DOM regression gate: node --test src/ui/reader/annotations.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const source = fs.readFileSync(path.join(__dirname, "annotations.js"), "utf8");
const readerSource = fs.readFileSync(path.join(__dirname, "..", "reader.rs"), "utf8");
const selectionBridgeMatch = readerSource.match(/const READER_INITIALIZATION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
assert.ok(selectionBridgeMatch, "The DOM gate must load the product's actual reader selection bridge");
const selectionBridge = selectionBridgeMatch[1];
const fixture = `<!doctype html><html><head><style>
  body{font:22px/1.8 Arial;margin:50px;max-width:760px}p{margin:15px 0}
  button{display:none!important}aside{color:red!important}body>div{display:none}
</style></head><body><h1>选段笔记</h1>
<p id="before">之前的文字😀</p><script>script text must not count</script>
<style>.unused{color:blue}</style><noscript>ignore me</noscript><template>ignore this too</template>
<p id="first">Alpha <b>beta</b> 😀 gamma</p>
<p id="second">Alpha <b>beta</b> 😀 gamma</p>
<p id="last">最后一段，用于保持原文选择与导航。</p>
</body></html>`;
let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function pageWithFixture(html = fixture, viewport = { width: 1150, height: 800 }, contentType = "text/html; charset=utf-8") {
  const page = await browser.newPage({ viewport });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200, contentType, body: html,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    const originalAttach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function(options) {
      const result = originalAttach.call(this, options);
      if (this.localName === 'ngy-reader-notes') window.__notesRoot = result;
      return result;
    };
    ${selectionBridge}
    ${source}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.evaluate(() => window.ngyAnnotations.configure({ session: "chapter-session", revision: 1, notes: [] }));
  await page.waitForFunction(() => !!window.__notesRoot && window.__messages.some((message) => message.action === "list"));
  return page;
}

async function select(page, selector = "#second") {
  await page.evaluate((selector) => {
    const range = document.createRange();
    range.selectNodeContents(document.querySelector(selector));
    window.getSelection().removeAllRanges();
    window.getSelection().addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
  }, selector);
  await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
}

async function click(page, selector) {
  const rect = await page.evaluate((selector) => {
    const node = window.__notesRoot.querySelector(selector);
    if (!node) throw new Error(`Missing control: ${selector}`);
    node.scrollIntoView({ block: "nearest", inline: "nearest" });
    const rect = node.getBoundingClientRect();
    return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
  }, selector);
  await page.mouse.click(rect.x, rect.y);
}

async function lastMessage(page, action) {
  await page.waitForFunction((action) => window.__messages.some((message) => message.action === action), action);
  return page.evaluate((action) => window.__messages.filter((message) => message.action === action).at(-1), action);
}

async function result(page, message, fields) {
  return page.evaluate(({ message, fields }) => window.ngyAnnotations.result({
    session: message.session, revision: message.revision, request_id: message.request_id, ...fields,
  }), { message, fields });
}

test("seven selection commands render mutually exclusive host marks at exact UTF-16 anchors without rewriting body", async () => {
  const page = await pageWithFixture();
  try {
    const beforeHtml = await page.locator("body").evaluate((body) => body.innerHTML);
    for (const kind of ["highlight", "wavy", "underline", "underline"]) {
      await select(page);
      const visible = await page.evaluate(() => {
        const host = document.querySelector("ngy-reader-notes");
        return { closed: host.shadowRoot === null,
          labels: [...window.__notesRoot.querySelectorAll(".tool")].map((node) => node.getAttribute("aria-label")) };
      });
      assert.equal(visible.closed, true);
      assert.deepEqual(visible.labels, ["复制", "马克笔", "波浪线", "直线", "删除划线", "写想法", "AI 解释"]);
      await click(page, `[data-action="${kind}"]`);
      const message = await lastMessage(page, kind);
      assert.deepEqual(message.anchor, {
        quote: "Alpha beta 😀 gamma",
        start: "选段笔记之前的文字😀Alphabeta😀gamma".length,
        end: "选段笔记之前的文字😀Alphabeta😀gammaAlphabeta😀gamma".length,
      });
      // The host owns the atomic style replacement; selecting the same style
      // returns the same note instead of accumulating another mark.
      const notes = [{ id: kind, kind, anchor: message.anchor, content: null }];
      assert.equal(await result(page, message, { ok: true, notes }), true);
      await page.waitForFunction((kind) => window.__notesRoot.querySelectorAll(`.mark.${kind}`).length > 0, kind);
      assert.deepEqual(await page.evaluate(() => [...new Set([...window.__notesRoot.querySelectorAll(".mark")]
        .map((node) => node.className))]), [`mark ${kind}`]);
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".note[data-note-id]").length), 1);
    }
    assert.equal(await page.locator("body").evaluate((body) => body.innerHTML), beforeHtml);
    assert.equal(await page.evaluate(() => document.querySelector("#second").textContent), "Alpha beta 😀 gamma");
    const markedPoint = await page.evaluate(() => {
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(document.querySelector("#second b"));
      const rect = range.getClientRects()[0];
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    });
    await page.evaluate(() => window.getSelection().removeAllRanges());
    await page.waitForFunction(() => window.__notesRoot.querySelector(".toolbar").hidden);
    await page.mouse.click(markedPoint.x, markedPoint.y);
    // A mark without any thought becomes a real selection: the same toolbar can
    // restyle or delete the mark, or start a thought on it.
    await page.waitForFunction(() => window.getSelection()?.toString() === "Alpha beta 😀 gamma");
    await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer").hidden), true);
    await page.evaluate(() => window.getSelection().removeAllRanges());
    await page.waitForFunction(() => window.__notesRoot.querySelector(".toolbar").hidden);
    await click(page, ".toggle");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer-title").textContent), "本章笔记");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".note.underline").length), 1);
    if (process.env.NGY_ANNOTATIONS_SCREENSHOT) {
      await select(page);
      await page.screenshot({ path: process.env.NGY_ANNOTATIONS_SCREENSHOT });
    }
  } finally { await page.close(); }
});

test("clicking a mark lists its thoughts when it has any and selects the mark when it has none", async () => {
  const page = await pageWithFixture();
  try {
    await select(page, "#second");
    await click(page, '[data-action="highlight"]');
    const mark = await lastMessage(page, "highlight");
    await result(page, mark, { ok: true, notes: [{ id: "plain-mark", kind: "highlight", anchor: mark.anchor }] });
    const clickMark = async () => {
      const point = await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        const rect = window.__notesRoot.querySelector(".mark.highlight").getBoundingClientRect();
        return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
      });
      await page.mouse.click(point.x, point.y);
    };
    await clickMark();
    await page.waitForFunction(() => window.getSelection()?.toString() === "Alpha beta 😀 gamma");
    await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer").hidden), true);
    assert.deepEqual(await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".tool")]
      .map((tool) => tool.getAttribute("aria-label"))),
    ["复制", "马克笔", "波浪线", "直线", "删除划线", "写想法", "AI 解释"]);

    // A thought on the very same range keeps the drawer, and the empty-state
    // status is no longer reachable from a mark click.
    await page.evaluate(() => window.getSelection().removeAllRanges());
    await page.waitForFunction(() => window.__notesRoot.querySelector(".toolbar").hidden);
    const notes = [
      { id: "plain-mark", kind: "highlight", anchor: mark.anchor },
      { id: "range-human", kind: "human_comment", anchor: mark.anchor, content: "这处划线的想法" },
      { id: "other-human", kind: "human_comment", anchor: { ...mark.anchor, start: mark.anchor.start + 1 },
        content: "同一句中另一个范围的其它想法" },
    ];
    await page.evaluate((notes) => window.ngyAnnotations.render({ session: "chapter-session", revision: 1, notes }), notes);
    await clickMark();
    await page.waitForFunction(() => window.__notesRoot.querySelector(".drawer-title").textContent === "划线相关笔记");
    assert.deepEqual(await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".note[data-note-id]")]
      .map((note) => note.dataset.noteId)), ["range-human"]);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".toolbar").hidden), true);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".list-status").textContent), "");
  } finally { await page.close(); }
});

test("human thought failures preserve drafts and successful saves clear the host dirty flag", async () => {
  const page = await pageWithFixture();
  try {
    await select(page);
    await click(page, '[data-action="human_comment"]');
    assert.equal((await lastMessage(page, "draft_changed")).dirty, true);
    const thought = '人工想法：<img src="https://example.com/not-loaded"> **不会执行 HTML**';
    await page.evaluate((value) => {
      const input = window.__notesRoot.querySelector("textarea");
      input.value = value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, thought);
    await click(page, ".editor .save");
    const failed = await lastMessage(page, "human_comment");
    assert.equal(failed.content, thought);
    await result(page, failed, { ok: false, error: "模拟磁盘写入失败" });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").value), thought);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").disabled), false);
    await click(page, ".editor .save");
    const success = await lastMessage(page, "human_comment");
    assert.notEqual(success.request_id, failed.request_id);
    const note = { id: "human-1", kind: "human_comment", anchor: success.anchor, content: thought };
    await result(page, success, { ok: true, notes: [note] });
    assert.equal((await lastMessage(page, "draft_changed")).dirty, false);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".editor").hidden), true);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".content").textContent), thought);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll("img,a,iframe,script").length), 0);
    await click(page, ".note .text-button:nth-child(2)");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").value), thought);
    await click(page, ".editor .cancel");
    assert.equal((await lastMessage(page, "draft_changed")).dirty, false);
  } finally { await page.close(); }
});

test("native AI explanation uses the frozen context-menu anchor and can retry a failed AI save", async () => {
  const page = await pageWithFixture();
  try {
    await select(page);
    await page.evaluate(() => {
      document.querySelector("#second").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      window.getSelection().removeAllRanges();
    });
    assert.equal(await page.evaluate(() => window.ngyAnnotations.explainSelection("Alpha beta 😀 gamma")), true);
    const request = await lastMessage(page, "ai_explain");
    const aiText = "AI 解释：这是生成的想法，不是人工想法。\n<script>never executed</script>";
    await page.evaluate(({ request, aiText }) => window.ngyAnnotations.state({
      session: request.session, revision: request.revision, request_id: request.request_id,
      phase: "streaming", content: aiText,
    }), { request, aiText });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".pending-note .content").textContent), aiText);
    await result(page, request, { ok: false, retry_ai_save: true, content: aiText, error: "模拟保存失败" });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".pending-note .content").textContent), aiText);
    await click(page, ".pending-note .save");
    const retry = await lastMessage(page, "retry_ai_save");
    assert.equal(Object.hasOwn(retry, "content"), false);
    assert.equal(Object.hasOwn(retry, "anchor"), false);
    await result(page, retry, { ok: true, notes: [{ id: "ai-1", kind: "ai_comment", anchor: request.anchor, content: aiText }] });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".note-type").textContent), "AI 想法");
    assert.equal(await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".note button")].some((node) => node.textContent === "编辑")), false);
    assert.equal(await page.evaluate(() => window.ngyAnnotations.explainSelection("different text")), false);
  } finally { await page.close(); }
});

test("native AI explanation accepts the block-separated text WebView2 reports", async () => {
  // Minified block markup has no whitespace between the blocks, so the frozen
  // quote is glued while the menu reports the boundary as newlines.
  const page = await pageWithFixture('<!doctype html><html><body>' +
    '<p id="second">Alpha <b>beta</b> 😀 gamma</p>' +
    '<p id="last">最后一段，用于保持原文选择与导航。</p></body></html>');
  const quote = "Alpha beta 😀 gamma最后一段，用于保持原文选择与导航。";
  try {
    // WebView2 captures the menu selection with Blink's text iterator, which
    // emits '\n' at block boundaries where the frozen range's textContent has no
    // separator. Chromium derives getSelection().toString() the same way, so it
    // stands in for the text the host hands back to the page bridge.
    const reported = await page.evaluate(() => {
      const end = document.querySelector("#last").firstChild;
      const range = document.createRange();
      range.setStart(document.querySelector("#second").firstChild, 0);
      range.setEnd(end, end.length);
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
      document.querySelector("#second").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      return window.getSelection().toString();
    });
    assert.ok(reported.includes("\n"), `the menu text keeps the block separator: ${JSON.stringify(reported)}`);
    assert.equal(reported.replace(/\s/gu, ""), quote.replace(/\s/gu, ""));
    // The exact comparison the native menu used to be gated on: normalizing
    // both sides still leaves a space where the frozen quote has none.
    assert.notEqual(reported.replace(/\s+/gu, " ").trim(), quote);
    assert.equal(await page.evaluate((text) => window.ngyAnnotations.explainSelection(text), reported), true);
    const request = await lastMessage(page, "ai_explain");
    assert.equal(request.anchor.quote, quote);
    assert.deepEqual([request.anchor.start, request.anchor.end], [0, quote.replace(/\s/gu, "").length]);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".note.pending-note .quote").textContent),
      quote);
  } finally { await page.close(); }
});

test("native AI explanation accepts a <br> selection reported with a newline", async () => {
  const page = await pageWithFixture('<!doctype html><html><body><p id="second">Alpha<br>beta</p></body></html>');
  try {
    const reported = await page.evaluate(() => {
      const range = document.createRange();
      range.selectNodeContents(document.querySelector("#second"));
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
      document.querySelector("#second").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      return window.getSelection().toString();
    });
    assert.ok(reported.includes("\n"), `the menu text keeps the line break: ${JSON.stringify(reported)}`);
    assert.notEqual(reported.replace(/\s+/gu, " ").trim(), "Alphabeta");
    assert.equal(await page.evaluate((text) => window.ngyAnnotations.explainSelection(text), reported), true);
    assert.equal((await lastMessage(page, "ai_explain")).anchor.quote, "Alphabeta");
  } finally { await page.close(); }
});

test("stale sessions cannot mutate notes; deletion failure restores its button", async () => {
  const page = await pageWithFixture();
  try {
    await select(page);
    await click(page, '[data-action="highlight"]');
    const request = await lastMessage(page, "highlight");
    const note = { id: "note-1", kind: "highlight", anchor: request.anchor, content: null };
    await result(page, request, { ok: true, notes: [note] });
    const stale = await page.evaluate(() => window.ngyAnnotations.render({ session: "old-session", revision: 1, notes: [] }));
    assert.equal(stale, false);
    await click(page, ".toggle");
    await click(page, ".note .danger");
    const deletion = await lastMessage(page, "delete");
    await result(page, deletion, { ok: false, error: "模拟删除失败" });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".note .danger").disabled), false);
    await click(page, ".note .danger");
    await result(page, await lastMessage(page, "delete"), { ok: true, notes: [] });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".note").length), 0);
  } finally { await page.close(); }
});

test("oversized selections are rejected without truncating the original selection", async () => {
  const text = "段".repeat(12000);
  const page = await pageWithFixture(`<!doctype html><body><p id="long">${text}</p></body>`);
  try {
    const accepted = await page.evaluate(() => {
      const range = document.createRange();
      range.selectNodeContents(document.querySelector("#long"));
      window.getSelection().addRange(range);
      return window.ngyAnnotations.explainSelection();
    });
    assert.equal(accepted, false);
    assert.equal(await page.evaluate(() => window.getSelection().toString().length), 12000);
    assert.equal(await page.evaluate(() => window.__messages.some((message) => message.action === "ai_explain")), false);
  } finally { await page.close(); }
});

test("stale notes retain their quote and thought but do not paint onto matching revised text", async () => {
  const page = await pageWithFixture();
  try {
    await select(page);
    await click(page, '[data-action="highlight"]');
    const request = await lastMessage(page, "highlight");
    await result(page, request, { ok: true, notes: [{
      id: "stale", kind: "human_comment", anchor: request.anchor, content: "保留的想法", stale: true,
    }] });
    await click(page, ".toggle");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".stale").textContent), "原文已变化");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".content").textContent), "保留的想法");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".mark").length), 0);
    assert.equal(await page.evaluate(() => [...window.__notesRoot.querySelectorAll(".note button")].some((node) => node.textContent === "定位原文")), false);
  } finally { await page.close(); }
});

test("selection toolbar stays within a narrow viewport", async () => {
  const page = await pageWithFixture(fixture, { width: 360, height: 700 });
  try {
    await select(page);
    const box = await page.evaluate(() => {
      const rect = window.__notesRoot.querySelector(".toolbar").getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    });
    assert.ok(box.left >= 0 && box.right <= 360, JSON.stringify(box));
    assert.ok(box.top >= 0 && box.bottom <= 700, JSON.stringify(box));
  } finally { await page.close(); }
});

test("multiline selections keep all seven toolbar commands on one row through narrow resizes", async () => {
  const paragraph = "读者可以从一行文字的中间开始选择，跨过下一行继续阅读和记录想法。浮动菜单应当保持所有操作在同一行，并完整显示图标与名称。".repeat(5);
  const html = `<!doctype html><html><head><style>
    body{margin:24px;font:22px/1.8 "Microsoft YaHei",Arial,sans-serif}
    h1{font-size:26px;margin-bottom:36px}p{margin:0;overflow-wrap:anywhere}
  </style></head><body><h1>多行选区菜单回归</h1><p id="multiline">${paragraph}</p></body></html>`;
  const page = await pageWithFixture(html, { width: 1000, height: 900 });
  const selectMultiline = async (fraction) => {
    await page.evaluate(() => window.getSelection().removeAllRanges());
    await page.waitForFunction(() => window.__notesRoot.querySelector(".toolbar").hidden);
    const geometry = await page.evaluate((fraction) => {
      const paragraph = document.querySelector("#multiline");
      const text = paragraph.firstChild;
      const paragraphBox = paragraph.getBoundingClientRect();
      const chars = [];
      for (let offset = 0; offset < text.length; offset++) {
        const character = document.createRange();
        character.setStart(text, offset);
        character.setEnd(text, offset + 1);
        const rect = character.getBoundingClientRect();
        chars.push({ offset, left: rect.left, top: rect.top });
      }
      const lines = [...new Set(chars.map((character) => character.top))];
      const start = chars.find((character) => character.top === lines[1] &&
        character.left >= paragraphBox.left + paragraphBox.width * fraction);
      const end = chars.find((character) => character.top === lines[2] &&
        character.left >= paragraphBox.left + paragraphBox.width * 0.45);
      if (!start || !end) throw new Error("Fixture did not form the required multiline selection");
      const range = document.createRange();
      range.setStart(text, start.offset);
      range.setEnd(text, end.offset + 1);
      window.getSelection().addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const rects = [...range.getClientRects()];
      return { start: start.offset, firstLeft: rects[0].left, paragraphLeft: paragraphBox.left,
        selectedLines: new Set(rects.map((rect) => rect.top)).size };
    }, fraction);
    assert.ok(geometry.start > 0 && geometry.firstLeft > geometry.paragraphLeft + 40, JSON.stringify(geometry));
    assert.ok(geometry.selectedLines >= 2, JSON.stringify(geometry));
    await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
  };
  const assertSingleRow = async (scenario) => {
    const layout = await page.evaluate(() => {
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      const rect = toolbar.getBoundingClientRect();
      const buttons = [...toolbar.querySelectorAll(".tool")].map((button) => {
        const icon = button.querySelector(".tool-icon");
        const label = button.lastElementChild;
        const buttonBox = button.getBoundingClientRect();
        const iconBox = icon.getBoundingClientRect();
        const labelBox = label.getBoundingClientRect();
        const labelRange = document.createRange();
        labelRange.selectNodeContents(label);
        return { label: label.textContent, top: buttonBox.top, bottom: buttonBox.bottom,
          left: buttonBox.left, right: buttonBox.right, icon: iconBox.toJSON(), text: labelBox.toJSON(),
          labelLines: new Set([...labelRange.getClientRects()].map((part) => part.top)).size,
          labelScroll: label.scrollWidth, labelWidth: label.clientWidth };
      });
      return { viewport: document.documentElement.clientWidth, rect: rect.toJSON(), buttons };
    });
    const evidence = `${scenario}: ${JSON.stringify(layout)}`;
    assert.equal(layout.buttons.length, 7, evidence);
    assert.ok(layout.rect.left >= 0 && layout.rect.right <= layout.viewport + 0.5, evidence);
    const rowTops = layout.buttons.map((button) => button.top);
    assert.ok(Math.max(...rowTops) - Math.min(...rowTops) <= 0.5, evidence);
    for (const button of layout.buttons) {
      assert.equal(button.labelLines, 1, evidence);
      assert.ok(button.labelScroll <= button.labelWidth + 1, evidence);
      for (const part of [button.icon, button.text]) {
        assert.ok(part.left >= button.left - 0.5 && part.right <= button.right + 0.5, evidence);
        assert.ok(part.top >= button.top - 0.5 && part.bottom <= button.bottom + 0.5, evidence);
      }
    }
  };
  try {
    await selectMultiline(0.78);
    await assertSingleRow("initial right-side selection at 1000px");
    for (const width of [620, 500, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await assertSingleRow(`resize existing multiline selection to ${width}px`);
      for (const fraction of [0.5, 0.78, 0.5]) {
        await selectMultiline(fraction);
        await assertSingleRow(`new selection at ${width}px starting ${fraction} across the line`);
      }
    }
    const lastCommand = await page.evaluate(() => {
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      toolbar.scrollLeft = toolbar.scrollWidth;
      const ai = toolbar.querySelector('[data-action="ai_explain"]').getBoundingClientRect();
      const bounds = toolbar.getBoundingClientRect();
      return { x: ai.left + ai.width / 2, y: ai.top + ai.height / 2,
        visible: ai.left >= bounds.left && ai.right <= bounds.right + 0.5,
        overflow: toolbar.scrollWidth > toolbar.clientWidth, scrollLeft: toolbar.scrollLeft };
    });
    assert.equal(lastCommand.visible, true, JSON.stringify(lastCommand));
    if (lastCommand.overflow) assert.ok(lastCommand.scrollLeft > 0, JSON.stringify(lastCommand));
    await page.mouse.click(lastCommand.x, lastCommand.y);
    const explanation = await lastMessage(page, "ai_explain");
    assert.ok(explanation.anchor.quote.length > 0);
  } finally { await page.close(); }
});

test("the trusted bridge stays absent on external documents and child frames", async () => {
  const page = await browser.newPage();
  try {
    await page.route("http://**/*", (route) => route.fulfill({
      status: 200, contentType: "text/html; charset=utf-8",
      body: route.request().url().endsWith("/frame") ? "<!doctype html><body>child</body>" :
        '<!doctype html><body>top<iframe src="/frame"></iframe></body>',
    }));
    await page.addInitScript({ content: `${selectionBridge}\n${source}` });
    await page.goto("http://external.invalid/chapter");
    assert.equal(await page.evaluate(() => typeof window.ngyAnnotations), "undefined");
    await page.goto("http://epubreader.book/chapter");
    assert.equal(await page.evaluate(() => typeof window.ngyAnnotations), "object");
    const frame = page.frames().find((frame) => frame !== page.mainFrame());
    assert.ok(frame);
    assert.equal(await frame.evaluate(() => typeof window.ngyAnnotations), "undefined");
  } finally { await page.close(); }
});

test("XHTML reader pages attach working HTML controls and can save and discard AI drafts", async () => {
  const xhtml = fixture.replace("<!doctype html>", '<?xml version="1.0" encoding="UTF-8"?>')
    .replace("<html>", '<html xmlns="http://www.w3.org/1999/xhtml">');
  const page = await pageWithFixture(xhtml, { width: 1150, height: 800 }, "application/xhtml+xml; charset=utf-8");
  try {
    assert.equal(await page.evaluate(() => document.contentType), "application/xhtml+xml");
    await select(page);
    await click(page, '[data-action="highlight"]');
    const mark = await lastMessage(page, "highlight");
    await result(page, mark, { ok: true, notes: [{ id: "xhtml-mark", kind: "highlight", anchor: mark.anchor }] });
    await select(page);
    await click(page, '[data-action="ai_explain"]');
    const ai = await lastMessage(page, "ai_explain");
    const longReply = "长AI回复".repeat(14000);
    await result(page, ai, { ok: false, retry_ai_save: true, content: longReply, error: "模拟保存失败" });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".pending-note .content").textContent.length), longReply.length);
    await click(page, ".pending-note .danger");
    const discarded = await lastMessage(page, "discard_ai_save");
    assert.equal(Object.hasOwn(discarded, "content"), false);
    await result(page, discarded, { ok: true, notes: [] });
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".pending-note").length), 0);
    await click(page, ".close");
    await select(page);
    await click(page, '[data-action="human_comment"]');
    await page.evaluate(() => {
      const input = window.__notesRoot.querySelector("textarea");
      input.value = "XHTML 页面中的人工想法";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(page, ".editor .save");
    assert.equal((await lastMessage(page, "human_comment")).content, "XHTML 页面中的人工想法");
  } finally { await page.close(); }
});

test("book links cannot navigate away from an unsaved human thought, including an empty editor", async () => {
  const html = fixture.replace("</body>", '<p><a id="next" href="next.html">下一章链接</a></p></body>');
  const page = await pageWithFixture(html);
  try {
    const originalUrl = page.url();
    await select(page);
    await click(page, '[data-action="human_comment"]');
    await page.locator("#next").click();
    assert.equal(page.url(), originalUrl);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".editor").hidden), false);
    await page.evaluate(() => {
      const input = window.__notesRoot.querySelector("textarea");
      input.value = "不能因正文链接跳转而丢失的想法";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await page.locator("#next").click();
    assert.equal(page.url(), originalUrl);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").value), "不能因正文链接跳转而丢失的想法");
    await click(page, ".editor .cancel");
    await page.locator("#next").click();
    assert.equal(page.url(), "http://epubreader.book/next.html");
  } finally { await page.close(); }
});

test("clicking overlapping marks shows only associated ranges and keeps that scope through refresh and deletion", async () => {
  const xhtml = fixture.replace("<!doctype html>", '<?xml version="1.0" encoding="UTF-8"?>')
    .replace("<html>", '<html xmlns="http://www.w3.org/1999/xhtml">');
  const page = await pageWithFixture(xhtml, { width: 1150, height: 800 }, "application/xhtml+xml; charset=utf-8");
  const visibleIds = () => page.evaluate(() => [...window.__notesRoot.querySelectorAll(".note[data-note-id]")]
    .map((note) => note.dataset.noteId).sort());
  const title = () => page.evaluate(() => window.__notesRoot.querySelector(".drawer-title").textContent);
  const clickBeta = async () => {
    const point = await page.evaluate(() => {
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(document.querySelector("#second b"));
      const rect = range.getClientRects()[0];
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    });
    await page.mouse.click(point.x, point.y);
    try {
      await page.waitForFunction(() => window.__notesRoot.querySelector(".drawer-title").textContent === "划线相关笔记", undefined, { timeout: 3000 });
    } catch {
      assert.fail(JSON.stringify(await page.evaluate((point) => ({
        point, target: document.elementFromPoint(point.x, point.y)?.outerHTML,
        collapsed: window.getSelection()?.isCollapsed, selected: window.getSelection()?.toString(),
        title: window.__notesRoot.querySelector(".drawer-title").textContent,
        marks: [...window.__notesRoot.querySelectorAll(".mark")].map((node) => ({
          className: node.className, rect: node.getBoundingClientRect().toJSON(),
        })),
      }), point)));
    }
  };
  try {
    await select(page, "#first");
    await click(page, '[data-action="highlight"]');
    const first = await lastMessage(page, "highlight");
    await result(page, first, { ok: true, notes: [] });
    await select(page, "#second");
    await click(page, '[data-action="underline"]');
    const second = await lastMessage(page, "underline");
    assert.equal(first.anchor.quote, second.anchor.quote);
    assert.notEqual(first.anchor.start, second.anchor.start);
    const beta = { quote: "beta", start: second.anchor.start + 5, end: second.anchor.start + 9 };
    const gamma = { quote: "gamma", start: second.anchor.end - 5, end: second.anchor.end };
    let notes = [
      { id: "first-mark", kind: "highlight", anchor: first.anchor },
      { id: "first-human", kind: "human_comment", anchor: first.anchor, content: "别处相同文字的想法" },
      { id: "second-mark", kind: "underline", anchor: second.anchor },
      { id: "second-human", kind: "human_comment", anchor: { ...second.anchor, quote: "Alpha \n beta   😀 gamma" }, content: "本处整句人工想法" },
      { id: "second-ai", kind: "ai_comment", anchor: second.anchor, content: "本处整句 AI 想法" },
      { id: "second-stale", kind: "wavy", anchor: second.anchor, stale: true },
      { id: "beta-mark", kind: "wavy", anchor: beta },
      { id: "beta-human", kind: "human_comment", anchor: beta, content: "与整句同时命中的词语想法" },
      { id: "gamma-human", kind: "human_comment", anchor: gamma, content: "同一句中未命中的另一个范围" },
      { id: "different-quote", kind: "ai_comment", anchor: { ...second.anchor, quote: "Other beta 😀 gamma" }, content: "偏移相同但引用不匹配" },
    ];
    await result(page, second, { ok: true, notes });
    await page.waitForFunction(() => window.__notesRoot.querySelectorAll(".mark.wavy").length > 0);
    await clickBeta();
    const relatedIds = ["beta-human", "second-ai", "second-human"];
    assert.deepEqual(await visibleIds(), relatedIds);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".note.highlight,.note.wavy,.note.underline").length), 0);

    notes = [...notes, { id: "first-new-ai", kind: "ai_comment", anchor: first.anchor, content: "刷新增加的无关想法" }];
    await page.evaluate((notes) => window.ngyAnnotations.render({ session: "chapter-session", revision: 1, notes }), notes);
    assert.equal(await title(), "划线相关笔记");
    assert.deepEqual(await visibleIds(), relatedIds);
    await select(page, "#second");
    await click(page, '[data-action="remove_mark"]');
    const removed = await lastMessage(page, "remove_mark");
    assert.deepEqual(removed.anchor, second.anchor);
    assert.equal(Object.hasOwn(removed, "id"), false);
    assert.equal(Object.hasOwn(removed, "content"), false);
    notes = notes.filter((note) => note.id !== "second-mark");
    await result(page, removed, { ok: true, notes });
    assert.equal(await title(), "划线相关笔记");
    assert.deepEqual(await visibleIds(), relatedIds);
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".toast").textContent), "划线已删除。");
    await page.waitForFunction(() => window.__notesRoot.querySelectorAll(".mark.underline").length === 0);
    assert.ok(await page.evaluate(() => window.__notesRoot.querySelectorAll(".mark.highlight,.mark.wavy,.mark.human_comment,.mark.ai_comment").length > 0));

    await click(page, '.note[data-note-id="second-human"] .danger');
    const deletion = await lastMessage(page, "delete");
    notes = notes.filter((note) => note.id !== "second-human");
    await result(page, deletion, { ok: true, notes });
    assert.equal(await title(), "划线相关笔记");
    assert.deepEqual(await visibleIds(), relatedIds.filter((id) => id !== "second-human"));

    await click(page, ".toggle");
    assert.equal(await title(), "本章笔记");
    assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".drawer").hidden), false);
    assert.deepEqual(await visibleIds(), notes.map((note) => note.id).sort());
    await clickBeta();
    await page.keyboard.press("Control+Alt+n");
    assert.equal(await title(), "本章笔记");
    assert.deepEqual(await visibleIds(), notes.map((note) => note.id).sort());

    await clickBeta();
    await page.evaluate((notes) => window.ngyAnnotations.configure({ session: "next-chapter-session", revision: 2, notes }), notes);
    assert.equal(await title(), "本章笔记");
    assert.deepEqual(await visibleIds(), notes.map((note) => note.id).sort());
  } finally { await page.close(); }
});

for (const mime of ["text/html", "application/xhtml+xml"]) {
  test(`Markdown thoughts render safely, preserve source, and stay outside chapter selection in ${mime}`, async () => {
    const html = mime === "application/xhtml+xml" ? fixture
      .replace("<!doctype html>", '<?xml version="1.0" encoding="UTF-8"?>')
      .replace("<html>", '<html xmlns="http://www.w3.org/1999/xhtml">') : fixture;
    const page = await pageWithFixture(html, { width: 1150, height: 800 }, `${mime}; charset=utf-8`);
    const requests = [];
    page.on("request", (request) => requests.push(request.url()));
    const markdown = "## 标题\n\n**加粗** 和 *强调*，~~删除内容~~。\n\n- 一\n- 二\n\n3. 三\n\n> 引用\n\n```rust\nlet value = 1;\n```\n\n| 列 | 值 |\n| --- | --- |\n| A | B |\n\n- [x] 完成\n\n---\n\n[链接文字](https://untrusted.invalid/)";
    const contentHtml = `<h2 onclick="window.__markdownExecuted=true" style="background:red">标题</h2>
      <p><strong>加粗</strong> 和 <em>强调</em>，<del>删除内容</del>。</p>
      <ul><li>一</li><li>二</li></ul><ol start="3"><li>三</li></ol>
      <blockquote><p>引用</p></blockquote><pre><code class="language-rust">${"let value = 1; ".repeat(90)}</code></pre>
      <table><thead><tr><th align="center">列</th><th>值</th></tr></thead><tbody><tr><td align="right">A</td><td>${"long_cell_".repeat(80)}</td></tr></tbody></table>
      <ul><li><input type="checkbox" disabled="" checked="" onclick="window.__markdownExecuted=true">完成</li></ul><hr>
      <p><a href="https://untrusted.invalid/link" onclick="window.__markdownExecuted=true">链接文字</a></p>
      <script>window.__markdownExecuted=true</script><img src="https://untrusted.invalid/image" onerror="window.__markdownExecuted=true">
      <iframe src="https://untrusted.invalid/frame"></iframe><svg onload="window.__markdownExecuted=true"></svg>
      <style>@import url(https://untrusted.invalid/style);</style><template><img src="https://untrusted.invalid/template"></template>`;
    try {
      const beforeBody = await page.locator("body").evaluate((body) => body.innerHTML);
      await page.evaluate(() => {
        window.__markdownExecuted = false;
        document.addEventListener("copy", (event) => {
          window.__copiedMarkdown = event.clipboardData?.getData("text/plain");
        });
      });
      await select(page);
      await click(page, '[data-action="highlight"]');
      const mark = await lastMessage(page, "highlight");
      const notes = [
        { id: "markdown-mark", kind: "highlight", anchor: mark.anchor },
        { id: "markdown-human", kind: "human_comment", anchor: mark.anchor, content: markdown, content_html: contentHtml },
        { id: "markdown-ai", kind: "ai_comment", anchor: mark.anchor, content: markdown, content_html: contentHtml },
      ];
      await result(page, mark, { ok: true, notes });
      await click(page, ".toggle");
      const rendering = await page.evaluate(() => {
        const cards = [...window.__notesRoot.querySelectorAll(".note .markdown")];
        return cards.map((content) => ({
          h2: content.querySelector("h2")?.textContent,
          strong: content.querySelector("strong")?.textContent,
          emphasis: getComputedStyle(content.querySelector("em")).fontStyle,
          deletion: getComputedStyle(content.querySelector("del")).textDecorationLine,
          listCount: content.querySelectorAll("ul,ol").length,
          start: content.querySelector("ol").start,
          blockquote: content.querySelector("blockquote")?.textContent,
          task: content.querySelector(".task-checkbox")?.textContent,
          align: getComputedStyle(content.querySelector("td")).textAlign,
          hasRule: !!content.querySelector("hr"),
          preScroll: content.querySelector("pre").scrollWidth > content.querySelector("pre").clientWidth,
          tableScroll: content.querySelector("table").scrollWidth > content.querySelector("table").clientWidth,
          fitsDrawer: content.closest(".note").getBoundingClientRect().width <= window.__notesRoot.querySelector(".drawer").clientWidth,
          activeCount: content.querySelectorAll("a,img,iframe,script,style,svg,math,template,input,[href],[src],[onclick],[onerror]").length,
          htmlNamespace: [...content.querySelectorAll("*")].every((node) => node.namespaceURI === "http://www.w3.org/1999/xhtml"),
          linkText: content.textContent.includes("链接文字"),
        }));
      });
      assert.equal(rendering.length, 2);
      for (const card of rendering) {
        assert.deepEqual(card, {
          h2: "标题", strong: "加粗", emphasis: "italic", deletion: "line-through", listCount: 3,
          start: 3, blockquote: "引用", task: "☑", align: "right", hasRule: true,
          preScroll: true, tableScroll: true, fitsDrawer: true, activeCount: 0, htmlNamespace: true, linkText: true,
        });
      }
      await click(page, '.note[data-note-id="markdown-human"] h2');
      assert.equal(await page.evaluate(() => window.__markdownExecuted), false);
      assert.deepEqual(requests, [], "Inert parsing or rendered Markdown requested a resource");

      for (const id of ["markdown-human", "markdown-ai"]) {
        await click(page, `.note[data-note-id="${id}"] .copy-thought`);
        assert.equal(await page.evaluate(() => window.__copiedMarkdown), markdown);
        assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".copy-source")), null);
      }
      await click(page, '.note[data-note-id="markdown-human"] .text-button:nth-child(2)');
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").value), markdown);
      await page.evaluate(() => {
        const input = window.__notesRoot.querySelector("textarea");
        input.value += "\n\n**人工补充**";
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
      await click(page, ".editor .save");
      const edit = await lastMessage(page, "update");
      assert.equal(edit.content, `${markdown}\n\n**人工补充**`);
      await result(page, edit, { ok: false, error: "保留 Markdown 草稿" });
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelector("textarea").value), edit.content);
      await click(page, ".editor .cancel");

      const beforeSelection = await page.evaluate(() => window.__messages.filter((message) => message.type === "selection_changed").length);
      const points = await page.evaluate(() => {
        const strong = window.__notesRoot.querySelector('.note[data-note-id="markdown-human"] strong');
        strong.scrollIntoView({ block: "center" });
        const range = document.createRange();
        range.selectNodeContents(strong);
        const rect = range.getBoundingClientRect();
        return { start: rect.left + .1, end: rect.right - .1, y: rect.top + rect.height / 2 };
      });
      await page.mouse.move(points.start, points.y);
      await page.mouse.down();
      await page.mouse.move(points.end, points.y, { steps: 4 });
      await page.mouse.up();
      await page.waitForTimeout(120);
      assert.equal(await page.evaluate(() => window.getSelection().toString()), "加粗");
      const selectionUpdates = await page.evaluate((before) => window.__messages
        .filter((message) => message.type === "selection_changed").slice(before), beforeSelection);
      assert.ok(selectionUpdates.length > 0);
      assert.ok(selectionUpdates.every((message) => message.selected_text === ""));

      const hit = await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        const rect = document.querySelector("#second b").getBoundingClientRect();
        return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
      });
      await page.mouse.click(hit.x, hit.y);
      await page.waitForFunction(() => window.__notesRoot.querySelector(".drawer-title").textContent === "划线相关笔记");
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelectorAll(".note .markdown h2").length), 2);
      assert.equal(await page.locator("body").evaluate((body) => body.innerHTML), beforeBody);

      await click(page, ".close");
      await select(page, "#first");
      await click(page, '[data-action="ai_explain"]');
      const ai = await lastMessage(page, "ai_explain");
      await result(page, ai, { ok: false, retry_ai_save: true, content: markdown,
        content_html: contentHtml, error: "模拟保存失败" });
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".pending-note .markdown h2")?.textContent), "标题");
      await click(page, ".pending-note .copy-thought");
      assert.equal(await page.evaluate(() => window.__copiedMarkdown), markdown);
      await click(page, ".pending-note .danger");
      await result(page, await lastMessage(page, "discard_ai_save"), { ok: true });
      assert.equal(await page.evaluate(() => window.__notesRoot.querySelector(".pending-note")), null);
      assert.deepEqual(requests, []);
    } finally { await page.close(); }
  });

  test(`the joint reader bridges exclude selectable note content from chapter highlights in ${mime}`, async () => {
    const html = mime === "application/xhtml+xml" ? fixture
      .replace("<!doctype html>", '<?xml version="1.0" encoding="UTF-8"?>')
      .replace("<html>", '<html xmlns="http://www.w3.org/1999/xhtml">') : fixture;
    const page = await pageWithFixture(html, { width: 1150, height: 800 }, `${mime}; charset=utf-8`);
    const chapterMessages = () => page.evaluate(() => window.__messages
      .filter((message) => message.type === "selection_changed"));
    const settleSelection = () => page.waitForTimeout(120); // Product bridge debounces for 80 ms.
    const dragShadowText = async (selector, pendingChapter = false) => {
      const points = await page.evaluate(({ selector, pendingChapter }) => {
        const node = window.__notesRoot.querySelector(selector);
        if (!node) throw new Error(`Missing selectable notes text: ${selector}`);
        node.scrollIntoView({ block: "nearest", inline: "nearest" });
        const range = document.createRange();
        range.selectNodeContents(node);
        const rects = [...range.getClientRects()];
        if (pendingChapter) {
          const chapterRange = document.createRange();
          chapterRange.selectNodeContents(document.querySelector("#last"));
          window.getSelection().removeAllRanges();
          window.getSelection().addRange(chapterRange);
          document.dispatchEvent(new Event("selectionchange"));
          window.__raceSelectionStarted = performance.now();
        }
        return { text: node.textContent,
          start: { x: rects[0].left + 0.1, y: rects[0].top + rects[0].height / 2 },
          end: { x: rects.at(-1).right - 0.1, y: rects.at(-1).top + rects.at(-1).height / 2 } };
      }, { selector, pendingChapter });
      await page.mouse.move(points.start.x, points.start.y);
      await page.mouse.down();
      await page.mouse.move(points.end.x, points.end.y, { steps: pendingChapter ? 1 : 4 });
      await page.mouse.up();
      assert.equal(await page.evaluate(() => window.getSelection().toString()), points.text);
      return points.text;
    };
    const assertNoChapterHighlight = async (before, label) => {
      await settleSelection();
      const messages = await chapterMessages();
      assert.ok(messages.length > before, `${label}: no selection update cleared the previous chapter highlight`);
      assert.equal(messages.at(-1).selected_text, "", `${label}: notes text became a chapter highlight`);
      return messages.slice(before);
    };
    try {
      await select(page, "#second");
      await settleSelection();
      const chapterQuote = "Alpha beta 😀 gamma";
      assert.equal((await chapterMessages()).at(-1).selected_text, chapterQuote);
      await click(page, '[data-action="highlight"]');
      const mark = await lastMessage(page, "highlight");
      const aiThought = "AI 想法独有内容，不能用作章节引用。";
      await result(page, mark, { ok: true, notes: [
        { id: "joint-mark", kind: "highlight", anchor: mark.anchor },
        { id: "joint-human", kind: "human_comment", anchor: mark.anchor, content: chapterQuote },
        { id: "joint-ai", kind: "ai_comment", anchor: mark.anchor, content: aiThought },
      ] });
      const markPoint = await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        const range = document.createRange();
        range.selectNodeContents(document.querySelector("#second b"));
        const rect = range.getClientRects()[0];
        return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
      });
      await page.mouse.click(markPoint.x, markPoint.y);
      await page.waitForFunction(() => window.__notesRoot.querySelector(".drawer-title").textContent === "划线相关笔记");
      await settleSelection();

      await page.evaluate(() => document.addEventListener("copy", () => {
        window.__copiedNotesText = window.getSelection().toString();
      }));
      for (const [id, thought] of [["joint-human", chapterQuote], ["joint-ai", aiThought]]) {
        if (id === "joint-human") {
          await page.evaluate(() => {
            const paragraph = document.querySelector("#second");
            const range = document.createRange();
            range.selectNodeContents(paragraph);
            window.getSelection().removeAllRanges();
            window.getSelection().addRange(range);
            paragraph.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, composed: true }));
          });
        }
        const before = (await chapterMessages()).length;
        assert.equal(await dragShadowText(`.note[data-note-id="${id}"] .content`), thought);
        if (id === "joint-human") {
          const explanationBefore = await page.evaluate(() => window.__messages
            .filter((message) => message.action === "ai_explain").length);
          const accepted = await page.evaluate((chapterQuote) => {
            window.__notesRoot.querySelector('.note[data-note-id="joint-human"] .content')
              .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, composed: true }));
            return window.ngyAnnotations.explainSelection(chapterQuote);
          }, chapterQuote);
          assert.equal(accepted, false, "A note reused the previously frozen chapter context-menu selection");
          assert.equal(await page.evaluate(() => window.__messages
            .filter((message) => message.action === "ai_explain").length), explanationBefore);
        }
        const updates = await assertNoChapterHighlight(before, id);
        assert.ok(updates.every((message) => message.selected_text === ""), `${id}: transient notes highlight leaked`);
        assert.equal(await page.evaluate(() => document.execCommand("copy")), true);
        assert.equal(await page.evaluate(() => window.__copiedNotesText), thought);
      }

      const beforeTitle = (await chapterMessages()).length;
      assert.equal(await dragShadowText(".drawer-title"), "划线相关笔记");
      await assertNoChapterHighlight(beforeTitle, "notes panel title");

      await click(page, '.note[data-note-id="joint-human"] .text-button:nth-child(2)');
      await page.keyboard.press("Control+a");
      await page.keyboard.insertText("编辑器中的人工想法");
      const beforeInput = (await chapterMessages()).length;
      await page.keyboard.press("Control+a");
      await assertNoChapterHighlight(beforeInput, "thought textarea");
      assert.equal(await page.evaluate(() => {
        const input = window.__notesRoot.querySelector("textarea");
        return input.value.slice(input.selectionStart, input.selectionEnd);
      }), "编辑器中的人工想法");
      await click(page, ".editor .cancel");

      // Start the real 80 ms chapter-selection debounce, then immediately move
      // the real mouse selection into a note before the queued send can run.
      await page.locator("#last").click();
      await settleSelection();
      const beforeRace = (await chapterMessages()).length;
      assert.equal(await dragShadowText('.note[data-note-id="joint-human"] .content', true), chapterQuote);
      const raceElapsed = await page.evaluate(() => performance.now() - window.__raceSelectionStarted);
      assert.ok(raceElapsed < 80, `The mouse transfer did not exercise the pending 80 ms debounce: ${raceElapsed} ms`);
      const raceMessages = await assertNoChapterHighlight(beforeRace, "pending chapter-to-note selection");
      assert.ok(raceMessages.every((message) => message.selected_text === ""), "The delayed send published a note or stale chapter selection");

      await click(page, ".close");
      const chapterPoints = await page.evaluate(() => {
        const range = document.createRange();
        range.selectNodeContents(document.querySelector("#first"));
        const rects = [...range.getClientRects()];
        return { start: { x: rects[0].left + 0.1, y: rects[0].top + rects[0].height / 2 },
          end: { x: rects.at(-1).right - 0.1, y: rects.at(-1).top + rects.at(-1).height / 2 } };
      });
      await page.mouse.move(chapterPoints.start.x, chapterPoints.start.y);
      await page.mouse.down();
      await page.mouse.move(chapterPoints.end.x, chapterPoints.end.y, { steps: 4 });
      await page.mouse.up();
      await settleSelection();
      assert.equal(await page.evaluate(() => window.getSelection().toString()), chapterQuote);
      assert.equal((await chapterMessages()).at(-1).selected_text, chapterQuote);
    } finally { await page.close(); }
  });
}

/// The chapter runtime owns the Ctrl + wheel gesture: it resizes the reading
/// text inside the range the host clamps, never scrolls the chapter by the same
/// notch, and reports the size it stopped at once per burst.
test("ctrl + wheel resizes the chapter text within the host range and reports it once", async () => {
  const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
  try {
    await page.route("http://epubreader.book/**", (route) => route.fulfill({
      status: 200, contentType: "text/html; charset=utf-8",
      body: `<!doctype html><html><head><style>:root{--ngy-font-size:18px}
        body{font-size:var(--ngy-font-size);margin:0;height:4000px}</style></head>
        <body><p>正文</p></body></html>`,
      headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
    }));
    await page.addInitScript({ content: `
      window.__messages = [];
      window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
      ${selectionBridge}
    ` });
    await page.goto("http://epubreader.book/chapter.html");

    const textSize = () => page.evaluate(() =>
      document.documentElement.style.getPropertyValue("--ngy-font-size"));
    const scrollY = () => page.evaluate(() => window.scrollY);
    const wheel = (deltaY, ctrlKey = true) => page.evaluate(({ deltaY, ctrlKey }) =>
      window.dispatchEvent(new WheelEvent("wheel", {
        deltaY, ctrlKey, cancelable: true, bubbles: true,
      })), { deltaY, ctrlKey });

    // A notch is one step, and Ctrl means resize rather than scroll.
    await page.evaluate(() => window.scrollTo(0, 400));
    assert.equal(await scrollY(), 400);
    await wheel(-120);
    assert.equal(await textSize(), "20px");
    assert.equal(await scrollY(), 400, "a Ctrl + wheel notch must not also scroll the chapter");

    // A plain wheel keeps scrolling and leaves the text alone.
    await wheel(-120, false);
    assert.equal(await textSize(), "20px");

    // The range is a wall, not a wrap-around, however many notches arrive.
    for (let notch = 0; notch < 20; notch += 1) await wheel(-120);
    assert.equal(await textSize(), "30px");
    for (let notch = 0; notch < 40; notch += 1) await wheel(120);
    assert.equal(await textSize(), "14px");

    const reports = () => page.evaluate(() =>
      window.__messages.filter((message) => message.type === "reader_text_size")
        .map((message) => message.font_size));
    // Every report stays inside the range the host accepts.
    await page.waitForTimeout(500);
    const settled = await reports();
    assert.ok(settled.length > 0, "the runtime must report the size it applied");
    assert.ok(settled.every((size) => size >= 14 && size <= 30), `${settled}`);
    assert.equal(settled.at(-1), 14);

    // One burst of notches leaves exactly one report, at the size it stopped at.
    const before = settled.length;
    for (let notch = 0; notch < 5; notch += 1) await wheel(-120);
    assert.equal(await textSize(), "24px");
    await page.waitForTimeout(500);
    assert.deepEqual((await reports()).slice(before), [24]);
  } finally { await page.close(); }
});
