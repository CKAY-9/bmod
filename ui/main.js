const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { getCurrentWebview } = window.__TAURI__.webview;

const $ = (id) => document.getElementById(id);

const state = {
  model: null,
  texture: null,
  env: null,
  running: false,
  nameTouched: false,
  lastDir: null,
};

const ext = (path) => (path.split(".").pop() || "").toLowerCase();
const basename = (path) => path.split(/[\\/]/).pop();
const stem = (path) => basename(path).replace(/\.[^.]+$/, "");

function setFile(kind, path) {
  state[kind] = path;
  $(`${kind}-file`).textContent = basename(path);
  $(`${kind}-hint`).textContent = path;
  $(`${kind}-card`).classList.add("filled");
  if (kind === "model" && !state.nameTouched) {
    $("name").value = stem(path).toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
  }
  refresh();
}

function refresh() {
  const ready = state.model && state.texture && state.env && !state.env.problem;
  $("convert").disabled = !ready || state.running;
  $("convert").textContent = state.running ? "Converting…" : "Convert";
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

async function browse(kind) {
  if (state.running || !state.env) return;
  const extensions = kind === "model" ? state.env.modelExtensions : state.env.textureExtensions;
  const path = await open({
    multiple: false,
    directory: false,
    filters: [{ name: kind === "model" ? "Models" : "Images", extensions }],
  });
  if (typeof path === "string") setFile(kind, path);
}

async function convert() {
  if ($("convert").disabled) return;
  state.running = true;
  refresh();
  $("result").hidden = true;
  $("log").textContent = "";
  $("log-box").open = true;
  try {
    const res = await invoke("convert", {
      req: {
        model: state.model,
        texture: state.texture,
        name: $("name").value,
        scale: parseFloat($("scale").value),
        surfaceprop: $("surfaceprop").value,
        alpha: $("alpha").value,
        doubleSided: $("double-sided").checked,
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

async function init() {
  for (const kind of ["model", "texture"]) {
    $(`${kind}-card`).addEventListener("click", () => browse(kind));
  }
  $("name").addEventListener("input", () => (state.nameTouched = $("name").value.trim() !== ""));
  $("convert").addEventListener("click", convert);
  $("open-folder").addEventListener("click", () => {
    if (state.lastDir) invoke("open_folder", { path: state.lastDir }).catch((e) => log(`error: ${e}`));
  });

  await listen("bmod-log", (e) => log(e.payload));

  // Drag & drop: files are routed to the model or texture slot by extension.
  const cards = document.querySelectorAll(".file-card");
  await getCurrentWebview().onDragDropEvent((e) => {
    const type = e.payload.type;
    cards.forEach((c) => c.classList.toggle("dragging", type === "enter" || type === "over"));
    if (type !== "drop" || state.running || !state.env) return;
    for (const path of e.payload.paths) {
      if (state.env.modelExtensions.includes(ext(path))) setFile("model", path);
      else if (state.env.textureExtensions.includes(ext(path))) setFile("texture", path);
      else log(`warning: ignored unsupported file ${basename(path)}`);
    }
  });

  const env = await invoke("environment");
  state.env = env;
  $("scale").value = env.defaultScale;
  $("model-hint").textContent = env.modelExtensions.map((e) => "." + e).join(" · ");
  $("texture-hint").textContent = env.textureExtensions.map((e) => "." + e).join(" · ");
  $("output-hint").textContent = `Output goes to ${env.outputRoot}`;
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
