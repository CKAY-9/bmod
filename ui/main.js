const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { getCurrentWebview } = window.__TAURI__.webview;

const $ = (id) => document.getElementById(id);

const state = {
  env: null,
  model: null,
  /** Material slots of the loaded model, in model order: { name, triangles, el, texture } */
  slots: [],
  /** Per-slot settings remembered by slot name, so re-loading a re-exported model keeps them. */
  remembered: new Map(),
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

// ---------- Model ----------

async function loadModel(path) {
  if (path !== state.model) {
    // Different model: start from fresh slots rather than carrying settings across.
    state.slots = [];
    state.remembered.clear();
  }
  state.model = path;
  state.loadingModel = true;
  $("model-file").textContent = basename(path);
  $("model-hint").textContent = "Reading materials…";
  $("model-card").classList.add("filled");
  if (!state.nameTouched) {
    $("name").value = stem(path).toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
  }
  refresh();
  try {
    const info = await invoke("inspect_model", { path });
    $("model-hint").textContent = `${path} · ${info.triangles} triangles`;
    renderSlots(info.materials);
  } catch (err) {
    state.model = null;
    $("model-hint").textContent = path;
    $("model-card").classList.remove("filled");
    renderSlots([]);
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
      },
    });
    state.lastDir = res.dir;
    showResult(true, `Created ${res.modelPath}`, res.dir);
  } catch (err) {
    log(`error: ${err}`);
    showResult(false, "Conversion failed", String(err));
  } finally {
    state.running = false;
    refresh();
  }
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

  const env = await invoke("environment");
  state.env = env;
  $("scale").value = env.defaultScale;
  $("model-hint").textContent = env.modelExtensions.map((e) => "." + e).join(" · ");
  const pill = $("env");
  pill.classList.remove("pending");
  if (env.problem) {
    pill.classList.add("bad");
    pill.textContent = "studiomdl not available";
    pill.title = env.problem;
    showResult(false, "Setup needed", env.problem);
  } else {
    pill.classList.add("ok");
    pill.textContent = env.wine ? "studiomdl ready (Wine)" : "studiomdl ready";
    pill.title = env.studiomdl;
  }
  refresh();
}

init().catch((e) => log(`error: ${e}`));
