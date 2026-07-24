import init, {
  build_tar,
  decode_layer,
  extract_file,
  layer_encoding,
  normalize_repository,
  parse_catalog,
  parse_diff_ids,
  parse_document,
  parse_tags,
  sha256,
} from "./pkg/oci_zero_web.js?v=20260718-3";

const REGISTRIES = [
  ["Docker Hub", "registry-1.docker.io"],
  ["GitHub", "ghcr.io"],
  ["Quay", "quay.io"],
  ["Kubernetes", "registry.k8s.io"],
  ["ECR Public", "public.ecr.aws"],
  ["Microsoft", "mcr.microsoft.com"],
  ["Datadog", "install.datadoghq.com"],
  ["DD Testing", "installtesting.datad0g.com"],
];

const REPOSITORIES = [
  ["Alpine", "docker.io/library/alpine"],
  ["Datadog Agent packages", "install.datadoghq.com/agent-package"],
  ["Datadog testing", "installtesting.datad0g.com/agent-package"],
  ["ORAS", "ghcr.io/oras-project/oras"],
  ["Prometheus Helm chart", "ghcr.io/prometheus-community/charts/prometheus"],
  ["CRI-O bundle", "ghcr.io/cri-o/bundle"],
  ["oci-zero fixtures", "ghcr.io/pawelchcki/oci-zero-small-window-fixtures"],
  ["Bitnami Harbor", "registry-1.docker.io/bitnamicharts/harbor"],
  ["Kubernetes pause", "registry.k8s.io/pause"],
  ["Prometheus", "quay.io/prometheus/prometheus"],
];

const metadataAccept = [
  "application/vnd.oci.image.index.v1+json",
  "application/vnd.oci.image.manifest.v1+json",
  "application/vnd.docker.distribution.manifest.list.v2+json",
  "application/vnd.docker.distribution.manifest.v2+json",
].join(", ");

const extensionMode = Boolean(globalThis.chrome?.runtime?.id && chrome.permissions);
const encoder = new TextEncoder();
const maxBrowserBytes = 256 * 1024 * 1024;
const state = {
  repo: null,
  tag: null,
  rootManifest: null,
  manifestBytes: null,
  manifest: null,
  selectedPlatform: null,
  configBytes: null,
  diffIds: [],
  tags: [],
  tagGroups: [],
  tagManifests: new Map(),
  catalogRepositories: [],
  nextTags: null,
  nextCatalog: null,
  catalogScan: null,
  tagScan: null,
  manifestResolution: null,
  catalogGeneration: 0,
  tagGeneration: 0,
  filePage: 0,
  layerEvents: [],
  layerStatus: [],
  merged: new Map(),
  scan: null,
  tokens: new Map(),
  pending: null,
  redirectOrigin: null,
};

const listRowOverscan = 8;
const filePageSize = 250;
const virtualLists = {};

const $ = (id) => document.getElementById(id);

class PermissionNeeded extends Error {
  constructor(origin) {
    super(`Access to ${origin} is required`);
    this.origin = origin;
  }
}

class FixedVirtualList {
  constructor(element, { rowHeight, overscan, renderRow, onRangeChange = null }) {
    this.element = element;
    this.rowHeight = rowHeight;
    this.overscan = overscan;
    this.renderRow = renderRow;
    this.onRangeChange = onRangeChange;
    this.items = [];
    this.emptyMessage = "";
    this.rangeKey = "";
    element.setAttribute("role", "list");
    element.tabIndex = 0;
    element.addEventListener("scroll", () => this.render());
    element.addEventListener("keydown", (event) => this.handleKey(event));
    this.resizeObserver = new ResizeObserver(() => this.render());
    this.resizeObserver.observe(element);
  }

  setItems(items) {
    this.items = items;
    const focusId = document.activeElement?.closest?.("[data-focus-id]")?.dataset.focusId;
    const focusedTag = focusId?.match(/^tag-(?:open|select):(.+)$/)?.[1];
    const focusedIndex = focusedTag
      ? items.findIndex((item) => item.tags?.includes(focusedTag))
      : -1;
    if (focusedIndex >= 0) {
      const firstVisible = Math.floor(this.element.scrollTop / this.rowHeight);
      const visibleRows = Math.ceil((this.element.clientHeight || this.rowHeight * 6) / this.rowHeight);
      if (focusedIndex < firstVisible || focusedIndex >= firstVisible + visibleRows) {
        this.element.scrollTop = focusedIndex * this.rowHeight;
      }
    }
    const maximum = Math.max(0, items.length * this.rowHeight - this.element.clientHeight);
    if (this.element.scrollTop > maximum) this.element.scrollTop = maximum;
    this.render(true);
  }

  setEmptyMessage(message) {
    this.emptyMessage = message;
    this.render(true);
  }

  render(force = false) {
    const height = this.element.clientHeight || this.rowHeight * 6;
    const first = Math.max(0, Math.floor(this.element.scrollTop / this.rowHeight) - this.overscan);
    const last = Math.min(this.items.length, Math.ceil((this.element.scrollTop + height) / this.rowHeight) + this.overscan);
    const rangeKey = `${first}:${last}:${this.items.length}`;
    if (!force && rangeKey === this.rangeKey) return;
    this.rangeKey = rangeKey;
    const focused = document.activeElement?.closest?.("[data-focus-id]")?.dataset.focusId;
    const spacer = document.createElement("div");
    spacer.className = "virtual-spacer";
    spacer.style.height = `${this.items.length * this.rowHeight}px`;
    if (!this.items.length && this.emptyMessage) {
      const empty = document.createElement("p");
      empty.className = "muted virtual-empty";
      empty.textContent = this.emptyMessage;
      empty.setAttribute("role", "listitem");
      empty.setAttribute("aria-posinset", "1");
      empty.setAttribute("aria-setsize", "1");
      spacer.append(empty);
    }
    for (let index = first; index < last; index += 1) {
      const row = this.renderRow(this.items[index], index, this.items.length);
      row.classList.add("virtual-row");
      row.style.height = `${this.rowHeight}px`;
      row.style.transform = `translateY(${index * this.rowHeight}px)`;
      row.setAttribute("role", "listitem");
      row.setAttribute("aria-posinset", String(index + 1));
      row.setAttribute("aria-setsize", String(this.items.length));
      spacer.append(row);
    }
    this.element.replaceChildren(spacer);
    if (focused) {
      let target = this.element.querySelector(`[data-focus-id="${CSS.escape(focused)}"]`);
      const tagFocus = focused.match(/^(tag-open|tag-select):(.+)$/);
      if (!target && tagFocus) {
        for (const group of this.element.querySelectorAll("[data-group-tags]")) {
          if (!JSON.parse(group.dataset.groupTags).includes(tagFocus[2])) continue;
          target = group.querySelector(tagFocus[1] === "tag-select" ? "select" : "[data-group-open]");
          break;
        }
      }
      target?.focus({ preventScroll: true });
    }
    this.onRangeChange?.(this.items.slice(first, last));
  }

  handleKey(event) {
    if (event.target !== this.element) return;
    const page = Math.max(this.rowHeight, this.element.clientHeight - this.rowHeight);
    const movement = { ArrowDown: this.rowHeight, ArrowUp: -this.rowHeight, PageDown: page, PageUp: -page }[event.key];
    if (movement !== undefined) {
      event.preventDefault();
      this.element.scrollBy({ top: movement });
    } else if (event.key === "Home") {
      event.preventDefault();
      this.element.scrollTop = 0;
    } else if (event.key === "End") {
      event.preventDefault();
      this.element.scrollTop = this.element.scrollHeight;
    }
  }
}

const initWasm = () => init({
  module_or_path: new URL("./pkg/oci_zero_web_bg.wasm?v=20260718-3", import.meta.url),
});
await initWasm();
initializeUi();

function initializeUi() {
  const runtimeBadge = $("runtime-badge");
  if (extensionMode) {
    runtimeBadge.textContent = "Chrome extension";
    runtimeBadge.classList.remove("hidden");
  } else {
    runtimeBadge.textContent = "";
    runtimeBadge.classList.add("hidden");
  }
  renderPresets();

  $("repository-form").addEventListener("submit", (event) => {
    event.preventDefault();
    runWithPermissions(() => openRepository($("repository-input").value, true));
  });
  $("catalog-form").addEventListener("submit", (event) => {
    event.preventDefault();
    runWithPermissions(() => openCatalog($("registry-input").value, true));
  });
  $("permission-button").addEventListener("click", grantPendingPermission);
  $("catalog-more").addEventListener("click", () => runWithPermissions(loadMoreCatalog));
  $("tags-more").addEventListener("click", () => runWithPermissions(loadMoreTags));
  $("catalog-search-all").addEventListener("click", () => runWithPermissions(searchRemainingCatalog));
  $("catalog-cancel-search").addEventListener("click", cancelCatalogSearch);
  $("tags-search-all").addEventListener("click", () => runWithPermissions(searchRemainingTags));
  $("tags-cancel-search").addEventListener("click", cancelTagSearch);
  $("tag-filter").addEventListener("input", debounce(renderTags, 150));
  $("catalog-filter").addEventListener("input", debounce(renderCatalog, 150));
  $("catalog-sort").addEventListener("change", renderCatalog);
  $("tag-sort").addEventListener("change", renderTags);
  $("file-filter").addEventListener("input", () => { state.filePage = 0; renderFiles(); });
  $("files-previous").addEventListener("click", () => { state.filePage -= 1; renderFiles(); });
  $("files-next").addEventListener("click", () => { state.filePage += 1; renderFiles(); });
  $("scan-files").addEventListener("click", () => runWithPermissions(() => scanFiles()));
  $("export-oci").addEventListener("click", () => runWithPermissions(exportOci));
  $("export-docker").addEventListener("click", () => runWithPermissions(exportDocker));

  if (extensionMode) {
    chrome.runtime.onMessage.addListener((message) => {
      if (message?.type === "oci-redirect") state.redirectOrigin = message.origin;
    });
  }

  virtualLists.catalog = new FixedVirtualList($("catalog-results"), {
    rowHeight: 44,
    overscan: listRowOverscan,
    renderRow: renderCatalogRow,
  });
  virtualLists.tags = new FixedVirtualList($("tag-results"), {
    rowHeight: 116,
    overscan: listRowOverscan,
    renderRow: renderTagRow,
    onRangeChange: scheduleVisibleTagResolution,
  });
}

function renderPresets() {
  for (const [label, registry] of REGISTRIES) {
    const item = actionButton(label, () => {
      $("registry-input").value = registry;
      runWithPermissions(() => openCatalog(registry, true));
    });
    $("registry-presets").append(item);
  }
  for (const [label, repository] of REPOSITORIES) {
    const item = actionButton(label, () => {
      $("repository-input").value = repository;
      runWithPermissions(() => openRepository(repository, true));
    });
    const small = document.createElement("small");
    small.textContent = repository;
    item.append(small);
    $("repository-presets").append(item);
  }
}

async function runWithPermissions(operation) {
  hidePermission();
  try {
    await operation();
  } catch (error) {
    if (error instanceof PermissionNeeded) {
      state.pending = { origin: error.origin, operation };
      $("permission-message").textContent = `Allow the extension to contact ${error.origin}.`;
      $("permission-box").classList.remove("hidden");
      setStatus(`Permission required for ${error.origin}.`);
      return;
    }
    console.error(error);
    const suffix = !extensionMode && String(error).toLowerCase().includes("fetch")
      ? " This registry probably blocks browser CORS; load the Chrome extension or open this page in a CORS-disabled browser instead."
      : "";
    setStatus(`${errorMessage(error)}${suffix}`, true);
  }
}

async function grantPendingPermission() {
  if (!state.pending) return;
  const pending = state.pending;
  try {
    const granted = await requestOrigin(pending.origin);
    if (!granted) throw new Error(`Access to ${pending.origin} was denied`);
    state.pending = null;
    hidePermission();
    await runWithPermissions(pending.operation);
  } catch (error) {
    setStatus(errorMessage(error), true);
  }
}

function hidePermission() {
  $("permission-box").classList.add("hidden");
}

async function ensureOrigin(url, interactive = false) {
  if (!extensionMode) return;
  const parsed = new URL(url);
  if (parsed.protocol === "http:" && !["localhost", "127.0.0.1"].includes(parsed.hostname)) {
    throw new Error("The extension permits plain HTTP only for localhost registries");
  }
  const origin = parsed.origin;
  const pattern = permissionPattern(origin);
  if (interactive) {
    const granted = await chrome.permissions.request({ origins: [pattern] });
    if (!granted) throw new Error(`Access to ${origin} was denied`);
    return;
  }
  const granted = await chrome.permissions.contains({ origins: [pattern] });
  if (!granted) throw new PermissionNeeded(origin);
}

function requestOrigin(origin) {
  if (!extensionMode) return Promise.resolve(true);
  return chrome.permissions.request({ origins: [permissionPattern(origin)] });
}

function permissionPattern(origin) {
  const url = new URL(origin);
  return `${url.protocol}//${url.hostname}/*`;
}

async function registryFetch(url, accept = "*/*", interactive = false, signal = undefined) {
  await ensureOrigin(url, interactive);
  let response = await rawFetch(url, { accept, signal });
  if (response.status !== 401) return response;

  const challenge = response.headers.get("www-authenticate");
  if (!challenge) throw new Error("Registry returned 401 without WWW-Authenticate");
  if (!/^Bearer\s/i.test(challenge)) {
    throw new Error("Registry requires private/Basic credentials; this example supports anonymous access and cookies only");
  }
  const bearer = parseBearerChallenge(challenge);
  const cacheKey = `${bearer.realm}|${bearer.service || ""}|${bearer.scope || ""}`;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const cached = state.tokens.get(cacheKey);
    if (cached && cached.expiresAt <= Date.now()) state.tokens.delete(cacheKey);
    let token = cached?.expiresAt > Date.now() ? cached.token : null;
    if (!token) {
      const tokenUrl = new URL(bearer.realm);
      if (bearer.service) tokenUrl.searchParams.set("service", bearer.service);
      if (bearer.scope) tokenUrl.searchParams.set("scope", bearer.scope);
      await ensureOrigin(tokenUrl);
      const tokenResponse = await rawFetch(tokenUrl, { accept: "application/json", signal });
      if (!tokenResponse.ok) throw new Error(`Token service returned HTTP ${tokenResponse.status}`);
      const body = await tokenResponse.json();
      token = body.token || body.access_token;
      if (!token) throw new Error("Token service response has no token");
      const expiresAt = Date.now() + Math.max(30, Number(body.expires_in || 300) - 15) * 1000;
      state.tokens.set(cacheKey, { token, expiresAt });
    }
    response = await rawFetch(url, { accept, authorization: `Bearer ${token}`, signal });
    if (response.status !== 403 || !cached) {
      if (response.status === 401) state.tokens.delete(cacheKey);
      return response;
    }
    // Some registries revoke a token before its advertised expiry. Refresh it
    // once before surfacing the 403, without retrying genuine ACL failures.
    state.tokens.delete(cacheKey);
  }
  return response;
}

async function rawFetch(url, { accept, authorization, signal } = {}) {
  state.redirectOrigin = null;
  const headers = new Headers();
  if (accept) headers.set("Accept", accept);
  if (authorization) headers.set("Authorization", authorization);
  try {
    return await fetch(url, {
      headers,
      credentials: "include",
      redirect: "follow",
      signal,
    });
  } catch (error) {
    if (error?.name === "AbortError") throw error;
    await new Promise((resolve) => setTimeout(resolve, 50));
    if (state.redirectOrigin) throw new PermissionNeeded(state.redirectOrigin);
    throw error;
  }
}

function parseBearerChallenge(value) {
  const result = {};
  for (const match of value.slice(value.indexOf(" ") + 1).matchAll(/([A-Za-z_]+)="((?:[^"\\]|\\.)*)"/g)) {
    result[match[1].toLowerCase()] = match[2].replace(/\\(.)/g, "$1");
  }
  if (!result.realm) throw new Error("Bearer challenge has no realm");
  return result;
}

async function openCatalog(input, interactive = false, path = null, append = false) {
  const registry = normalizeRegistry(input);
  if (!append) {
    cancelCatalogSearch();
    state.catalogGeneration += 1;
    state.catalogRepositories = [];
    $("catalog-progress").textContent = "";
  }
  const generation = state.catalogGeneration;
  state.catalogBase = registry.base;
  virtualLists.catalog.setEmptyMessage("");
  const url = path ? new URL(path, registry.base) : new URL("/v2/_catalog?n=100", registry.base);
  setStatus(`Loading the ${registry.host} catalog…`);
  const response = await registryFetch(url, "application/json", interactive);
  if (generation !== state.catalogGeneration) return;
  if (response.status === 403 || response.status === 404 || response.status === 405) {
    state.catalogRepositories = [];
    state.nextCatalog = null;
    toggleMore("catalog-more", false);
    showPanel("catalog-panel");
    const message = "This registry does not expose a catalog. Choose a repository preset or enter its path directly.";
    virtualLists.catalog.setItems([]);
    virtualLists.catalog.setEmptyMessage(message);
    $("catalog-count").textContent = message;
    setStatus(`${registry.host} does not expose /v2/_catalog.`);
    return;
  }
  if (!response.ok) throw new Error(`Catalog returned HTTP ${response.status}`);
  const repositories = parse_catalog(new Uint8Array(await response.arrayBuffer()));
  state.catalogRepositories = append ? [...new Set([...state.catalogRepositories, ...repositories])] : repositories;
  state.nextCatalog = nextLink(response, url);
  toggleMore("catalog-more", state.nextCatalog);
  renderCatalog();
  showPanel("catalog-panel");
  setStatus(`Loaded ${countLabel(state.catalogRepositories.length, "repository", "repositories")} from ${registry.host}.`);
}

function loadMoreCatalog() {
  if (!state.nextCatalog) return Promise.resolve();
  return openCatalog(state.nextCatalog.origin, false, state.nextCatalog.href, true);
}

async function openRepository(input, interactive = false, path = null, append = false) {
  const repository = normalize_repository(input);
  if (!append) {
    resetSelection();
    cancelTagSearch();
    cancelManifestResolution();
    state.tagGeneration += 1;
    state.tags = [];
    state.tagManifests = new Map();
    $("tags-progress").textContent = "";
  }
  state.repo = { ...repository, base: `${repository.scheme}://${repository.registry}` };
  const generation = state.tagGeneration;
  const url = path
    ? new URL(path, state.repo.base)
    : new URL(`/v2/${state.repo.repository}/tags/list?n=100`, state.repo.base);
  setStatus(`Loading tags for ${state.repo.registry}/${state.repo.repository}…`);
  const response = await registryFetch(url, "application/json", interactive);
  if (generation !== state.tagGeneration) return;
  if (!response.ok) throw new Error(`Tags returned HTTP ${response.status}`);
  const page = parse_tags(new Uint8Array(await response.arrayBuffer()));
  state.tags = append ? [...new Set([...state.tags, ...page.tags])] : page.tags;
  state.nextTags = nextLink(response, url);
  $("repository-name").textContent = `${state.repo.registry}/${state.repo.repository}`;
  $("repository-input").value = state.repo.scheme === "https"
    ? `${state.repo.registry}/${state.repo.repository}`
    : `${state.repo.base}/${state.repo.repository}`;
  toggleMore("tags-more", state.nextTags);
  rebuildTagGroups();
  renderTags();
  showPanel("tags-panel");
  setStatus(`Loaded ${countLabel(state.tags.length, "tag")} for ${page.name || state.repo.repository}.`);
}

function loadMoreTags() {
  if (!state.nextTags || !state.repo) return Promise.resolve();
  return openRepository(`${state.repo.base}/${state.repo.repository}`, false, state.nextTags.href, true);
}

function renderCatalog() {
  const query = $("catalog-filter").value.trim().toLowerCase();
  const direction = $("catalog-sort").value === "name-desc" ? -1 : 1;
  const matching = state.catalogRepositories
    .filter((repository) => repository.toLowerCase().includes(query))
    .sort((left, right) => direction * left.localeCompare(right));
  virtualLists.catalog.setItems(matching);
  const more = state.nextCatalog ? "; more pages available." : "; all available pages loaded.";
  $("catalog-count").textContent = `${countLabel(matching.length, "match")} in ${countLabel(state.catalogRepositories.length, "loaded repository", "loaded repositories")}${more}`;
  $("catalog-more").disabled = Boolean(state.catalogScan);
  toggleMore("catalog-search-all", Boolean(query && state.nextCatalog && !state.catalogScan));
  toggleMore("catalog-cancel-search", Boolean(state.catalogScan));
}

function renderCatalogRow(repository) {
  const row = document.createElement("div");
  row.className = "catalog-row";
  row.dataset.key = repository;
  const button = actionButton(repository, () => {
    const target = `${state.catalogBase}/${repository}`;
    $("repository-input").value = target;
    runWithPermissions(() => openRepository(target));
  });
  button.className = "catalog-open";
  button.dataset.focusId = `catalog:${repository}`;
  button.setAttribute("aria-label", `Open repository ${repository}`);
  row.append(button);
  return row;
}

function rebuildTagGroups() {
  const grouped = new Map();
  for (const tag of state.tags) {
    const resolved = state.tagManifests.get(tag);
    const key = resolved?.digest || `tag:${tag}`;
    const group = grouped.get(key) || { key, digest: resolved?.digest, tags: [], failed: Boolean(resolved?.error) };
    group.tags.push(tag);
    grouped.set(key, group);
  }
  state.tagGroups = [...grouped.values()].map((group) => ({ ...group, title: canonicalTag(group.tags) }));
}

function canonicalTag(tags) {
  const immutable = tags.filter((tag) => !/^(latest|\d+(?:\.\d+){0,2})$/i.test(tag));
  return (immutable.length ? immutable : tags).sort(compareVersions)[0];
}

function compareVersions(left, right) {
  const parse = (value) => (value.match(/\d+/g) || []).map(Number);
  const a = parse(left); const b = parse(right);
  for (let i = 0; i < Math.max(a.length, b.length); i += 1) if ((b[i] || 0) !== (a[i] || 0)) return (b[i] || 0) - (a[i] || 0);
  return right.localeCompare(left);
}

function compareVersionGroups(left, right) { return compareVersions(left.title, right.title); }

function renderTags() {
  const query = $("tag-filter").value.trim().toLowerCase();
  const filtered = state.tagGroups.filter((group) => !query
    || group.digest?.toLowerCase().includes(query)
    || group.tags.some((tag) => tag.toLowerCase().includes(query)));
  const sort = $("tag-sort").value;
  filtered.sort(sort === "version-desc"
    ? compareVersionGroups
    : (left, right) => (sort === "name-desc" ? -1 : 1) * left.title.localeCompare(right.title));
  virtualLists.tags.setItems(filtered);
  const completed = state.tags.filter((tag) => {
    const result = state.tagManifests.get(tag);
    return result && !result.pending;
  }).length;
  const more = state.nextTags ? " More pages are available." : " All available tag pages are loaded.";
  $("tag-limit").textContent = `${countLabel(filtered.length, "matching group")} across ${countLabel(state.tags.length, "loaded tag")}. ${completed} of ${state.tags.length} loaded tags resolved; alias groups may be incomplete.${more}`;
  $("tags-more").disabled = Boolean(state.tagScan);
  toggleMore("tags-search-all", Boolean(query && state.nextTags && !state.tagScan));
  toggleMore("tags-cancel-search", Boolean(state.tagScan));
}

function renderTagRow(group) {
  const card = document.createElement("div");
  card.className = "version-group";
  card.dataset.key = group.key;
  card.dataset.groupTags = JSON.stringify(group.tags);
  if (group.tags.includes(state.tag)) card.setAttribute("aria-current", "true");
  const heading = document.createElement("div");
  heading.className = "version-title";
  const title = document.createElement("strong");
  title.textContent = group.title;
  const controls = document.createElement("div");
  controls.className = "tag-row-controls";
  const selector = document.createElement("select");
  selector.dataset.focusId = `tag-select:${group.title}`;
  selector.setAttribute("aria-label", `Alias for ${group.title}`);
  for (const tag of [...group.tags].sort(compareVersions)) {
    const option = document.createElement("option");
    option.value = tag;
    option.textContent = tag;
    option.selected = tag === group.title;
    selector.append(option);
  }
  const open = actionButton("Open", () => runWithPermissions(() => inspectTag(selector.value)));
  const updateLabel = () => open.setAttribute("aria-label", selector.value === group.title
    ? `Open version ${selector.value}` : `Open alias ${selector.value}`);
  selector.addEventListener("change", updateLabel);
  updateLabel();
  open.dataset.focusId = `tag-open:${group.title}`;
  open.dataset.groupOpen = "true";
  controls.append(selector, open);
  heading.append(title, controls);
  const digest = document.createElement("code");
  digest.className = "version-digest";
  digest.textContent = group.digest || (group.failed ? "manifest resolution failed" : "manifest not resolved yet");
  card.append(heading, digest);
  const latest = group.tags.find((tag) => tag.toLowerCase() === "latest" && tag !== group.title);
  if (latest) {
    const chip = actionButton(latest, () => runWithPermissions(() => inspectTag(latest)));
    chip.className = "chip";
    chip.dataset.focusId = `tag-open:${latest}`;
    chip.setAttribute("aria-label", `Open alias ${latest}`);
    card.append(chip);
  }
  return card;
}

function scheduleVisibleTagResolution(groups) {
  if (!state.repo) return;
  let work = state.manifestResolution;
  if (!work || work.generation !== state.tagGeneration) {
    work = {
      generation: state.tagGeneration,
      controller: new AbortController(),
      queue: [],
      queued: new Set(),
      active: 0,
    };
    state.manifestResolution = work;
  }
  for (const group of groups) for (const tag of group.tags) {
    if (!state.tagManifests.has(tag) && !work.queued.has(tag)) {
      work.queued.add(tag);
      work.queue.push(tag);
    }
  }
  pumpTagResolution(work);
}

function pumpTagResolution(work) {
  while (work.active < 4 && work.queue.length && state.manifestResolution === work) {
    const tag = work.queue.shift();
    work.active += 1;
    state.tagManifests.set(tag, { pending: true });
    void resolveOneTag(tag, work).finally(() => {
      work.active -= 1;
      pumpTagResolution(work);
    });
  }
}

async function resolveOneTag(tag, work) {
  try {
    const response = await registryFetch(manifestUrl(tag), metadataAccept, false, work.controller.signal);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (state.manifestResolution !== work) return;
    state.tagManifests.set(tag, { bytes, document: parse_document(bytes), digest: sha256(bytes) });
  } catch (error) {
    if (error?.name === "AbortError" || state.manifestResolution !== work) return;
    state.tagManifests.set(tag, { error: errorMessage(error) });
  }
  rebuildTagGroups();
  renderTags();
}

async function searchRemainingCatalog() {
  if (!state.nextCatalog || state.catalogScan || !$("catalog-filter").value.trim()) return;
  const scan = {
    controller: new AbortController(),
    generation: state.catalogGeneration,
    seen: new Set(),
    pages: 0,
    initialItems: state.catalogRepositories.length,
  };
  state.catalogScan = scan;
  renderCatalog();
  try {
    while (state.nextCatalog && state.catalogScan === scan) {
      const url = state.nextCatalog;
      if (scan.seen.has(url.href)) {
        $("catalog-progress").textContent = `Stopped after ${scan.pages} pages because the registry repeated a pagination cursor; partial results were retained.`;
        state.nextCatalog = null;
        break;
      }
      scan.seen.add(url.href);
      const response = await registryFetch(url, "application/json", false, scan.controller.signal);
      if (!response.ok) throw new Error(`Catalog returned HTTP ${response.status}`);
      const repositories = parse_catalog(new Uint8Array(await response.arrayBuffer()));
      state.catalogRepositories = [...new Set([...state.catalogRepositories, ...repositories])];
      state.nextCatalog = nextLink(response, url);
      scan.pages += 1;
      renderCatalog();
      $("catalog-progress").textContent = `Searched ${countLabel(scan.pages, "additional page")}; loaded ${countLabel(state.catalogRepositories.length - scan.initialItems, "additional repository")}.`;
    }
    if (state.catalogScan === scan && !state.nextCatalog) {
      $("catalog-progress").textContent = `Search complete: ${scan.pages} additional pages searched; ${state.catalogRepositories.length} repositories loaded.`;
    }
  } catch (error) {
    if (error?.name !== "AbortError") {
      $("catalog-progress").textContent = `Search stopped after ${scan.pages} pages: ${errorMessage(error)} Partial results were retained.`;
    }
  } finally {
    if (state.catalogScan === scan) state.catalogScan = null;
    toggleMore("catalog-more", state.nextCatalog);
    renderCatalog();
  }
}

function cancelCatalogSearch() {
  const scan = state.catalogScan;
  if (!scan) return;
  state.catalogScan = null;
  scan.controller.abort();
  $("catalog-progress").textContent = `Search cancelled after ${scan.pages} pages; partial results were retained.`;
  renderCatalog();
}

async function searchRemainingTags() {
  if (!state.nextTags || state.tagScan || !$("tag-filter").value.trim() || !state.repo) return;
  const scan = {
    controller: new AbortController(),
    generation: state.tagGeneration,
    seen: new Set(),
    pages: 0,
    initialItems: state.tags.length,
  };
  state.tagScan = scan;
  renderTags();
  try {
    while (state.nextTags && state.tagScan === scan && scan.generation === state.tagGeneration) {
      const url = state.nextTags;
      if (scan.seen.has(url.href)) {
        $("tags-progress").textContent = `Stopped after ${scan.pages} pages because the registry repeated a pagination cursor; partial results were retained.`;
        state.nextTags = null;
        break;
      }
      scan.seen.add(url.href);
      const response = await registryFetch(url, "application/json", false, scan.controller.signal);
      if (!response.ok) throw new Error(`Tags returned HTTP ${response.status}`);
      const page = parse_tags(new Uint8Array(await response.arrayBuffer()));
      state.tags = [...new Set([...state.tags, ...page.tags])];
      state.nextTags = nextLink(response, url);
      scan.pages += 1;
      rebuildTagGroups();
      renderTags();
      $("tags-progress").textContent = `Searched ${countLabel(scan.pages, "additional page")}; loaded ${countLabel(state.tags.length - scan.initialItems, "additional tag name")}.`;
    }
    if (state.tagScan === scan && !state.nextTags) {
      $("tags-progress").textContent = `Search complete: ${scan.pages} additional pages searched; ${state.tags.length} tag names loaded. Manifests were not fetched by the scan.`;
    }
  } catch (error) {
    if (error?.name !== "AbortError") {
      $("tags-progress").textContent = `Search stopped after ${scan.pages} pages: ${errorMessage(error)} Partial results were retained.`;
    }
  } finally {
    if (state.tagScan === scan) state.tagScan = null;
    toggleMore("tags-more", state.nextTags);
    renderTags();
  }
}

function cancelTagSearch() {
  const scan = state.tagScan;
  if (!scan) return;
  state.tagScan = null;
  scan.controller.abort();
  $("tags-progress").textContent = `Search cancelled after ${scan.pages} pages; partial results were retained.`;
  renderTags();
}

function cancelManifestResolution() {
  const work = state.manifestResolution;
  state.manifestResolution = null;
  work?.controller.abort();
}

async function inspectTag(tag) {
  cancelScan();
  state.tag = tag;
  state.selectedPlatform = null;
  renderTags();
  setStatus(`Loading ${state.repo.repository}:${tag}…`);
  let cached = state.tagManifests.get(tag);
  if (!cached?.bytes) {
    const response = await registryFetch(manifestUrl(tag), metadataAccept);
    if (!response.ok) throw new Error(`Manifest returned HTTP ${response.status}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    cached = { bytes, document: parse_document(bytes), digest: sha256(bytes) };
    state.tagManifests.set(tag, cached);
    rebuildTagGroups();
    renderTags();
  }
  const { bytes, document } = cached;
  state.rootManifest = { bytes, document };
  $("selection-name").textContent = `${state.repo.registry}/${state.repo.repository}:${tag}`;
  $("manifest-json").textContent = prettyJson(bytes);
  renderDocument(document);
  showPanel("metadata-panel");
  if (document.kind === "manifest") await activateManifest(bytes, document, null);
  else setStatus(`Select one of ${countLabel(document.manifests.length, "platform manifest")}.`);
}

function renderDocument(document) {
  clear($("platforms"));
  clear($("descriptors"));
  $("image-actions").classList.add("hidden");
  $("config-details").classList.add("hidden");
  if (document.kind === "index") {
    for (const descriptor of document.manifests) {
      const platform = descriptor.platform;
      const label = platform
        ? `${platform.os}/${platform.architecture}${platform.variant ? `/${platform.variant}` : ""}`
        : descriptor.digest.slice(7, 19);
      const action = actionButton(label, () => runWithPermissions(() => selectPlatform(descriptor)));
      action.setAttribute("aria-label", `Select platform ${label}`);
      $("platforms").append(action);
      $("descriptors").append(descriptorRow("manifest", descriptor));
    }
  }
}

async function selectPlatform(descriptor) {
  cancelScan();
  setStatus(`Loading ${descriptor.digest}…`);
  const response = await registryFetch(manifestUrl(descriptor.digest), metadataAccept);
  if (!response.ok) throw new Error(`Platform manifest returned HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  verifyDescriptor(bytes, descriptor);
  const document = parse_document(bytes);
  if (document.kind === "index") {
    $("manifest-json").textContent = prettyJson(bytes);
    renderDocument(document);
    setStatus(`Nested index loaded; select one of ${countLabel(document.manifests.length, "entry", "entries")}.`);
    return;
  }
  state.selectedPlatform = descriptor.platform;
  $("manifest-json").textContent = prettyJson(bytes);
  await activateManifest(bytes, document, descriptor.platform);
}

async function activateManifest(bytes, document, platform) {
  cancelScan();
  state.manifestBytes = bytes;
  state.manifest = document;
  state.selectedPlatform = platform;
  state.layerEvents = [];
  state.layerStatus = [];
  state.merged = new Map();
  state.filePage = 0;
  hidePanel("files-panel");
  clear($("descriptors"));
  $("descriptors").append(descriptorRow("config", document.config));
  document.layers.forEach((layer, index) => {
    const row = descriptorRow(`layer ${index}`, layer);
    const archiveEncoding = layer_encoding(layer.media_type);
    const action = archiveEncoding
      ? actionButton("Browse layer", () => runWithPermissions(() => scanFiles([index])))
      : actionButton(
        isInstallerPayload(layer) ? "Download executable" : "Download blob",
        () => runWithPermissions(() => downloadLayerPayload(layer, index)),
      );
    action.setAttribute("aria-label", archiveEncoding
      ? `Browse layer ${index + 1}`
      : `${isInstallerPayload(layer) ? "Download executable" : "Download blob"} for layer ${index + 1}`);
    if (!archiveEncoding) {
      action.title = "This payload is not a tar archive; the download is verified against its descriptor.";
    }
    row.append(action);
    $("descriptors").append(row);
  });

  const config = await fetchDescriptor(document.config);
  state.configBytes = config;
  $("config-json").textContent = prettyJson(config);
  $("config-details").classList.remove("hidden");
  try {
    state.diffIds = parse_diff_ids(config);
  } catch (_) {
    state.diffIds = [];
  }
  const imageConfig = isImageConfig(document.config);
  if (!imageConfig) state.diffIds = [];
  const image = imageConfig && state.diffIds.length === document.layers.length;
  const supported = document.layers.filter((layer) => layer_encoding(layer.media_type)).length;
  $("scan-files").disabled = supported === 0;
  $("export-docker").disabled = !image;
  $("image-actions").classList.remove("hidden");
  setStatus(image
    ? `Ready: ${countLabel(document.layers.length, "verified image layer")}.`
    : `Ready: ${countLabel(supported, "supported artifact layer")}; compressed descriptor verification is enabled.`);
}

function isImageConfig(descriptor) {
  return descriptor?.media_type === "application/vnd.oci.image.config.v1+json"
    || descriptor?.media_type === "application/vnd.docker.container.image.v1+json";
}

async function scanFiles(targets = null) {
  if (!state.manifest) throw new Error("Select a manifest first");
  cancelScan();
  const scan = {
    cancelled: false,
    renderTimer: null,
    worker: new LayerScanWorker(),
  };
  state.scan = scan;
  const allLayers = targets === null;
  const selected = targets || state.manifest.layers.map((_, index) => index);
  if (allLayers || state.layerEvents.length !== state.manifest.layers.length) {
    state.layerEvents = state.manifest.layers.map(() => []);
    state.layerStatus = state.manifest.layers.map(() => "pending");
    state.merged = new Map();
  } else {
    for (const index of selected) {
      state.layerEvents[index] = [];
      state.layerStatus[index] = "pending";
    }
    rebuildMergedFilesystem();
  }
  let indexed = 0;
  let activeLayer = null;
  $("scan-files").disabled = true;
  showPanel("files-panel");
  renderLayers();
  renderFiles();

  try {
    setStatus("Starting the background layer scanner…");
    await scan.worker.waitUntilReady();
    for (const index of selected) {
      if (state.scan !== scan) return;
      activeLayer = index;
      const layer = state.manifest.layers[index];
      if (!layer_encoding(layer.media_type)) {
        state.layerStatus[index] = "skipped";
        state.layerEvents[index] = [];
        flushScanRender(scan);
        continue;
      }
      state.layerStatus[index] = "scanning";
      setStatus(`Scanning layer ${index + 1}: 0 B / ${formatBytes(layer.size)}…`);
      scheduleScanRender(scan);
      const response = await registryFetch(blobUrl(layer.digest), layer.media_type || "application/octet-stream");
      if (!response.ok) throw new Error(`Blob ${layer.digest} returned HTTP ${response.status}`);
      if (!response.body) throw new Error(`Blob ${layer.digest} has no readable response body`);

      await scan.worker.scan(response.body.getReader(), layer, state.diffIds[index], {
        onEvents(events) {
          if (state.scan !== scan) return;
          indexed += events.filter((event) => event.type === "entry").length;
          if (indexed > 200_000) throw new Error("Image contains more than 200000 indexed entries");
          state.layerEvents[index].push(...events);
          applyLayerEvents(events, index);
          scheduleScanRender(scan);
        },
        onProgress(received) {
          if (state.scan !== scan) return;
          setStatus(
            `Scanning layer ${index + 1}: ${formatBytes(received)} / ${formatBytes(layer.size)} · ${indexed} files…`,
          );
        },
      });
      if (state.scan !== scan) return;
      state.layerStatus[index] = "verified";
      activeLayer = null;
      flushScanRender(scan);
    }
    const skipped = selected.filter((index) => state.layerStatus[index] === "skipped").length;
    setStatus(`Scanned ${countLabel(selected.length - skipped, "layer")}${skipped ? `; skipped ${countLabel(skipped, "unsupported layer")}` : ""}.`);
  } catch (error) {
    if (scan.cancelled) return;
    if (activeLayer !== null) {
      state.layerEvents[activeLayer] = [];
      state.layerStatus[activeLayer] = "failed";
      rebuildMergedFilesystem();
      flushScanRender(scan);
    }
    throw error;
  } finally {
    scan.worker.terminate();
    if (state.scan === scan) {
      state.scan = null;
      if (scan.renderTimer !== null) clearTimeout(scan.renderTimer);
      $("scan-files").disabled = false;
    }
  }
}

class LayerScanWorker {
  constructor() {
    this.worker = new Worker(new URL("./scan-worker.js?v=20260718-3", import.meta.url), { type: "module" });
    this.job = null;
    this.nextJobId = 1;
    this.closed = false;
    this.readyState = "pending";
    this.readyPromise = new Promise((resolve, reject) => {
      this.resolveReady = resolve;
      this.rejectReady = reject;
    });
    this.worker.addEventListener("message", (event) => this.handleMessage(event.data));
    this.worker.addEventListener("error", (event) => {
      this.fail(new Error(event.message || "layer scan worker failed"));
    });
    this.worker.addEventListener("messageerror", () => {
      this.fail(new Error("layer scan worker sent an unreadable message"));
    });
  }

  waitUntilReady() {
    return this.readyPromise;
  }

  scan(reader, layer, diffId, callbacks) {
    if (this.closed) return Promise.reject(new Error("layer scan worker is closed"));
    if (this.job) return Promise.reject(new Error("layer scan worker is busy"));
    return new Promise((resolve, reject) => {
      const jobId = this.nextJobId;
      this.nextJobId += 1;
      this.job = { jobId, reader, callbacks, received: 0, reading: false, resolve, reject };
      this.worker.postMessage({
        type: "scan",
        jobId,
        mediaType: layer.media_type,
        digest: layer.digest,
        size: BigInt(layer.size),
        diffId,
      });
    });
  }

  handleMessage(message) {
    if (message?.type === "ready") {
      if (this.readyState === "pending") {
        this.readyState = "resolved";
        this.resolveReady();
      }
      return;
    }
    const job = this.job;
    if (!job || message?.jobId !== job.jobId) return;
    if (message.type === "pull") {
      void this.provideChunk(job);
    } else if (message.type === "events") {
      try {
        job.callbacks.onEvents(message.events);
      } catch (error) {
        this.fail(error);
      }
    } else if (message.type === "complete") {
      this.finish(job);
    } else if (message.type === "error") {
      this.fail(new Error(message.error));
    }
  }

  async provideChunk(job) {
    if (this.job !== job || job.reading) {
      this.fail(new Error("layer scan worker requested overlapping chunks"));
      return;
    }
    job.reading = true;
    try {
      const { done, value } = await job.reader.read();
      if (this.job !== job) return;
      if (done) {
        this.worker.postMessage({ type: "end", jobId: job.jobId });
        return;
      }
      job.received += value.byteLength;
      job.callbacks.onProgress(job.received);
      const bytes = value.byteOffset === 0 && value.byteLength === value.buffer.byteLength
        ? value.buffer
        : value.slice().buffer;
      this.worker.postMessage({ type: "chunk", jobId: job.jobId, bytes }, [bytes]);
    } catch (error) {
      if (this.job === job) {
        this.worker.postMessage({ type: "source_error", jobId: job.jobId, error: errorMessage(error) });
      }
    } finally {
      job.reading = false;
    }
  }

  finish(job) {
    if (this.job !== job) return;
    this.job = null;
    job.reader.releaseLock();
    job.resolve();
  }

  fail(error) {
    if (this.closed) return;
    this.closed = true;
    if (this.readyState === "pending") {
      this.readyState = "rejected";
      this.rejectReady(error);
    }
    const job = this.job;
    this.job = null;
    if (job) {
      void job.reader.cancel(errorMessage(error));
      job.reject(error);
    }
    this.worker.terminate();
  }

  terminate() {
    if (this.closed) return;
    this.closed = true;
    if (this.readyState === "pending") {
      this.readyState = "rejected";
      this.rejectReady(new Error("layer scan was cancelled"));
    }
    const job = this.job;
    this.job = null;
    if (job) {
      void job.reader.cancel("layer scan was cancelled");
      job.reject(new Error("layer scan was cancelled"));
    }
    this.worker.terminate();
  }
}

function cancelScan() {
  const scan = state.scan;
  if (!scan) return;
  state.scan = null;
  scan.cancelled = true;
  if (scan.renderTimer !== null) clearTimeout(scan.renderTimer);
  scan.worker.terminate();
}

function scheduleScanRender(scan) {
  if (scan.renderTimer !== null) return;
  scan.renderTimer = setTimeout(() => {
    scan.renderTimer = null;
    if (state.scan !== scan) return;
    renderLayers();
    renderFiles();
  }, 100);
}

function flushScanRender(scan) {
  if (scan.renderTimer !== null) clearTimeout(scan.renderTimer);
  scan.renderTimer = null;
  renderLayers();
  renderFiles();
}

function rebuildMergedFilesystem() {
  state.merged = new Map();
  state.layerEvents.forEach((events, index) => {
    if (state.layerStatus[index] === "verified") applyLayerEvents(events, index);
  });
}

function applyLayerEvents(events, layerIndex) {
  for (const event of events) {
    if (event.type === "whiteout") {
      removePath(event.path);
    } else if (event.type === "opaque_directory") {
      const prefix = `${event.path.replace(/\/$/, "")}/`;
      for (const path of state.merged.keys()) if (path.startsWith(prefix)) state.merged.delete(path);
    } else if (event.type === "entry") {
      state.merged.set(event.path.replace(/^\.\//, ""), { ...event, layerIndex });
    }
  }
}

function removePath(target) {
  const normalized = target.replace(/\/$/, "");
  for (const path of state.merged.keys()) {
    if (path === normalized || path.startsWith(`${normalized}/`)) state.merged.delete(path);
  }
}

function renderLayers() {
  clear($("layer-results"));
  state.layerEvents.forEach((events, index) => {
    const status = state.layerStatus[index];
    if (status === "pending") return;
    const details = document.createElement("details");
    details.className = "layer";
    const summary = document.createElement("summary");
    const entries = events.filter((event) => event.type === "entry");
    summary.textContent = `Layer ${index + 1} · ${status} · ${countLabel(entries.length, "entry", "entries")} · ${state.manifest.layers[index].digest}`;
    const body = document.createElement("div");
    body.className = "layer-entries";
    for (const entry of events.slice(0, 500)) {
      const line = document.createElement("div");
      const kind = entry.type === "entry" ? entry.kind : entry.type;
      line.textContent = `${kind.padEnd(18)} ${entry.path}`;
      body.append(line);
    }
    if (events.length > 500) body.append(textNode(`… ${events.length - 500} more events`));
    details.append(summary, body);
    $("layer-results").append(details);
  });
}

function renderFiles() {
  const query = $("file-filter").value.trim().toLowerCase();
  const files = [...state.merged.values()]
    .filter((entry) => entry.path.toLowerCase().includes(query))
    .sort((left, right) => left.path.localeCompare(right.path));
  const pageCount = Math.max(1, Math.ceil(files.length / filePageSize));
  state.filePage = Math.max(0, Math.min(state.filePage, pageCount - 1));
  const start = state.filePage * filePageSize;
  const visible = files.slice(start, start + filePageSize);
  clear($("file-results-body"));
  for (const entry of visible) {
    const row = document.createElement("tr");
    row.className = "file-row";
    const pathCell = document.createElement("th");
    pathCell.scope = "row";
    pathCell.dataset.label = "Path";
    const path = document.createElement("code");
    path.textContent = entry.path;
    pathCell.append(path);
    const kind = document.createElement("td");
    kind.dataset.label = "Type";
    kind.textContent = entry.kind;
    const size = document.createElement("td");
    size.dataset.label = "Size";
    size.className = "file-meta";
    size.textContent = formatBytes(entry.size || 0);
    const source = document.createElement("td");
    source.dataset.label = "Source";
    source.className = "file-meta";
    source.textContent = `layer ${entry.layerIndex + 1}`;
    const verification = document.createElement("td");
    verification.dataset.label = "Status";
    verification.className = "file-meta";
    verification.textContent = state.layerStatus[entry.layerIndex] === "verified" ? "verified" : "scanning";
    const action = document.createElement("td");
    action.dataset.label = "Action";
    if (entry.kind === "regular" || entry.kind === "hard_link") {
      const download = actionButton(
        state.layerStatus[entry.layerIndex] === "verified" ? "Download" : "Scanning…",
        () => runWithPermissions(() => downloadEntry(entry)),
      );
      download.setAttribute("aria-label", state.layerStatus[entry.layerIndex] === "verified"
        ? `Download ${entry.path}`
        : `Scanning ${entry.path}`);
      download.disabled = state.layerStatus[entry.layerIndex] !== "verified";
      action.append(download);
    } else {
      action.textContent = entry.kind === "symbolic_link" ? `→ ${entry.link_target || ""}` : "";
    }
    row.append(pathCell, kind, size, source, verification, action);
    $("file-results-body").append(row);
  }
  const provisional = state.layerStatus.includes("scanning") ? " · scanning" : "";
  $("file-count").textContent = `${countLabel(state.merged.size, "entry", "entries")}${provisional}`;
  $("files-previous").disabled = state.filePage === 0;
  $("files-next").disabled = state.filePage >= pageCount - 1;
  $("file-limit").textContent = files.length > filePageSize
    ? `Showing ${start + 1}–${Math.min(start + filePageSize, files.length)} of ${countLabel(files.length, "matching entry", "matching entries")}.`
    : `${countLabel(files.length, "matching entry", "matching entries")}.`;
}

async function downloadEntry(original) {
  const entry = resolveHardLink(original);
  const saver = await prepareSave(fileName(original.path));
  const layer = state.manifest.layers[entry.layerIndex];
  setStatus(`Extracting ${original.path} from layer ${entry.layerIndex + 1}…`);
  const bytes = await fetchDescriptor(layer);
  const extracted = extract_file(
    bytes,
    layer.media_type,
    layer.digest,
    BigInt(layer.size),
    state.diffIds[entry.layerIndex],
    entry.path,
  );
  await saver(extracted, "application/octet-stream");
  setStatus(`Downloaded ${original.path} (${formatBytes(extracted.length)}).`);
}

async function downloadLayerPayload(layer, index) {
  const name = layerPayloadName(layer, index);
  const saver = await prepareSave(name);
  setStatus(`Downloading and verifying ${name}…`);
  const bytes = await fetchDescriptor(layer);
  await saver(bytes, layer.media_type || "application/octet-stream");
  setStatus(`Downloaded verified ${name} (${formatBytes(bytes.length)}).`);
}

function isInstallerPayload(layer) {
  return layer.media_type === "application/vnd.datadog.package.installer.layer.v1";
}

function layerPayloadName(layer, index) {
  if (isInstallerPayload(layer)) {
    const platform = state.selectedPlatform
      ? `-${[state.selectedPlatform.os, state.selectedPlatform.architecture, state.selectedPlatform.variant].filter(Boolean).join("-")}`
      : "";
    return `datadog-installer${platform}`;
  }
  return `layer-${index}-${layer.digest.slice(7, 19)}.blob`;
}

function resolveHardLink(entry, seen = new Set()) {
  if (entry.kind !== "hard_link") return entry;
  if (!entry.link_target || seen.has(entry.path)) throw new Error("Hard-link target cannot be resolved");
  seen.add(entry.path);
  const target = state.merged.get(entry.link_target.replace(/^\.\//, ""));
  if (!target) throw new Error(`Hard-link target ${entry.link_target} is not visible`);
  return resolveHardLink(target, seen);
}

async function exportOci() {
  if (!state.manifest) throw new Error("Select an image manifest first");
  const saver = await prepareSave(archiveName("oci"));
  const entries = [];
  const seen = new Set();
  const addBlob = (digest, bytes) => {
    if (seen.has(digest)) return;
    verifyDigest(bytes, digest);
    seen.add(digest);
    entries.push({ path: blobPath(digest), bytes });
  };
  addBlob(state.manifest.config.digest, state.configBytes);
  for (let index = 0; index < state.manifest.layers.length; index += 1) {
    setStatus(`Downloading OCI layer ${index + 1}/${state.manifest.layers.length}…`);
    const layer = state.manifest.layers[index];
    addBlob(layer.digest, await fetchDescriptor(layer));
  }
  const exportManifest = normalizedOciManifest(state.manifestBytes);
  const manifestDigest = sha256(exportManifest);
  addBlob(manifestDigest, exportManifest);
  const descriptor = {
    mediaType: state.manifest.media_type || "application/vnd.oci.image.manifest.v1+json",
    digest: manifestDigest,
    size: exportManifest.length,
    annotations: { "org.opencontainers.image.ref.name": state.tag },
  };
  if (state.selectedPlatform) descriptor.platform = camelPlatform(state.selectedPlatform);
  entries.push(
    { path: "oci-layout", bytes: encoder.encode('{"imageLayoutVersion":"1.0.0"}') },
    { path: "index.json", bytes: encoder.encode(JSON.stringify({ schemaVersion: 2, mediaType: "application/vnd.oci.image.index.v1+json", manifests: [descriptor] })) },
  );
  setStatus("Building OCI archive…");
  const archive = build_tar(entries);
  await saver(archive, "application/x-tar");
  setStatus(`Downloaded OCI archive (${formatBytes(archive.length)}).`);
}

async function exportDocker() {
  if (!state.manifest || state.diffIds.length !== state.manifest.layers.length) {
    throw new Error("Docker export requires an image config with matching diff IDs");
  }
  const saver = await prepareSave(archiveName("docker"));
  const entries = [];
  const configPath = blobPath(state.manifest.config.digest);
  entries.push({ path: configPath, bytes: state.configBytes });
  const layerPaths = [];
  const uncompressedDescriptors = [];
  for (let index = 0; index < state.manifest.layers.length; index += 1) {
    const layer = state.manifest.layers[index];
    setStatus(`Unpacking Docker layer ${index + 1}/${state.manifest.layers.length}…`);
    const compressed = await fetchDescriptor(layer);
    const decoded = decode_layer(
      compressed,
      layer.media_type,
      layer.digest,
      BigInt(layer.size),
      state.diffIds[index],
    );
    const path = blobPath(state.diffIds[index]);
    layerPaths.push(path);
    entries.push({ path, bytes: decoded });
    uncompressedDescriptors.push({
      mediaType: "application/vnd.oci.image.layer.v1.tar",
      digest: state.diffIds[index],
      size: decoded.length,
    });
  }
  const repoTag = `${state.repo.registry}/${state.repo.repository}:${state.tag}`;
  const manifestJson = [{ Config: configPath, RepoTags: [repoTag], Layers: layerPaths }];
  const synthesized = encoder.encode(JSON.stringify({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: {
      mediaType: "application/vnd.oci.image.config.v1+json",
      digest: state.manifest.config.digest,
      size: state.configBytes.length,
    },
    layers: uncompressedDescriptors,
  }));
  const synthesizedDigest = sha256(synthesized);
  entries.push(
    { path: blobPath(synthesizedDigest), bytes: synthesized },
    { path: "manifest.json", bytes: encoder.encode(`${JSON.stringify(manifestJson)}\n`) },
    { path: "repositories", bytes: encoder.encode(`${JSON.stringify({ [`${state.repo.registry}/${state.repo.repository}`]: { [state.tag]: state.diffIds.at(-1).slice(7) } })}\n`) },
    { path: "oci-layout", bytes: encoder.encode('{"imageLayoutVersion":"1.0.0"}') },
    { path: "index.json", bytes: encoder.encode(JSON.stringify({
      schemaVersion: 2,
      mediaType: "application/vnd.oci.image.index.v1+json",
      manifests: [{
        mediaType: "application/vnd.oci.image.manifest.v1+json",
        digest: synthesizedDigest,
        size: synthesized.length,
        annotations: { "org.opencontainers.image.ref.name": repoTag },
        ...(state.selectedPlatform ? { platform: camelPlatform(state.selectedPlatform) } : {}),
      }],
    })) },
  );
  setStatus("Building Docker archive…");
  const archive = build_tar(entries);
  await saver(archive, "application/x-tar");
  setStatus(`Downloaded Docker archive (${formatBytes(archive.length)}).`);
}

async function fetchDescriptor(descriptor) {
  if (Number(descriptor.size) > maxBrowserBytes) {
    throw new Error(`${descriptor.digest} exceeds the 256 MiB browser blob limit`);
  }
  const response = await registryFetch(blobUrl(descriptor.digest), descriptor.media_type || "application/octet-stream");
  if (!response.ok) throw new Error(`Blob ${descriptor.digest} returned HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  verifyDescriptor(bytes, descriptor);
  return bytes;
}

function verifyDescriptor(bytes, descriptor) {
  if (Number(descriptor.size) !== bytes.length) {
    throw new Error(`Size mismatch for ${descriptor.digest}: expected ${descriptor.size}, received ${bytes.length}`);
  }
  verifyDigest(bytes, descriptor.digest);
}

function verifyDigest(bytes, expected) {
  const actual = sha256(bytes);
  if (actual !== expected) throw new Error(`Digest mismatch: expected ${expected}, received ${actual}`);
}

function manifestUrl(selector) {
  const segment = selector.startsWith("sha256:") ? selector : encodeURIComponent(selector);
  return new URL(`/v2/${state.repo.repository}/manifests/${segment}`, state.repo.base);
}

function blobUrl(digest) {
  return new URL(`/v2/${state.repo.repository}/blobs/${digest}`, state.repo.base);
}

function blobPath(digest) {
  const [algorithm, encoded] = digest.split(":", 2);
  if (algorithm !== "sha256" || !encoded) throw new Error(`Unsupported digest ${digest}`);
  return `blobs/${algorithm}/${encoded}`;
}

function normalizeRegistry(input) {
  let value = input.trim().replace(/\/$/, "");
  if (!/^https?:\/\//.test(value)) value = `https://${value}`;
  const url = new URL(value);
  if (url.pathname !== "/") throw new Error("Enter only the registry host for catalog browsing");
  return { base: url.origin, host: url.host };
}

function nextLink(response, current) {
  const value = response.headers.get("link");
  if (!value) return null;
  for (const item of value.split(",")) {
    const match = item.match(/<([^>]+)>\s*;.*\brel\s*=\s*"?next"?/i);
    if (match) return new URL(match[1], current);
  }
  return null;
}

function descriptorRow(label, descriptor) {
  const row = document.createElement("div");
  row.className = "descriptor";
  const name = document.createElement("strong");
  name.textContent = `${label} · ${descriptor.media_type || "unknown media type"}`;
  const digest = document.createElement("code");
  digest.textContent = descriptor.digest;
  row.append(name, digest, textNode(formatBytes(descriptor.size)));
  return row;
}

function prettyJson(bytes) {
  const text = new TextDecoder().decode(bytes);
  try { return JSON.stringify(JSON.parse(text), null, 2); } catch (_) { return text; }
}

function normalizedOciManifest(bytes) {
  const manifest = JSON.parse(new TextDecoder().decode(bytes));
  const mediaTypes = new Map([
    ["application/vnd.docker.distribution.manifest.v2+json", "application/vnd.oci.image.manifest.v1+json"],
    ["application/vnd.docker.container.image.v1+json", "application/vnd.oci.image.config.v1+json"],
    ["application/vnd.docker.image.rootfs.diff.tar.gzip", "application/vnd.oci.image.layer.v1.tar+gzip"],
    ["application/vnd.docker.image.rootfs.foreign.diff.tar.gzip", "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip"],
  ]);
  manifest.mediaType = mediaTypes.get(manifest.mediaType) || manifest.mediaType || "application/vnd.oci.image.manifest.v1+json";
  if (manifest.config) manifest.config.mediaType = mediaTypes.get(manifest.config.mediaType) || manifest.config.mediaType;
  for (const layer of manifest.layers || []) layer.mediaType = mediaTypes.get(layer.mediaType) || layer.mediaType;
  return encoder.encode(JSON.stringify(manifest));
}

function camelPlatform(platform) {
  return {
    os: platform.os,
    architecture: platform.architecture,
    ...(platform.variant ? { variant: platform.variant } : {}),
    ...(platform.os_version ? { "os.version": platform.os_version } : {}),
  };
}

async function prepareSave(suggestedName) {
  if (globalThis.showSaveFilePicker) {
    const handle = await showSaveFilePicker({ suggestedName });
    return async (bytes) => {
      const writable = await handle.createWritable();
      await writable.write(bytes);
      await writable.close();
    };
  }
  return async (bytes, type) => {
    const blob = new Blob([bytes], { type });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = suggestedName;
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 30_000);
  };
}

function archiveName(kind) {
  const repo = state.repo.repository.replaceAll("/", "_");
  return `${repo}_${state.tag}_${kind}.tar`;
}

function fileName(path) {
  return path.split("/").filter(Boolean).at(-1) || "download";
}

function formatBytes(value) {
  const bytes = Number(value);
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

function setStatus(message, error = false) {
  const status = $("status");
  status.textContent = error && !/^Error:\s/i.test(message) ? `Error: ${message}` : message;
  status.classList.toggle("error", error);
  status.setAttribute("role", error ? "alert" : "status");
  status.setAttribute("aria-live", error ? "assertive" : "polite");
}

function resetSelection() {
  cancelScan();
  state.tag = null;
  state.rootManifest = null;
  state.manifestBytes = null;
  state.manifest = null;
  state.selectedPlatform = null;
  state.configBytes = null;
  state.diffIds = [];
  state.layerEvents = [];
  state.layerStatus = [];
  state.merged = new Map();
  hidePanel("metadata-panel");
  hidePanel("files-panel");
}

function countLabel(count, singular, plural = `${singular}s`) {
  return `${count} ${count === 1 ? singular : plural}`;
}

function showPanel(id) { $(id).classList.remove("hidden"); }
function hidePanel(id) { $(id).classList.add("hidden"); }
function toggleMore(id, visible) { $(id).classList.toggle("hidden", !visible); }
function clear(element) { element.replaceChildren(); }
function textNode(value) { return document.createTextNode(String(value)); }

function actionButton(label, action) {
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = label;
  button.addEventListener("click", action);
  return button;
}

function errorMessage(error) {
  return error?.message || String(error);
}

function debounce(callback, delay) {
  let timer = null;
  return (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => callback(...args), delay);
  };
}
