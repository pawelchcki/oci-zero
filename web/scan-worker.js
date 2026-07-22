import init, { scan_layer_stream } from "./pkg/oci_zero_web.js?v=20260718-3";

const initWasm = () => init({
  module_or_path: new URL("./pkg/oci_zero_web_bg.wasm?v=20260718-3", import.meta.url),
});
await initWasm();
self.postMessage({ type: "ready" });

let current = null;

self.addEventListener("message", (event) => {
  const message = event.data;
  if (message?.type === "scan") {
    if (current) {
      self.postMessage({
        type: "error",
        jobId: message.jobId,
        error: "the scan worker is already processing a layer",
      });
      return;
    }
    const job = { jobId: message.jobId, pendingChunk: null };
    current = job;
    void runScan(job, message);
    return;
  }
  if (!current || message?.jobId !== current.jobId || !current.pendingChunk) return;
  const pending = current.pendingChunk;
  current.pendingChunk = null;
  if (message.type === "chunk") {
    pending.resolve(new Uint8Array(message.bytes));
  } else if (message.type === "end") {
    pending.resolve(null);
  } else if (message.type === "source_error") {
    pending.reject(new Error(message.error));
  }
});

async function runScan(job, message) {
  try {
    await scan_layer_stream(
      message.mediaType,
      message.digest,
      message.size,
      message.diffId,
      () => nextChunk(job),
      (events) => self.postMessage({ type: "events", jobId: job.jobId, events }),
    );
    self.postMessage({ type: "complete", jobId: job.jobId });
  } catch (error) {
    self.postMessage({ type: "error", jobId: job.jobId, error: errorMessage(error) });
  } finally {
    if (current === job) current = null;
  }
}

function nextChunk(job) {
  if (current !== job) return Promise.reject(new Error("layer scan was cancelled"));
  if (job.pendingChunk) return Promise.reject(new Error("layer scanner requested concurrent chunks"));
  return new Promise((resolve, reject) => {
    job.pendingChunk = { resolve, reject };
    self.postMessage({ type: "pull", jobId: job.jobId });
  });
}

function errorMessage(error) {
  return error?.message || String(error);
}
