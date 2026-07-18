chrome.action.onClicked.addListener(() => {
  chrome.tabs.create({ url: chrome.runtime.getURL("index.html") });
});

chrome.webRequest.onBeforeRedirect.addListener(
  (details) => {
    const extensionOrigin = `chrome-extension://${chrome.runtime.id}`;
    if (details.initiator !== extensionOrigin) return;
    let origin;
    try {
      origin = new URL(details.redirectUrl).origin;
    } catch (_) {
      return;
    }
    chrome.runtime.sendMessage({ type: "oci-redirect", origin }).catch(() => {});
  },
  { urls: ["https://*/*", "http://localhost/*", "http://127.0.0.1/*"] }
);
