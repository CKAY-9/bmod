const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { getCurrentWebview } = window.__TAURI__.webview;

const $ = (id) => document.getElementById(id);

const state = {
  env: null,
  /** Global settings as stored by the backend (see `Settings` in src-tauri). */
  settings: null,
  /** Whether the result box currently shows a toolchain problem. */
  setupShown: false,
  model: null,
  /** Material slots of the loaded model, in model order: { name, triangles, el, texture } */
  slots: [],
  /** Per-slot settings remembered by slot name, so re-loading a re-exported model keeps them. */
  remembered: new Map(),
  /** Parts (objects) of the loaded model, in model order: { name, triangles, el } */
  parts: [],
  /** Bodygroup name typed for each part ("" = always shown), remembered by part name. */
  partGroups: new Map(),
  /** "When spawned" choice per bodygroup, by group name: "last" | "first" | "none". */
  groupBlank: new Map(),
  /** Textures dropped before a model was loaded. */
  pendingTextures: [],
  loadingModel: false,
  running: false,
  nameTouched: false,
  lastDir: null,
};

const ext = (path) => (path.split(".").pop() || "").toLowerCase();
const basename = (path) => path.split(/[\\/]/).pop();
const stem = (path) => basename(path).replace(/\.[^.]+$/, "");
const busy = () => state.running || state.loadingModel;
/** Mirrors bmod_core::sanitize_name. */
const sanitize = (s) => s.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");

function refresh() {
  const ready =
    state.env && !state.env.problem &&
    state.model && state.slots.length > 0 &&
    state.slots.every((s) => s.texture);
  $("convert").disabled = !ready || busy();
  $("convert").textContent = state.running ? "Converting…" : "Convert";
  const missing = state.slots.filter((s) => !s.texture).length;
  $("output-hint").textContent =
    state.model && missing
      ? `${missing} material${missing > 1 ? "s" : ""} still need${missing > 1 ? "" : "s"} a texture`
      : state.env ? `Output goes to ${state.env.outputRoot}` : "";
}

function log(line) {
  const el = document.createElement("div");
  if (/^warning|^WARNING/.test(line)) el.className = "warn";
  if (/ERROR|^error/.test(line)) el.className = "err";
  el.textContent = line;
  const pre = $("log");
  pre.appendChild(el);
  pre.scrollTop = pre.scrollHeight;
}

function showResult(ok, title, detail) {
  const box = $("result");
  box.hidden = false;
  box.classList.toggle("error", !ok);
  $("result-title").textContent = title;
  $("result-detail").textContent = detail;
}

// ---------- Material slots ----------

function readSlot(slot) {
  const q = (sel) => slot.el.querySelector(sel);
  return {
    texture: slot.texture,
    alpha: q(".alpha").value,
    doubleSided: q(".double-sided").checked,
    scrollOn: q(".scroll-on").checked,
    rate: q(".scroll-rate").value,
    angle: q(".scroll-angle").value,
  };
}

function setSlotTexture(slot, path) {
  slot.texture = path;
  slot.el.querySelector(".tex-file").textContent = basename(path);
  slot.el.querySelector(".tex-path").textContent = path;
  slot.el.classList.add("filled");
  refresh();
}

function updateScrollFields(slot) {
  const on = slot.el.querySelector(".scroll-on").checked;
  slot.el.querySelectorAll(".scroll-field").forEach((f) => (f.hidden = !on));
}

function renderSlots(materials) {
  // Remember settings of the outgoing slots.
  for (const s of state.slots) state.remembered.set(s.name, readSlot(s));

  const list = $("materials");
  list.textContent = "";
  state.slots = materials.map((m) => {
    const el = $("slot-template").content.firstElementChild.cloneNode(true);
    const slot = { name: m.name, triangles: m.triangles, el, texture: null };
    el.dataset.slot = m.name;
    el.querySelector(".slot-name").textContent = m.name;
    el.querySelector(".slot-tris").textContent = `${m.triangles} triangle${m.triangles === 1 ? "" : "s"}`;
    el.querySelector(".tex-pick").addEventListener("click", () => browseTexture(slot));
    el.querySelector(".scroll-on").addEventListener("change", () => updateScrollFields(slot));

    const prev = state.remembered.get(m.name);
    if (prev) {
      el.querySelector(".alpha").value = prev.alpha;
      el.querySelector(".double-sided").checked = prev.doubleSided;
      el.querySelector(".scroll-on").checked = prev.scrollOn;
      el.querySelector(".scroll-rate").value = prev.rate;
      el.querySelector(".scroll-angle").value = prev.angle;
    }
    list.appendChild(el);
    if (prev?.texture) setSlotTexture(slot, prev.texture);
    updateScrollFields(slot);
    return slot;
  });
  $("materials-empty").hidden = state.slots.length > 0;

  for (const path of state.pendingTextures.splice(0)) assignTexture(path);
  refresh();
}

/** Puts a texture on `target`, or the first slot without one (or the only slot). */
function assignTexture(path, target) {
  if (state.slots.length === 0) {
    state.pendingTextures.push(path);
    log(`texture ${basename(path)} will be assigned once a model is loaded`);
    return;
  }
  const slot =
    target ||
    state.slots.find((s) => !s.texture) ||
    (state.slots.length === 1 ? state.slots[0] : null);
  if (slot) setSlotTexture(slot, path);
  else log(`warning: every material already has a texture; drop ${basename(path)} onto a specific material`);
}

// ---------- Parts / bodygroups ----------

/** Group key for a part's input: sanitized like the backend, so "Hat" and "hat" match. */
const groupKey = (value) => sanitize(value) || value.trim();

/** Bodygroups in order of their first part: [{ name, parts: [partName] }]. */
function currentGroups() {
  const groups = new Map();
  for (const p of state.parts) {
    const key = groupKey(state.partGroups.get(p.name) || "");
    if (!key) continue;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(p.name);
  }
  return [...groups].map(([name, parts]) => ({ name, parts }));
}

function renderGroups() {
  const groups = currentGroups();
  const list = $("bodygroups");
  list.textContent = "";
  for (const g of groups) {
    const el = $("group-template").content.firstElementChild.cloneNode(true);
    const blank = el.querySelector(".group-blank");
    el.querySelector(".group-name").textContent = g.name;
    blank.value = state.groupBlank.get(g.name) || "last";
    const describe = () => {
      const options = [...g.parts];
      if (blank.value === "last") options.push("(empty)");
      if (blank.value === "first") options.unshift("(empty)");
      el.querySelector(".group-options").textContent =
        g.parts.length > 1 || blank.value === "none" ? `Options: ${options.join(", ")}` : "";
    };
    blank.addEventListener("change", () => {
      state.groupBlank.set(g.name, blank.value);
      describe();
    });
    describe();
    list.appendChild(el);
  }
  const names = $("group-names");
  names.textContent = "";
  for (const g of groups) names.appendChild(new Option(g.name));
}

function updatePartRow(part) {
  const group = state.partGroups.get(part.name) || "";
  part.el.querySelector(".part-toggle").checked = group !== "";
  part.el.querySelector(".part-group-field").hidden = group === "";
  const input = part.el.querySelector(".part-group");
  if (input.value !== group && document.activeElement !== input) input.value = group;
}

function renderParts(parts) {
  const list = $("parts");
  list.textContent = "";
  state.parts = parts.map((p) => {
    const el = $("part-template").content.firstElementChild.cloneNode(true);
    const part = { name: p.name, triangles: p.triangles, el };
    el.querySelector(".part-name").textContent = p.name;
    el.querySelector(".part-tris").textContent = `${p.triangles} triangle${p.triangles === 1 ? "" : "s"}`;
    el.querySelector(".part-toggle").addEventListener("change", (e) => {
      // A fresh toggleable part gets a group of its own, named after it.
      state.partGroups.set(p.name, e.target.checked ? p.name : "");
      updatePartRow(part);
      renderGroups();
      if (e.target.checked) el.querySelector(".part-group").select();
    });
    el.querySelector(".part-group").addEventListener("input", (e) => {
      state.partGroups.set(p.name, e.target.value);
      renderGroups();
    });
    // Clearing the name makes the part always shown again.
    el.querySelector(".part-group").addEventListener("change", () => updatePartRow(part));
    list.appendChild(el);
    updatePartRow(part);
    return part;
  });
  $("parts-panel").hidden = state.parts.length === 0;
  $("parts-hint").textContent =
    state.parts.length === 1
      ? `The model is a single object, so there is nothing to toggle. To make pieces toggleable, keep them as separate objects in Blender instead of joining them.`
      : "Each object in the model file is a part. Make a part toggleable to switch it on and off in-game (context menu → Bodygroups). Parts given the same group name are alternatives: one of them shows at a time.";
  $("parts").hidden = state.parts.length === 1;
  renderGroups();
}

// ---------- Model ----------

async function loadModel(path) {
  if (path !== state.model) {
    // Different model: start from fresh slots rather than carrying settings across.
    state.slots = [];
    state.remembered.clear();
    state.partGroups.clear();
    state.groupBlank.clear();
  }
  state.model = path;
  state.loadingModel = true;
  $("model-file").textContent = basename(path);
  $("model-hint").textContent = "Reading materials…";
  $("model-card").classList.add("filled");
  if (!state.nameTouched) {
    $("name").value = sanitize(stem(path));
  }
  refresh();
  try {
    const info = await invoke("inspect_model", { path });
    $("model-hint").textContent = `${path} · ${info.triangles} triangles`;
    renderSlots(info.materials);
    renderParts(info.parts);
  } catch (err) {
    state.model = null;
    $("model-hint").textContent = path;
    $("model-card").classList.remove("filled");
    renderSlots([]);
    renderParts([]);
    log(`error: ${err}`);
    showResult(false, "Could not read model", String(err));
  } finally {
    state.loadingModel = false;
    refresh();
  }
}

// ---------- Dialogs / actions ----------

async function browseModel() {
  if (busy() || !state.env) return;
  const path = await open({
    multiple: false,
    filters: [{ name: "Models", extensions: state.env.modelExtensions }],
  });
  if (typeof path === "string") loadModel(path);
}

async function browseTexture(slot) {
  if (busy() || !state.env) return;
  const path = await open({
    multiple: false,
    filters: [{ name: "Images", extensions: state.env.textureExtensions }],
  });
  if (typeof path === "string") setSlotTexture(slot, path);
}

async function convert() {
  if ($("convert").disabled) return;
  state.running = true;
  refresh();
  $("result").hidden = true;
  $("log").textContent = "";
  $("log-box").open = true;
  try {
    const materials = state.slots.map((s) => {
      const v = readSlot(s);
      return {
        slot: s.name,
        texture: v.texture,
        alpha: v.alpha,
        doubleSided: v.doubleSided,
        scroll: v.scrollOn ? { rate: parseFloat(v.rate) || 0, angle: parseFloat(v.angle) || 0 } : null,
      };
    });
    const res = await invoke("convert", {
      req: {
        model: state.model,
        materials,
        name: $("name").value,
        scale: parseFloat($("scale").value),
        surfaceprop: $("surfaceprop").value,
        concave: $("collision").value === "concave",
        bodygroups: currentGroups().map((g) => ({
          name: g.name,
          parts: g.parts,
          blank: state.groupBlank.get(g.name) || "last",
        })),
      },
    });
    state.lastDir = res.dir;
    state.setupShown = false;
    showResult(true, `Created ${res.modelPath}`, res.dir);
    if (state.settings?.openAfterConvert) {
      invoke("open_folder", { path: res.dir }).catch((e) => log(`error: ${e}`));
    }
  } catch (err) {
    log(`error: ${err}`);
    showResult(false, "Conversion failed", String(err));
  } finally {
    state.running = false;
    refresh();
  }
}

// ---------- Environment / settings ----------

async function loadEnvironment() {
  const env = await invoke("environment");
  state.env = env;
  $("model-hint").textContent = state.model ? $("model-hint").textContent : env.modelExtensions.map((e) => "." + e).join(" · ");
  const pill = $("env");
  pill.classList.remove("pending", "ok", "bad");
  if (env.problem) {
    pill.classList.add("bad");
    pill.textContent = "studiomdl not available";
    pill.title = env.problem;
    state.setupShown = true;
    showResult(false, "Setup needed", `${env.problem}\n\nYou can set the path under Settings (gear icon).`);
  } else {
    pill.classList.add("ok");
    pill.textContent = env.wine ? "studiomdl ready (Wine)" : "studiomdl ready";
    pill.title = env.studiomdl;
    if (state.setupShown) {
      $("result").hidden = true;
      state.setupShown = false;
    }
  }
  refresh();
}

/** Puts the default scale and collision onto the main form. */
function applyDefaults(settings) {
  $("scale").value = settings.defaultScale;
  $("collision").value = settings.defaultCollision;
}

function defaultSettings() {
  return {
    outputDir: "",
    modelDir: "bmod",
    keepSources: true,
    openAfterConvert: false,
    fastTextures: false,
    defaultScale: state.env?.defaultScale ?? 39.37,
    defaultCollision: "convex",
    studiomdlPath: "",
    winePath: "",
  };
}

function fillSettingsForm(s) {
  $("set-output-dir").value = s.outputDir;
  $("set-model-dir").value = s.modelDir;
  $("set-keep-sources").checked = s.keepSources;
  $("set-open-after").checked = s.openAfterConvert;
  $("set-fast-textures").checked = s.fastTextures;
  $("set-default-scale").value = s.defaultScale;
  $("set-default-collision").value = s.defaultCollision;
  $("set-studiomdl").value = s.studiomdlPath;
  $("set-wine").value = s.winePath;
  updateModelPreview();
}

function readSettingsForm() {
  return {
    outputDir: $("set-output-dir").value,
    modelDir: $("set-model-dir").value,
    keepSources: $("set-keep-sources").checked,
    openAfterConvert: $("set-open-after").checked,
    fastTextures: $("set-fast-textures").checked,
    defaultScale: parseFloat($("set-default-scale").value),
    defaultCollision: $("set-default-collision").value,
    studiomdlPath: $("set-studiomdl").value,
    winePath: $("set-wine").value,
  };
}

function updateModelPreview() {
  const dir = sanitize($("set-model-dir").value) || "bmod";
  const name = sanitize($("name").value) || "name";
  $("set-model-preview").textContent = `Spawns as models/${dir}/${name}.mdl`;
}

function formatBytes(bytes) {
  if (bytes < 1e6) return `${Math.round(bytes / 1e3)} KB`;
  if (bytes < 1e9) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${(bytes / 1e9).toFixed(2)} GB`;
}

function showCacheInfo(info) {
  const available = info.path != null;
  $("cache-path").textContent = available ? info.path : "Unavailable: no per-user cache folder on this system";
  $("cache-limit").textContent = formatBytes(info.limitBytes);
  $("cache-usage").textContent = !available
    ? "Textures are encoded on every conversion."
    : info.entries === 0
      ? "Empty"
      : `${info.entries} texture${info.entries === 1 ? "" : "s"}, ${formatBytes(info.bytes)}`;
  $("cache-open").disabled = !available;
  // Clearing mid-conversion is harmless but would throw away what it is about to save.
  $("cache-clear").disabled = !available || info.entries === 0 || busy();
}

async function refreshCacheInfo() {
  try {
    showCacheInfo(await invoke("texture_cache_info"));
  } catch (err) {
    $("cache-usage").textContent = String(err);
  }
}

async function clearCache() {
  $("cache-clear").disabled = true;
  try {
    showCacheInfo(await invoke("clear_texture_cache"));
  } catch (err) {
    $("cache-usage").textContent = String(err);
    $("cache-clear").disabled = false;
  }
}

function openSettings() {
  if (!state.settings) return;
  refreshCacheInfo();
  fillSettingsForm(state.settings);
  $("set-output-dir").placeholder = state.env?.defaultOutputRoot || "";
  $("set-wine-field").hidden = !!state.env?.isWindows;
  $("settings-error").hidden = true;
  $("settings").showModal();
}

async function saveSettings(e) {
  e.preventDefault();
  const draft = readSettingsForm();
  // NaN would serialize as null; send 0 so the backend reports "must be positive".
  if (!Number.isFinite(draft.defaultScale)) draft.defaultScale = 0;
  $("settings-save").disabled = true;
  try {
    const prev = state.settings;
    const saved = await invoke("save_settings", { settings: draft });
    state.settings = saved;
    if (saved.defaultScale !== prev.defaultScale || saved.defaultCollision !== prev.defaultCollision) {
      applyDefaults(saved);
    }
    $("settings").close();
    await loadEnvironment();
  } catch (err) {
    const box = $("settings-error");
    box.textContent = String(err);
    box.hidden = false;
  } finally {
    $("settings-save").disabled = false;
  }
}

async function browseSetting(button) {
  const input = $(button.dataset.browse);
  const kind = button.dataset.kind;
  const current = input.value.trim() || undefined;
  const path = await open({
    multiple: false,
    directory: kind === "folder",
    defaultPath: current,
    filters: kind === "exe" ? [{ name: "studiomdl.exe", extensions: ["exe"] }] : undefined,
  });
  if (typeof path === "string") input.value = path;
}

function initSettings() {
  const dialog = $("settings");
  $("settings-open").addEventListener("click", openSettings);
  $("settings-form").addEventListener("submit", saveSettings);
  $("settings-cancel").addEventListener("click", () => dialog.close());
  $("settings-reset").addEventListener("click", () => fillSettingsForm(defaultSettings()));
  $("set-model-dir").addEventListener("input", updateModelPreview);
  $("cache-clear").addEventListener("click", clearCache);
  $("cache-open").addEventListener("click", () =>
    invoke("open_texture_cache").catch((err) => ($("cache-usage").textContent = String(err))),
  );
  // Clicking the backdrop (the dialog element itself, outside the form) closes it.
  dialog.addEventListener("click", (e) => {
    if (e.target === dialog) dialog.close();
  });
  dialog.querySelectorAll("[data-browse]").forEach((b) =>
    b.addEventListener("click", () => browseSetting(b).catch((err) => log(`error: ${err}`))),
  );
}

/** The material slot under a drop position (physical pixels), if any. */
function slotAt(position) {
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(position.x / dpr, position.y / dpr)?.closest(".slot");
  return el ? state.slots.find((s) => s.el === el) : null;
}

async function init() {
  $("model-card").addEventListener("click", browseModel);
  $("name").addEventListener("input", () => (state.nameTouched = $("name").value.trim() !== ""));
  $("convert").addEventListener("click", convert);
  $("open-folder").addEventListener("click", () => {
    if (state.lastDir) invoke("open_folder", { path: state.lastDir }).catch((e) => log(`error: ${e}`));
  });

  await listen("bmod-log", (e) => log(e.payload));

  // Drag & drop: models load; images go to the slot under the cursor, else the first empty slot.
  let hovered = null;
  await getCurrentWebview().onDragDropEvent((e) => {
    const { type, position, paths } = e.payload;
    document.body.classList.toggle("dragging", type === "enter" || type === "over");
    hovered?.el.classList.remove("drop-target");
    hovered = type === "over" ? slotAt(position) : null;
    hovered?.el.classList.add("drop-target");
    if (type !== "drop" || busy() || !state.env) return;

    const target = slotAt(position);
    const model = paths.find((p) => state.env.modelExtensions.includes(ext(p)));
    const textures = paths.filter((p) => state.env.textureExtensions.includes(ext(p)));
    for (const p of paths) {
      if (p !== model && !textures.includes(p)) log(`warning: ignored unsupported file ${basename(p)}`);
    }
    // Queue textures first so they land on the new model's slots.
    if (model) {
      state.pendingTextures.push(...textures);
      loadModel(model);
    } else {
      textures.forEach((p, i) => assignTexture(p, i === 0 ? target : null));
    }
  });

  initSettings();
  state.settings = await invoke("get_settings");
  applyDefaults(state.settings);
  await loadEnvironment();
}

init().catch((e) => log(`error: ${e}`));
