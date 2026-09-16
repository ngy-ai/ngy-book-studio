import { readFile } from "node:fs/promises";
import { Script } from "node:vm";
import { build } from "esbuild";
import { buildOptions, outputFile } from "./build-options.mjs";

const [expected, result] = await Promise.all([
  readFile(outputFile),
  build({ ...buildOptions, write: false }),
]);
const actual = result.outputFiles[0]?.contents;

if (!actual || !expected.equals(actual)) {
  throw new Error(
    "assets/editor/prosemirror.js is stale; run `npm run build` in web/editor and commit the result.",
  );
}

const documentStartCallbacks = [];
new Script(expected.toString("utf8")).runInNewContext({
  document: {
    readyState: "loading",
    addEventListener(name, callback, options) {
      documentStartCallbacks.push({ name, callback, options });
    },
  },
});

const bundleSource = expected.toString("utf8");
for (const marker of [
  "__ngyEditorTrustedShell",
  "epubeditor.shell",
  "epubeditor.content",
  'searchParams.set("mode","source")',
  'credentials:"omit"',
  'redirect:"error"',
]) {
  if (!bundleSource.includes(marker)) {
    throw new Error(`the trusted-shell/content-origin boundary is missing ${marker}`);
  }
}

if (
  documentStartCallbacks.length !== 1 ||
  documentStartCallbacks[0].name !== "DOMContentLoaded" ||
  typeof documentStartCallbacks[0].callback !== "function" ||
  documentStartCallbacks[0].options?.once !== true
) {
  throw new Error(
    "the complete ProseMirror bundle must defer until DOMContentLoaded",
  );
}

console.log("ProseMirror bundle is reproducible and up to date.");
