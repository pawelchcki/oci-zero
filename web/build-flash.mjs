// Bundles flash.html into a single self-contained file, the same way
// build-proxyless.mjs does for index.html: the WASM is inlined as base64, the
// glue and esptool-js keep their source but lose their import statements, and
// the stylesheet is inlined. The result is one file that can be opened from
// disk, which matters because flashing must work without a server.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const [html, css, app, glue, wasm, esptool] = await Promise.all([
  readFile(join(root, "flash.html"), "utf8"),
  readFile(join(root, "style.css"), "utf8"),
  readFile(join(root, "flash.js"), "utf8"),
  readFile(join(root, "pkg/oci_zero_web.js"), "utf8"),
  readFile(join(root, "pkg/oci_zero_web_bg.wasm")),
  readFile(join(root, "node_modules/esptool-js/bundle.js"), "utf8"),
]);

const inlineGlue = glue
  .replace(/^export function /gm, "function ")
  .replace(
    /^export \{ initSync, __wbg_init as default \};\s*$/m,
    "const init = __wbg_init;",
  );

const wasmBootstrap = `
const INLINE_WASM_BASE64 = ${JSON.stringify(wasm.toString("base64"))};
function inlineWasmBytes() {
  const encoded = atob(INLINE_WASM_BASE64);
  const bytes = new Uint8Array(encoded.length);
  for (let index = 0; index < encoded.length; index += 1) bytes[index] = encoded.charCodeAt(index);
  return bytes;
}
`;

// esptool-js ships a single-file ESM bundle whose only export statement is the
// last one. Rewriting it into plain `const` bindings is enough to splice the
// module into an inline script.
const esptoolExports = /export\s*\{([^}]*)\};?\s*$/.exec(esptool);
if (!esptoolExports) {
  throw new Error("esptool-js/bundle.js no longer ends in an export statement; update build-flash.mjs");
}
const exported = esptoolExports[1]
  .split(",")
  .map((entry) => entry.trim())
  .filter(Boolean)
  .map((entry) => {
    const [local, name = local] = entry.split(/\s+as\s+/).map((part) => part.trim());
    return { local, name };
  });
for (const required of ["ESPLoader", "Transport"]) {
  if (!exported.some((binding) => binding.name === required)) {
    throw new Error(`esptool-js/bundle.js no longer exports ${required}; update build-flash.mjs`);
  }
}
// Wrapped in a function scope rather than concatenated flat. The bundle is
// minified, so its top-level names are single letters and `$` — exactly the
// names the WASM glue and this page use. Only the named exports escape.
const inlineEsptool = `
const { ${exported.map((binding) => binding.name).join(", ")} } = (() => {
${esptool.slice(0, esptoolExports.index)}
return { ${exported.map(({ local, name }) => `${name}: ${local}`).join(", ")} };
})();
`;

const appSource = [
  inlineGlue,
  wasmBootstrap,
  inlineEsptool,
  app
    .replace(/^import[^\n]*from\s+"\.\/pkg\/oci_zero_web\.js(?:\?[^"]*)?";\s*$/m, "")
    .replace(/^import\s+\{[^}]*\}\s+from\s+"\.\/node_modules\/esptool-js\/bundle\.js";\s*$/m, "")
    .replace(
      /await init\(\{ module_or_path: new URL\([^)]*\) \}\);/,
      "await init({ module_or_path: inlineWasmBytes() });",
    ),
].join("\n");

for (const [label, pattern] of [
  ["the WASM glue import", /from "\.\/pkg\/oci_zero_web\.js/],
  ["the esptool-js import", /from "\.\/node_modules\/esptool-js/],
  ["the WASM URL bootstrap", /new URL\("\.\/pkg\/oci_zero_web_bg\.wasm/],
]) {
  if (pattern.test(appSource)) {
    throw new Error(`build-flash.mjs failed to rewrite ${label}; the bundle would fetch from the network`);
  }
}

// Replacement *functions*, not strings: minified third-party code contains `$&`,
// which String.replace would expand into the matched text and splice a stray
// `<script>` tag into the middle of the bundle.
const bundled = html
  .replace(/\s*<link rel="stylesheet" href="style\.css">/, () => `\n    <style>\n${css}\n    </style>`)
  .replace(
    /\s*<script type="module" src="flash\.js"><\/script>/,
    () => `\n    <script type="module">\n${appSource.replaceAll("</script", "<\\/script")}\n    </script>`,
  );

const output = join(root, "dist/flash.html");
await mkdir(dirname(output), { recursive: true });
await writeFile(output, bundled);
process.stdout.write(`Built ${output} (${bundled.length} bytes)\n`);
