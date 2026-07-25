import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const [html, css, app, worker, glue, wasm] = await Promise.all([
  readFile(join(root, "index.html"), "utf8"),
  readFile(join(root, "style.css"), "utf8"),
  readFile(join(root, "app.js"), "utf8"),
  readFile(join(root, "scan-worker.js"), "utf8"),
  readFile(join(root, "pkg/oci_zero_web.js"), "utf8"),
  readFile(join(root, "pkg/oci_zero_web_bg.wasm")),
]);

const wasmBase64 = wasm.toString("base64");
const inlineGlue = glue
  .replace(/^export function /gm, "function ")
  .replace(
    /^export \{ initSync, __wbg_init as default \};\s*$/m,
    "const init = __wbg_init;",
  );
const wasmBootstrap = `
const INLINE_WASM_BASE64 = ${JSON.stringify(wasmBase64)};
function inlineWasmBytes() {
  const encoded = atob(INLINE_WASM_BASE64);
  const bytes = new Uint8Array(encoded.length);
  for (let index = 0; index < encoded.length; index += 1) bytes[index] = encoded.charCodeAt(index);
  return bytes;
}
`;
const workerSource = [
  inlineGlue,
  wasmBootstrap,
  withoutImports(worker).replace("await initWasm();", "await init({ module_or_path: inlineWasmBytes() });"),
].join("\n");
const workerBootstrap = `
const INLINE_SCAN_WORKER_SOURCE = ${JSON.stringify(workerSource)};
const INLINE_SCAN_WORKER_URL = URL.createObjectURL(
  new Blob([INLINE_SCAN_WORKER_SOURCE], { type: "text/javascript" }),
);
`;
const appSource = [
  inlineGlue,
  wasmBootstrap,
  workerBootstrap,
  withoutImports(app)
    .replace("await initWasm();", "await init({ module_or_path: inlineWasmBytes() });")
    .replace(
      'new Worker(new URL("./scan-worker.js?v=20260718-3", import.meta.url), { type: "module" })',
      'new Worker(INLINE_SCAN_WORKER_URL, { type: "module" })',
    ),
].join("\n");

// Replacement *functions*, not strings: a `$&` anywhere in the inlined sources
// would otherwise be expanded into the matched text, splicing a stray tag into
// the middle of the bundle.
const bundled = html
  .replace(/\s*<link rel="stylesheet" href="style\.css">/, () => `\n    <style>\n${css}\n    </style>`)
  .replace(
    /\s*<script type="module" src="app\.js"><\/script>/,
    () => `\n    <script type="module">\n${appSource.replaceAll("</script", "<\\/script")}\n    </script>`,
  );
const output = join(root, "dist/proxyless.html");
await mkdir(dirname(output), { recursive: true });
await writeFile(output, bundled);
process.stdout.write(`Built ${output} (${bundled.length} bytes)\n`);

function withoutImports(source) {
  return source.replace(/^import[\s\S]*?from\s+"\.\/pkg\/oci_zero_web\.js(?:\?[^\"]*)?";\s*/m, "");
}
