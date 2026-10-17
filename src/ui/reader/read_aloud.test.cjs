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

// 视口起读要有得滚：章首几个段落之外垫一段空白，后面再放一段。
const scrolledFixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:22px/1.8 Arial;margin:50px}p{margin:15px 0}
</style></head><body>
<h1 id="heading">第一章</h1>
<p id="p1">Alpha paragraph.</p>
<p id="p2">Beta paragraph.</p>
<div style="height:1200px"></div>
<p id="p3">Gamma paragraph.</p>
</body></html>`;

const segment = (key, source, translated) => ({
  key, source, segments: [{ source, translated }],
});

const translationsPayload = {
  session: "read-aloud-session",
  revision: 1,
  displayMode: "translation-only",
  blocks: [
    segment("p1", "Alpha paragraph.", "甲段落。"),
    segment("p2", "Beta paragraph.", "乙段落。"),
    // 列表项是「就地插入」的一类：译文层在它内部，原文被搬进 `display:contents` 包装。
    segment("li1", "List item one", "列表项一"),
  ],
};

const scrolledPayload = {
  ...translationsPayload,
  session: "read-aloud-viewport-session",
  blocks: [...translationsPayload.blocks, segment("p3", "Gamma paragraph.", "丙段落。")],
};

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function openPage(html = fixture, payload = translationsPayload) {
  const page = await browser.newPage({ viewport: { width: 1150, height: 900 } });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200,
    contentType: "application/xhtml+xml; charset=utf-8",
    body: html,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    ${translationsSource}
    ${readAloudSource}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.evaluate((value) => window.ngyTranslations.configure(value), payload);
  await page.waitForFunction(() => document.getElementById("p1").previousElementSibling
    && document.getElementById("p1").previousElementSibling.hasAttribute("data-ngy-translation"));
  // 译文层自己会在载荷应用后同步一次；这里再叫一次，确认重复刷新是幂等的。
  await page.evaluate(() => window.ngyReadAloud.refresh());
  return page;
}

test("段落末尾的小喇叭落在读者看得见的那份文字上", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      const probe = (id) => {
        const block = document.getElementById(id);
        const sibling = block.previousElementSibling;
        const layer = sibling && sibling.hasAttribute("data-ngy-translation")
          ? sibling
          : block.querySelector(":scope > [data-ngy-translation]");
        const text = layer ? layer.querySelector(".ngy-translation-text") : null;
        return {
          inBlock: !!block.querySelector(":scope > .ngy-read-aloud-button"),
          inLayer: !!text?.querySelector(":scope > .ngy-read-aloud-button"),
          layerText: text ? text.textContent : null,
          // 小喇叭排在眼睛之后、译文层的两个动作图标之前。
          tail: text ? [...text.children].map((element) =>
            element.classList.contains("ngy-read-aloud-button") ? "speaker"
              : element.hasAttribute("data-ngy-translation-actions") ? "actions"
                : element.hasAttribute("data-ngy-translation-reveal") ? "reveal" : "other") : [],
        };
      };
      return {
        count: document.querySelectorAll(".ngy-read-aloud-button").length,
        heading: probe("heading"),
        paragraph: probe("p1"),
        listItem: probe("li1"),
        code: !!document.getElementById("code").querySelector(".ngy-read-aloud-button"),
        cell: !!document.getElementById("cell").querySelector(".ngy-read-aloud-button"),
        // 译文层给每个块复制一份同名的文本元素：它不算候选块，不能因此多出一枚按钮、
        // 把同一段读两遍。层里这三枚就是 p1/p2/li1 自己的那枚。
        layerButtons: document.querySelectorAll("[data-ngy-translation] .ngy-read-aloud-button").length,
      };
    });
    assert.equal(state.heading.inBlock, true);
    assert.equal(state.paragraph.inBlock, false, "「译文」模式下原文块被隐藏，按钮不能留在它里面");
    assert.equal(state.paragraph.inLayer, true, "按钮要落在读者看得见的译文末尾");
    assert.equal(state.paragraph.layerText, "甲段落。", "小喇叭不含文字，译文层仍只报译文");
    assert.deepEqual(state.paragraph.tail, ["reveal", "speaker", "actions"],
      "眼睛、小喇叭、编辑与重译排在同一行末尾，小喇叭在两个动作图标之前");
    assert.equal(state.listItem.inBlock, false, "列表项的原文本也被隐藏，按钮同样要跟着走");
    assert.equal(state.listItem.inLayer, true);
    assert.equal(state.listItem.layerText, "列表项一");
    assert.equal(state.code, false, "代码块不朗读");
    assert.equal(state.cell, false, "表格单元格不朗读");
    assert.equal(state.count, 5, "五个候选块各一枚按钮");
    assert.equal(state.layerButtons, 3, "译文层里的按钮就是那三个折叠段自己的，没有复制");
  } finally { await page.close(); }
});

test("read aloud sends the displayed text and the block index it started from", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(async () => {
      // 点的就是读者看到的那一枚（「译文」模式下它在译文末尾）。
      document.getElementById("p1").previousElementSibling
        .querySelector(".ngy-translation-text > .ngy-read-aloud-button").click();
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
    // 译文模式下 p1/p2/li1 读译文，其余读原文——这就是「跟随显示方式」。
    assert.deepEqual(state.texts, ["第一章", "甲段落。", "乙段落。", "列表项一", "Gamma paragraph."]);
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

test("「译文」模式的高亮落在读者看得见的译文上，且能原样清除", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      window.ngyReadAloud.highlight("1");
      const layerText = document.getElementById("p1").previousElementSibling
        .querySelector(".ngy-translation-text");
      const after = {
        layerActive: layerText.classList.contains("ngy-read-aloud-active"),
        blockActive: document.getElementById("p1").classList.contains("ngy-read-aloud-active"),
        color: getComputedStyle(layerText).backgroundColor,
        rects: layerText.getClientRects().length,
      };
      window.ngyReadAloud.clear();
      return {
        after,
        cleared: {
          layerActive: layerText.classList.contains("ngy-read-aloud-active"),
          color: getComputedStyle(layerText).backgroundColor,
          inlineColor: layerText.style.getPropertyValue("background-color"),
        },
      };
    });
    assert.equal(state.after.layerActive, true);
    assert.equal(state.after.blockActive, false, "原文块被隐藏，类挂在它上面看不见");
    // 译文层把原文的 computed style 逐条写成内联 `!important`（背景也在内），只加类
    // 压不过它：高亮必须同时写一条内联颜色。
    assert.equal(state.after.color, "rgba(255, 214, 120, 0.38)");
    assert.ok(state.after.rects > 0, "高亮的地方必须真的有盒子");
    assert.equal(state.cleared.layerActive, false);
    assert.equal(state.cleared.color, "rgba(0, 0, 0, 0)", "清除后还回译文层自己的背景");
    assert.equal(state.cleared.inlineColor, "rgba(0, 0, 0, 0)", "还原的是它原来的内联值");
  } finally { await page.close(); }
});

test("「译文」模式从视口里第一段可见的译文开始读", async () => {
  const page = await openPage(scrolledFixture, scrolledPayload);
  try {
    const state = await page.evaluate(() => {
      const layer = document.getElementById("p3").previousElementSibling;
      window.scrollTo(0, window.scrollY + layer.getBoundingClientRect().top - 40);
      window.__messages.length = 0;
      window.ngyReadAloud.startFromViewport();
      const posted = window.__messages.at(-1);
      return {
        startBlock: posted?.start_block,
        // 章首的标题与 p1/p2 已经滚到视口上方：判可见性要按译文层算，原文块在「译文」
        // 模式下没有盒子，拿它比会把每一段都判成不可见、永远回到章首。
        aboveViewport: document.getElementById("p1").previousElementSibling
          .getBoundingClientRect().bottom < 0,
      };
    });
    assert.equal(state.aboveViewport, true, "夹具必须真的把前面的段落滚出视口");
    assert.equal(state.startBlock, "3", "起点是视口里第一段可见的段落，不是章首");
  } finally { await page.close(); }
});

// 按钮在两种承载之间搬家的门禁：展开原文、收起、清层都要回到正确的宿主，且只有一枚。
test("the speaker button follows the visible text across reveal, collapse and clear", async () => {
  const page = await openPage();
  try {
    const state = await page.evaluate(() => {
      const placement = () => {
        const block = document.getElementById("p1");
        const layer = block.previousElementSibling;
        return {
          inBlock: !!block.querySelector(":scope > .ngy-read-aloud-button"),
          inLayer: !!layer.querySelector(".ngy-translation-text > .ngy-read-aloud-button"),
          count: document.querySelectorAll(".ngy-read-aloud-button").length,
        };
      };
      // 列表项是就地插入的一类：原文住在 `display:contents` 包装里，搬回去要回到包装内，
      // 落到包装外面会改变 DOM 顺序（包装必须是它的最后一个元素子节点）。
      const listPlacement = () => {
        const block = document.getElementById("li1");
        const wrapper = block.querySelector("[data-ngy-translation-original]");
        return {
          inWrapper: !!wrapper?.querySelector(":scope > .ngy-read-aloud-button"),
          wrapperIsLast: block.lastElementChild === wrapper,
          inLayer: !!block.querySelector("[data-ngy-translation] .ngy-translation-text > .ngy-read-aloud-button"),
        };
      };
      const listLayer = document.getElementById("li1").querySelector("[data-ngy-translation]");
      const listReveal = listLayer.querySelector("[data-ngy-translation-reveal]");
      const listCollapsed = listPlacement();
      listReveal.click();
      const listRevealed = listPlacement();
      const layer = document.getElementById("p1").previousElementSibling;
      const reveal = layer.querySelector("[data-ngy-translation-reveal]");
      const collapsed = placement();
      reveal.click();
      const revealed = placement();
      reveal.click();
      const collapsedAgain = placement();
      window.ngyTranslations.clear();
      return { collapsed, revealed, collapsedAgain, cleared: placement(), listCollapsed, listRevealed };
    });
    assert.deepEqual(state.listCollapsed, { inWrapper: false, wrapperIsLast: true, inLayer: true });
    assert.deepEqual(state.listRevealed, { inWrapper: true, wrapperIsLast: true, inLayer: false },
      "列表项展开原文后按钮回到原文包装里，包装仍是最后一个元素子节点");
    assert.deepEqual(state.collapsed, { inBlock: false, inLayer: true, count: 5 });
    assert.deepEqual(state.revealed, { inBlock: true, inLayer: false, count: 5 },
      "展开原文后按钮回到正文块，不能让一段有两个入口");
    assert.deepEqual(state.collapsedAgain, { inBlock: false, inLayer: true, count: 5 });
    assert.deepEqual(state.cleared, { inBlock: true, inLayer: false, count: 5 },
      "层被清掉后按钮回到正文块");

    // 「原文」模式推的是空载荷：旧层（连同层里的按钮）被拆掉，按钮要还给正文块，
    // 否则整章都没有朗读入口。
    const originalOnly = await page.evaluate((payload) => {
      window.ngyTranslations.configure(payload);
      window.ngyTranslations.configure({
        session: "original-only-session", revision: 1, displayMode: "original-only", blocks: [],
      });
      return {
        layers: document.querySelectorAll("[data-ngy-translation]").length,
        inBlock: !!document.getElementById("p1").querySelector(":scope > .ngy-read-aloud-button"),
        count: document.querySelectorAll(".ngy-read-aloud-button").length,
      };
    }, translationsPayload);
    assert.deepEqual(originalOnly, { layers: 0, inBlock: true, count: 5 },
      "空载荷拆层后小喇叭回到正文块");
  } finally { await page.close(); }
});

// 读者真的能碰到它吗：小喇叭默认透明且不吃指针事件，只有悬停在它所在的那份文字上才出现。
test("悬停可见文字时小喇叭出现，移开后收起", async () => {
  const page = await openPage();
  try {
    // 「译文」模式下原文块没有盒子，按钮在译文层里：按可见的那份文字量它的透明度。
    const opacityInLayer = (id) => page.evaluate((id) => {
      const block = document.getElementById(id);
      const layer = block.previousElementSibling.hasAttribute("data-ngy-translation")
        ? block.previousElementSibling
        : block.querySelector("[data-ngy-translation]");
      return getComputedStyle(layer.querySelector(".ngy-translation-text > .ngy-read-aloud-button"))
        .opacity;
    }, id);
    const hoverPoint = await page.evaluate(() => {
      const text = document.getElementById("p1").previousElementSibling
        .querySelector(".ngy-translation-text");
      const rect = text.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    });
    assert.equal(await opacityInLayer("p1"), "0", "默认不显示");
    await page.mouse.move(hoverPoint.x, hoverPoint.y);
    await page.waitForFunction(() => getComputedStyle(document.getElementById("p1")
      .previousElementSibling.querySelector(".ngy-translation-text > .ngy-read-aloud-button"))
      .opacity === "0.5", undefined, { timeout: 2000 });
    await page.mouse.move(hoverPoint.x, 0);
    await page.waitForFunction(() => getComputedStyle(document.getElementById("p1")
      .previousElementSibling.querySelector(".ngy-translation-text > .ngy-read-aloud-button"))
      .opacity === "0", undefined, { timeout: 2000 });

    // 展开原文后承载元素是块内的原文包装（`display:contents`，自己不生成盒子、
    // 收不到 mouseenter），悬停由块来接。
    const listPoint = await page.evaluate(() => {
      document.getElementById("li1").querySelector("[data-ngy-translation-reveal]").click();
      const rect = document.getElementById("li1").getBoundingClientRect();
      return { x: rect.x + 20, y: rect.y + rect.height / 2 };
    });
    await page.mouse.move(listPoint.x, listPoint.y);
    await page.waitForFunction(
      () => getComputedStyle(document.getElementById("li1")
        .querySelector("[data-ngy-translation-original] > .ngy-read-aloud-button")).opacity === "0.5",
      undefined, { timeout: 2000 });
  } finally { await page.close(); }
});
