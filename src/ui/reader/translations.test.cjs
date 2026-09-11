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

const fixture = `<!DOCTYPE html><html><head><style>
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
    { key: "a", source: "Alpha", segments: [{ source: "Alpha", translated: "甲一" }] },
    { key: "b", source: "Alpha", segments: [{ source: "Alpha", translated: "甲二" }] },
    { key: "c", source: "Beta", segments: [{ source: "Beta", translated: "乙" }] },
    { key: "nested", source: "Nested item", segments: [{ source: "Nested item", translated: "嵌套" }] },
    { key: "quote", source: "Quoted", segments: [{ source: "Quoted", translated: "引用" }] },
    { key: "cell", source: "Cell", segments: [{ source: "Cell", translated: "单元格" }] },
    { key: "after", source: "Tail paragraph", segments: [{ source: "Tail paragraph", translated: "尾段" }] },
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

async function pageWithFixture(html = fixture, contentType = "text/html; charset=utf-8") {
  const page = await browser.newPage({ viewport: { width: 1150, height: 900 } });
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
        listItemTranslations: document.getElementById("li").querySelectorAll("[data-moye-translation]").length,
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
    assert.equal(state.listItemTranslations, 1, "the nested paragraph creates exactly one translation inside its list item");
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
    assert.equal(reported, "Alpha Alpha", "the selection bridge excludes translation text and keeps paragraph whitespace");

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
    assert.equal(noteQuote, "Alpha Alpha", "annotation anchors exclude translation text");

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

const formattedFixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:18px/1.6 Georgia;margin:24px;max-width:850px}
#heading{font-size:38px;font-weight:800;color:rgb(13,65,99);text-align:center}
#heading em{font-size:31px;color:rgb(81,14,113)}
#rich{text-indent:31px;text-align:right;margin-left:17px;line-height:32px;color:rgb(23,45,67)}
#rich strong{font-weight:900}#rich em{font-style:italic}#rich .tone{color:rgb(164,23,78)}
#rich code{font:15px monospace;background-color:rgb(223,231,239)}
#quote{border-left:7px solid rgb(77,88,99);padding-left:19px;margin-left:27px;font-style:italic}
#numbered{list-style-type:upper-roman}td,th{border:2px solid black;padding:12px}
#cell-strong{font-weight:900;color:rgb(22,120,45)}
</style></head><body>
<h2 id="heading">Styled <em>heading</em></h2>
<p id="rich">Alpha <strong>bold</strong><br /><em>italic</em> <del>deleted</del> <sub>down</sub><sup>up</sup> <code id="code-node">a &lt; b</code> <a href="https://invalid.example/link" onclick="window.__unsafe=1">link</a><span class="tone">color</span></p>
<blockquote id="quote">Quote <strong>text</strong></blockquote>
<ol id="numbered" start="5"><li id="one" value="7">First <em>item</em></li><li id="two">Second</li></ol>
<ul id="bulleted"><li id="bullet">Bullet <strong>item</strong></li></ul>
<table><tbody><tr><th id="header" colspan="2">Header</th></tr><tr><td id="table-cell"><strong id="cell-strong">Cell</strong> body</td><td>Untranslated</td></tr></tbody></table>
<p id="media">Look <img id="original-image" alt="Book illustration" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/%3E" /> image</p>
<p id="after-list">After list</p>
</body></html>`;

const segmentBlock = (key, source, pairs) => ({
  key, source, segments: pairs.map(([source, translated]) => ({ source, translated })),
});
const formattedPayload = {
  session: "formatted-session",
  displayMode: "translation-only",
  blocks: [
    segmentBlock("heading", "Styled heading", [["Styled ", "格式"], ["heading", "标题"]]),
    segmentBlock("rich", "Alpha bolditalic deleted downup a < b linkcolor", [
      ["Alpha ", "正文"], ["bold", "粗体"], ["italic", "斜体"], ["deleted", "删除"],
      ["down", "下标"], ["up", "上标"], ["link", '<img src="https://invalid.example/evil" onerror="window.__unsafe=1" />'],
      ["color", "彩色"],
    ]),
    segmentBlock("quote", "Quote text", [["Quote ", "引用"], ["text", "文字"]]),
    segmentBlock("one", "First item", [["First ", "第一"], ["item", "项"]]),
    segmentBlock("two", "Second", [["Second", "第二项"]]),
    segmentBlock("bullet", "Bullet item", [["Bullet ", "无序"], ["item", "项"]]),
    segmentBlock("header", "Header", [["Header", "表头"]]),
    segmentBlock("table-cell", "Cell body", [["Cell", "单元格"], [" body", "内容"]]),
    segmentBlock("media", "Look image", [["Look ", "查看"], [" image", "插图"]]),
    segmentBlock("after-list", "After list", [["After list", "列表之后"]]),
  ],
};

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: source formatting, list markers, cells and code survive structured translation safely`, async () => {
    const page = await pageWithFixture(formattedFixture, contentType);
    try {
      const before = await page.evaluate(() => {
        window.__originalLiNodes = Array.from(document.getElementById("one").childNodes);
        window.__originalImage = document.getElementById("original-image");
        const result = {};
        for (const [name, selector, properties] of [
          ["heading", "#heading", ["fontSize", "fontWeight", "color", "textAlign"]],
          ["headingEm", "#heading em", ["fontSize", "fontStyle", "color"]],
          ["rich", "#rich", ["textAlign", "textIndent", "marginLeft", "lineHeight", "color"]],
          ["strong", "#rich strong", ["fontWeight"]],
          ["em", "#rich em", ["fontStyle"]],
          ["del", "#rich del", ["textDecorationLine"]],
          ["sub", "#rich sub", ["verticalAlign", "fontSize"]],
          ["sup", "#rich sup", ["verticalAlign", "fontSize"]],
          ["code", "#rich code", ["fontFamily", "fontSize", "backgroundColor"]],
          ["tone", "#rich .tone", ["color"]],
          ["quote", "#quote", ["borderLeftWidth", "borderLeftStyle", "paddingLeft", "marginLeft"]],
          ["cellStrong", "#cell-strong", ["fontWeight", "color"]],
        ]) {
          const style = getComputedStyle(document.querySelector(selector));
          result[name] = Object.fromEntries(properties.map((property) => [property, style[property]]));
        }
        return result;
      });
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), formattedPayload);
      const state = await page.evaluate((expected) => {
        const translation = (id) => {
          const original = document.getElementById(id);
          const layer = ["li", "td", "th"].includes(original.localName)
            ? original.firstElementChild : original.previousElementSibling;
          return layer.querySelector(".moye-translation-text");
        };
        const nodes = {
          heading: translation("heading"), headingEm: translation("heading").querySelector("em"),
          rich: translation("rich"), strong: translation("rich").querySelector("strong"),
          em: translation("rich").querySelector("em"), del: translation("rich").querySelector("del"),
          sub: translation("rich").querySelector("sub"), sup: translation("rich").querySelector("sup"),
          code: translation("rich").querySelector("code"), tone: translation("rich").lastElementChild,
          quote: translation("quote"), cellStrong: translation("table-cell").querySelector("strong"),
        };
        const styles = Object.fromEntries(Object.entries(nodes).map(([name, element]) => {
          const style = getComputedStyle(element);
          return [name, Object.fromEntries(Object.keys(expected[name]).map((property) => [property, style[property]]))];
        }));
        const rich = translation("rich");
        return {
          styles, applied: window.moyeTranslations.applied(), headingTag: nodes.heading.localName,
          headingNamespace: nodes.heading.namespaceURI,
          headingText: nodes.heading.textContent, boldText: nodes.strong.textContent,
          brCount: rich.querySelectorAll("br").length, code: nodes.code.textContent,
          text: rich.textContent, active: rich.querySelectorAll("a,img,script,iframe,[id],[href],[onclick],[onerror]").length,
          liCount: document.querySelectorAll("li").length,
          olChildren: Array.from(document.getElementById("numbered").children, (node) => node.localName),
          olStart: document.getElementById("numbered").start, liValue: document.getElementById("one").value,
          liDisplay: getComputedStyle(document.getElementById("one")).display,
          liMarker: getComputedStyle(document.getElementById("one")).listStyleType,
          liTranslatedText: translation("one").textContent,
          liOriginalHidden: document.getElementById("one").lastElementChild.style.display,
          liOriginalIdentity: window.__originalLiNodes.every((node, index) =>
            document.getElementById("one").lastElementChild.childNodes[index] === node),
          headerColumns: document.getElementById("header").colSpan,
          cellDisplay: getComputedStyle(document.getElementById("table-cell")).display,
          cellText: translation("table-cell").textContent,
          cellCollapsed: translation("table-cell").parentElement.getAttribute("data-moye-collapsed"),
          mediaVisible: getComputedStyle(document.getElementById("media")).display !== "none",
          imageIdentity: document.getElementById("original-image") === window.__originalImage,
          mediaCopies: document.querySelectorAll("img").length,
        };
      }, before);
      assert.deepEqual(state.styles, before, "computed formatting survives ID-specific source styles and inheritance");
      assert.equal(state.applied, 10);
      assert.equal(state.headingTag, "h2");
      assert.equal(state.headingNamespace, "http://www.w3.org/1999/xhtml");
      assert.equal(state.headingText, "格式 标题");
      assert.equal(state.boldText, "粗体");
      assert.equal(state.brCount, 1);
      assert.equal(state.code, "a < b", "code text remains exactly as authored");
      assert.ok(state.text.includes('<img src="https://invalid.example/evil"'), "model markup is visible plain text");
      assert.equal(state.active, 0, "no source links, source IDs or model HTML become active nodes");
      assert.equal(state.liCount, 3);
      assert.deepEqual(state.olChildren, ["li", "li"], "no div is inserted directly into the list");
      assert.equal(state.olStart, 5);
      assert.equal(state.liValue, 7);
      assert.equal(state.liDisplay, "list-item", "translation-only mode preserves the real list marker");
      assert.equal(state.liMarker, "upper-roman");
      assert.equal(state.liTranslatedText, "第一 项");
      assert.equal(state.liOriginalHidden, "none");
      assert.equal(state.liOriginalIdentity, true);
      assert.equal(state.headerColumns, 2);
      assert.equal(state.cellDisplay, "table-cell");
      assert.equal(state.cellText, "单元格 内容");
      assert.equal(state.cellCollapsed, "0", "table cells stay bilingual");
      assert.equal(state.mediaVisible, true, "translation-only preference never hides the original illustration");
      assert.equal(state.imageIdentity, true);
      assert.equal(state.mediaCopies, 1);

      const restored = await page.evaluate(() => {
        window.moyeTranslations.clear();
        return {
          count: document.querySelectorAll("[data-moye-translation]").length,
          nodesRestored: window.__originalLiNodes.every((node, index) => document.getElementById("one").childNodes[index] === node),
          paragraphStyle: document.getElementById("rich").getAttribute("style"),
          liText: document.getElementById("one").textContent,
          imageSame: document.getElementById("original-image") === window.__originalImage,
          sourceLink: document.querySelector("#rich a").getAttribute("href"),
        };
      });
      assert.deepEqual(restored, {
        count: 0, nodesRestored: true, paragraphStyle: null, liText: "First item",
        imageSame: true, sourceLink: "https://invalid.example/link",
      }, "clear restores the original DOM nodes and display state");
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), formattedPayload);
      assert.equal(await page.evaluate(() => window.moyeTranslations.applied()), 10, "clear/reapply does not accumulate wrappers");
    } finally { await page.close(); }
  });

  test(`${mode}: stale, reordered, missing and legacy leaves keep their original paragraphs visible`, async () => {
    const page = await pageWithFixture(`<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Validation</title></head><body><p id="wrong">One <b>two</b></p><p id="missing">Three <i>four</i></p><p id="legacy">Legacy</p><p id="empty">Empty</p><p id="skipped">Visible<script>ignored()</script><style>.ignored{color:red}</style><span> text</span></p><p id="code">Run <code>print("ok")</code> now</p></body></html>`, contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "invalid-session", displayMode: "translation-only", blocks: [
          segmentBlock("wrong", "One two", [["two", "二"], ["One", "一"]]),
          segmentBlock("missing", "Three four", [["Three", "三"]]),
          { key: "legacy", source: "Legacy", translated: "旧格式" },
          segmentBlock("empty", "Empty", [["Empty", "  "]]),
          segmentBlock("skipped", "Visible text", [["Visible", "可见"], ["text", "正文"]]),
          segmentBlock("code", 'Run print("ok") now', [["Run", "运行"], ["now", "现在"]]),
        ],
      });
      const result = await page.evaluate(() => ({
        applied: window.moyeTranslations.applied(),
        unchanged: ["wrong", "missing", "legacy", "empty"].every((id) => document.getElementById(id).style.display !== "none"),
        skipped: document.getElementById("skipped").previousElementSibling.textContent,
        code: document.getElementById("code").previousElementSibling.querySelector("code").textContent,
        codeDisplay: document.getElementById("code").style.display,
      }));
      assert.equal(result.applied, 2);
      assert.equal(result.unchanged, true);
      assert.equal(result.skipped, "可见 正文");
      assert.equal(result.code, 'print("ok")');
      assert.equal(result.codeDisplay, "none");
    } finally { await page.close(); }
  });
}


for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: wrapping list originals preserves note offsets and excludes translated selections`, async () => {
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Anchors</title><style>body{font:24px/1.8 Arial;margin:30px}</style></head><body><p id="before">Before</p><ol start="4"><li id="list">A <strong>B</strong></li></ol><p id="end">End</p></body></html>', contentType);
    try {
      const readAnchor = async () => {
        await page.evaluate(() => {
          const range = document.createRange();
          range.selectNodeContents(document.getElementById("end"));
          window.getSelection().removeAllRanges();
          window.getSelection().addRange(range);
          document.dispatchEvent(new Event("selectionchange"));
        });
        await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
        return page.evaluate(() => {
          window.__notesRoot.querySelector('[data-action="highlight"]').click();
          const message = window.__messages.filter((item) => item.action === "highlight").at(-1);
          window.moyeAnnotations.result({
            session: message.session, revision: message.revision, request_id: message.request_id, ok: true, notes: [],
          });
          return message.anchor;
        });
      };
      const baseline = await readAnchor();
      assert.deepEqual(baseline, { quote: "End", start: 8, end: 11 });
      await page.evaluate((payload) => {
        window.getSelection().removeAllRanges();
        window.moyeTranslations.configure(payload);
      }, {
        session: "list-session", displayMode: "translation-only",
        blocks: [segmentBlock("list", "A B", [["A ", "甲"], ["B", "乙"]])],
      });
      assert.deepEqual(await readAnchor(), baseline, "the original hidden list leaves retain their UTF-16 offsets");
      await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        const text = document.getElementById("list").querySelector(".moye-translation-text");
        const range = document.createRange();
        range.selectNodeContents(text);
        window.getSelection().addRange(range);
        document.dispatchEvent(new Event("selectionchange"));
      });
      await page.waitForTimeout(160);
      const selection = await page.evaluate(() => ({
        reported: window.__messages.filter((item) => item.type === "selection_changed").at(-1)?.selected_text,
        toolbarHidden: window.__notesRoot.querySelector(".toolbar").hidden,
      }));
      assert.deepEqual(selection, { reported: "", toolbarHidden: true });
      await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        document.getElementById("list").querySelector(".moye-translation-text").click();
      });
      assert.equal(await page.evaluate(() => document.getElementById("list").lastElementChild.style.display), "contents", "click reveals original list text without hiding its marker");
      await page.evaluate(() => window.moyeTranslations.clear());
      assert.deepEqual(await readAnchor(), baseline, "clear restores list originals without shifting later notes");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: code descendants and code-only blocks do not consume repeated prose translations`, async () => {
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Code boundaries</title></head><body><pre><code><p id="inside-code">Same</p></code></pre><p id="only-code"><code>Same</code></p><p id="prose-first">Same</p><p id="prose-second">Same</p><ul><li id="code-container">Lead <pre><p id="nested-code">code()</p></pre> tail</li></ul></body></html>', contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "code-boundary-session", displayMode: "translation-only", blocks: [
          segmentBlock("first", "Same", [["Same", "第一处正文"]]),
          segmentBlock("second", "Same", [["Same", "第二处正文"]]),
          segmentBlock("container", "Lead code() tail", [["Lead ", "之前"], [" tail", "之后"]]),
        ],
      });
      const state = await page.evaluate(() => ({
        count: window.moyeTranslations.applied(),
        insideCode: document.getElementById("inside-code").style.display,
        codeOnly: document.getElementById("only-code").style.display,
        codeLayerCount: document.querySelectorAll("pre [data-moye-translation],code [data-moye-translation]").length,
        first: document.getElementById("prose-first").previousElementSibling.querySelector(".moye-translation-text")?.textContent,
        second: document.getElementById("prose-second").previousElementSibling.querySelector(".moye-translation-text")?.textContent,
        container: document.getElementById("code-container").firstElementChild.querySelector(".moye-translation-text")?.textContent,
        preservedCode: document.getElementById("code-container").firstElementChild.querySelector("pre")?.textContent,
      }));
      assert.deepEqual(state, {
        count: 3, insideCode: "", codeOnly: "", codeLayerCount: 0,
        first: "第一处正文", second: "第二处正文", container: "之前 code() 之后", preservedCode: "code()",
      }, "candidate and duplicate-source ordering match the host's skipped code subtrees");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: invisible-format-only leaves are not translation slots on either side`, async () => {
    // 宿主现场回归：代码用 ZWSP 缩进，ZWSP 不属于 ECMAScript `\s`。旧叶子列表把它
    // 当成片段，模型只能回空白，整块被 `empty_segment_text` 拒绝。
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Invisible leaves</title></head><body><p id="code-line"><span id="indent">\u200b\u200b</span><span>const</span> THREE_AND_A_BIT : f32 = 3.4028236;</p><p id="blank">\u200b\u00ad</p></body></html>', contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "invisible-leaf-session", displayMode: "translation-only", blocks: [
          segmentBlock("code-line", "\u200b\u200bconst THREE_AND_A_BIT : f32 = 3.4028236;", [
            ["const", "常量"],
            [" THREE_AND_A_BIT : f32 = 3.4028236;", "三分之一个字节的常量"],
          ]),
        ],
      });
      const state = await page.evaluate(() => ({
        applied: window.moyeTranslations.applied(),
        layers: document.querySelectorAll("[data-moye-translation]").length,
        indent: document.getElementById("indent").textContent,
        translated: document.getElementById("code-line").previousElementSibling
          ?.querySelector(".moye-translation-text")?.textContent,
      }));
      assert.deepEqual(state, {
        applied: 1, layers: 1,
        indent: "\u200b\u200b",
        translated: "\u200b\u200b常量 三分之一个字节的常量",
      }, "the invisible indentation keeps its node and only visible leaves are replaced");
    } finally { await page.close(); }
  });
}
