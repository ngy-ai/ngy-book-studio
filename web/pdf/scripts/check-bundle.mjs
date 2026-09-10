import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const project = resolve(here, "../../..");
const output = join(project, "assets", "pdfjs");
const manifest = JSON.parse(await readFile(join(output, "manifest.json"), "utf8"));
if (manifest.pdfjsVersion !== "5.7.284") throw new Error("unexpected PDF.js version");
for (const entry of manifest.files) {
  const bytes = await readFile(join(output, ...entry.path.split("/")));
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (bytes.length !== entry.bytes || digest !== entry.sha256) throw new Error(`asset mismatch: ${entry.path}`);
}
const viewer = await readFile(join(output, "viewer.mjs"), "utf8");
for (const contract of [
  "isEvalSupported: false",
  "enableXfa: false",
  "disableAutoFetch: true",
  "disableRange: true",
  "disableStream: true",
  "useWorkerFetch: false",
  "AnnotationMode.DISABLE",
  "page.getTextContent(",
  "new TextLayer(",
  "moye-pdf-selection-changed",
  "moye-pdf-request-page",
  "MAX_SELECTION_BYTES = 32 * 1024",
]) {
  if (!viewer.includes(contract)) throw new Error(`missing PDF viewer contract: ${contract}`);
}
for (const forbidden of ["AnnotationLayer", ".getAnnotations(", "enableXfa: true", "isEvalSupported: true"]) {
  if (viewer.includes(forbidden)) throw new Error(`forbidden PDF viewer capability: ${forbidden}`);
}
if (/https?:\/\//i.test(viewer)) throw new Error("PDF viewer contains a remote URL");
const html = await readFile(join(output, "viewer.html"), "utf8");
if (!html.includes("connect-src 'self'")) throw new Error("PDF viewer CSP allows external network connections");
if (/connect-src[^;]*(?:https?:|\*)/i.test(html)) throw new Error("PDF viewer CSP allows a remote connection target");
