import { initSync, dispatch } from "./pkg/oci_zero_registry_demo.js";
import wasm from "./pkg/oci_zero_registry_demo_bg.wasm";
import { registryFetch } from "./http.mjs";

// Wrangler imports .wasm as a compiled WebAssembly.Module. Initialize once per
// isolate; requests reuse the ROM-backed Rust store.
initSync({ module: wasm });

export default {
  fetch(request) {
    return registryFetch(request, dispatch);
  },
};
