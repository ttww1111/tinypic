import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { getVersion } from "@tauri-apps/api/app";

// GitHub 仓库地址
const GITHUB_REPO_URL = "https://github.com/ttww1111/tinypic";

const $ = (id) => document.getElementById(id);
const app = $("app"), drop = $("drop"), listEl = $("list"), dropTitle = $("dropTitle");
const progressWrap = $("progressWrap"), progressFill = $("progressFill"), progressText = $("progressText"), noticeEl = $("notice");
const files = new Map();
const SETTINGS_KEY = "tiny.settings.v1";
const defaults = { outputMode: "ask", suffix: "_tiny", targetWidth: "", targetHeight: "", keepCopyright: false, keepLocation: false, keepCreation: false };
let settings = loadSettings(), running = false;

function loadSettings() {
  let s;
  try { s = { ...defaults, ...JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}") }; } catch { s = { ...defaults }; }
  return s;
}
function saveSettings() { localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)); }
function humanSize(n) { const u = ["B", "KB", "MB", "GB"]; let v = n || 0, i = 0; while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; } return i ? `${v.toFixed(1)} ${u[i]}` : `${Math.round(v)} B`; }
function showNotice(text) { noticeEl.textContent = text; clearTimeout(showNotice.timer); showNotice.timer = setTimeout(() => noticeEl.textContent = "", 3500); }
function statusOf(r) { if (r.error) return r.error === "已取消" ? "cancel" : "err"; return r.kept ? "keep" : "ok"; }
function badgeFor(format) {
  const f = (format || "").toUpperCase();
  if (f.includes("PNG")) return { label: "PNG", cls: "badge badge-png" };
  if (f.includes("JPEG") || f.includes("JPG")) return { label: "JPEG", cls: "badge badge-jpg" };
  if (f.includes("WEBP")) return { label: "WEBP", cls: "badge badge-webp" };
  return { label: f || "图片", cls: "badge badge-other" };
}
function rowData(f) {
  const { label } = badgeFor(f.format || f.mime);
  const dims = f.width && f.height ? `${f.width}×${f.height}` : "";
  const info = `${humanSize(f.size)}${dims ? " · " + dims : ""}`;
  let state = "待压缩", cls = "pending";
  if (f.status === "running") { state = "压缩中…"; cls = "run"; }
  else if (f.status === "ok") { const pct = f.size ? ((1 - f.new / f.size) * 100).toFixed(1) : "0.0"; state = `节省 ${pct}% · 压缩后 ${humanSize(f.new)}`; cls = "ok"; }
  else if (f.status === "keep") { state = "已最优"; cls = "keep"; }
  else if (f.status === "cancel") { state = "已取消，原文件未改动"; cls = "err"; }
  else if (f.status === "err") { state = "失败"; cls = "err"; }
  return { label, info, state, cls };
}
function fillRow(row, f) {
  const d = rowData(f);
  const thumb = row.querySelector(".thumb");
  const fallback = row.querySelector(".thumb-fallback");
  const badge = row.querySelector(".badge");
  if (thumb && fallback) {
    if (f.thumbnail) { if (thumb.src !== f.thumbnail) thumb.src = f.thumbnail; thumb.hidden = false; fallback.hidden = true; }
    else { thumb.hidden = true; fallback.hidden = false; }
    thumb.alt = f.name;
  }
  if (badge) { badge.className = d.cls; badge.textContent = d.label; }
  const name = row.querySelector(".name");
  if (name) name.textContent = f.name;
  const metaText = row.querySelector(".meta-text");
  if (metaText) metaText.textContent = d.info;
  const state = row.querySelector(".state");
  if (state) { state.textContent = d.state; state.className = `state ${d.cls}`; state.title = f.error || ""; }
}
function makeRow(path, f) {
  const row = document.createElement("div");
  row.className = "row";
  row.dataset.path = path;
  row.innerHTML = `<div class="thumb-wrap"><img class="thumb" alt=""><div class="thumb-fallback" hidden></div></div><div class="row-body"><div class="name"></div><div class="meta"><span class="badge badge-other"></span><span class="meta-text"></span></div></div><div class="state"></div>`;
  fillRow(row, f);
  return row;
}
function updateRowEl(path) { const f = files.get(path); const row = listEl.querySelector(`[data-path="${CSS.escape(path)}"]`); if (f && row) fillRow(row, f); }
function updateSummary() {
  let done = 0;
  for (const f of files.values()) { if (["ok", "keep", "err", "cancel"].includes(f.status)) done++; }
  if (!files.size) { progressText.textContent = ""; }
  else if (running) { progressText.textContent = `处理中 ${done} / ${files.size}`; }
  else if (done === files.size) { progressText.textContent = `${files.size} 个文件已完成`; }
  else { progressText.textContent = `${files.size - done} 个文件待压缩`; }
}
function render() {
  listEl.replaceChildren(...[...files].map(([p, f]) => makeRow(p, f)));
  const has = files.size > 0;
  app.classList.toggle("has-files", has);
  dropTitle.textContent = has ? "拖放以添加更多图片" : "拖放图片到此处";
  updateSummary();
}
async function hydrate(paths) {
  for (let i = 0; i < paths.length; i += 50) {
    const batch = paths.slice(i, i + 50);
    try {
      const infos = await invoke("image_infos", { paths: batch });
      for (const info of infos) { if (files.has(info.path)) { Object.assign(files.get(info.path), info); if (info.width && info.height) setRefDims(info.width, info.height); updateRowEl(info.path); } }
    } catch (e) { showNotice(`读取图片信息失败：${e}`); }
  }
  updateSummary();
}
async function addPaths(paths) {
  if (running) { showNotice("当前有压缩任务进行中，请稍后再添加"); return; }
  const supported = /\.(png|jpe?g|webp)$/i;
  const valid = [], folders = [];
  for (const p of paths) { if (supported.test(p)) valid.push(p); else folders.push(p); }
  if (folders.length) showNotice("不支持文件夹");
  const added = [];
  for (const p of valid) if (!files.has(p)) { const name = p.split(/[\\/]/).pop(); files.set(p, { path: p, name, size: 0, status: "pending", thumbnail: null }); added.push(p); }
  if (added.length) {
    if (!running) { files.clear(); for (const p of added) { const name = p.split(/[\\/]/).pop(); files.set(p, { path: p, name, size: 0, status: "pending", thumbnail: null }); } }
    render();
    await hydrate(added);
    maybeStart();
  }
}
async function openFiles() { try { const selected = await open({ multiple: true, filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp"] }] }); if (selected?.length) addPaths(selected); } catch (e) { console.error(e); } }
drop.addEventListener("click", e => { if (e.target === drop || e.target.closest(".drop-inner")) openFiles(); });
async function initDragDrop() {
  try {
    await getCurrentWebview().onDragDropEvent(e => {
      const p = e.payload;
      if (p.type === "over") { drop.classList.add("dragover"); }
      else if (p.type === "drop") { drop.classList.remove("dragover"); if (p.paths?.length) addPaths(p.paths); }
      else { drop.classList.remove("dragover"); }
    });
  } catch (e) { console.error(e); }
}
initDragDrop();

document.addEventListener("paste", async e => {
  if (running) return;
  const items = [...(e.clipboardData?.items || [])].filter(i => i.type.startsWith("image/"));
  if (!items.length) return;
  e.preventDefault();
  for (const item of items) {
    const file = item.getAsFile();
    if (!file) continue;
    const data = await new Promise((resolve, reject) => { const r = new FileReader(); r.onload = () => resolve(r.result); r.onerror = reject; r.readAsDataURL(file); });
    try {
      const saved = await invoke("save_clipboard_image", { data });
      files.set(saved.path, { ...saved, path: saved.path, name: saved.name, size: saved.size, width: saved.width, height: saved.height, format: saved.format, mime: saved.mime, status: "pending", thumbnail: null, temp: true });
      render();
      await hydrate([saved.path]);
      maybeStart();
    } catch (err) { showNotice(String(err)); }
  }
});

let refDims = null; // 参考图尺寸，用于「设一项自动等比换算另一项」
function setRefDims(w, h) { if (!refDims && w && h) refDims = { w, h }; }
function syncSizeUI() {
  const wEl = $("targetWidth"), hEl = $("targetHeight");
  if (!wEl || !hEl) return;
  wEl.value = settings.targetWidth || "";
  hEl.value = settings.targetHeight || "";
  wEl.placeholder = refDims ? String(refDims.w) : "";
  hEl.placeholder = refDims ? String(refDims.h) : "";
  syncSizeDisable();
}
function syncSizeDisable() {
  const wEl = $("targetWidth"), hEl = $("targetHeight");
  if (!wEl || !hEl) return;
  const wOk = !!(wEl.value.trim() && Number(wEl.value) > 0);
  const hOk = !!(hEl.value.trim() && Number(hEl.value) > 0);
  if (wOk) { wEl.disabled = false; hEl.disabled = true; }      // 宽度已填 → 高度锁定（自动换算）
  else if (hOk) { hEl.disabled = false; wEl.disabled = true; } // 高度已填 → 宽度锁定
  else { wEl.disabled = false; hEl.disabled = false; }
}
function linkSize() {
  const wEl = $("targetWidth"), hEl = $("targetHeight");
  if (!wEl || !hEl) return;
  const wRaw = wEl.value.trim(), hRaw = hEl.value.trim();
  const wv = parseInt(wRaw, 10), hv = parseInt(hRaw, 10);
  const wOk = wRaw !== "" && !Number.isNaN(wv) && wv > 0;
  const hOk = hRaw !== "" && !Number.isNaN(hv) && hv > 0;
  if (wOk) {
    if (hOk && refDims && refDims.w && refDims.h) {
      const h = Math.round(wv * refDims.h / refDims.w);
      settings.targetWidth = String(wv); settings.targetHeight = String(h); hEl.value = String(h);
    } else {
      settings.targetWidth = String(wv); settings.targetHeight = hEl.value.trim();
    }
    wEl.disabled = false; hEl.disabled = true;
  } else if (hOk) {
    if (refDims && refDims.w && refDims.h) {
      const w = Math.round(hv * refDims.w / refDims.h);
      settings.targetHeight = String(hv); settings.targetWidth = String(w); wEl.value = String(w);
    } else {
      settings.targetHeight = String(hv); settings.targetWidth = wEl.value.trim();
    }
    hEl.disabled = false; wEl.disabled = true;
  } else {
    settings.targetWidth = wRaw; settings.targetHeight = hRaw;
    wEl.disabled = false; hEl.disabled = false;
  }
  saveSettings();
}
function syncSettingsUI() {
  $("keepCopyright").checked = settings.keepCopyright;
  $("keepLocation").checked = settings.keepLocation;
  $("keepCreation").checked = settings.keepCreation;
  $("suffix").value = settings.suffix;
  const mode = document.querySelector(`input[name=outputMode][value="${settings.outputMode}"]`);
  if (mode) mode.checked = true;
  syncSizeUI();
  updateSuffixState();
}
function updateSuffixState() {
  const enabled = settings.outputMode === "new_file" || settings.outputMode === "ask";
  $("suffix").disabled = !enabled;
}

function openSettings() { syncSettingsUI(); $("settingsSheet").hidden = false; }
function closeSettings() { $("settingsSheet").hidden = true; }
$("settingsBtn").addEventListener("click", openSettings);
$("settingsClose").addEventListener("click", closeSettings);
$("settingsBackdrop").addEventListener("click", closeSettings);
$("settingsHandle").addEventListener("click", closeSettings);
$("suffix").addEventListener("input", e => { settings.suffix = e.target.value; saveSettings(); });
document.querySelectorAll("input[name=outputMode]").forEach(x => x.addEventListener("change", e => { settings.outputMode = e.target.value; saveSettings(); updateSuffixState(); }));

/* 调整尺寸已并入设置页：宽/高同排，设一项自动按参考图等比换算另一项 */
$("targetWidth").addEventListener("input", linkSize);
$("targetHeight").addEventListener("input", linkSize);
$("keepCopyright").addEventListener("change", e => { settings.keepCopyright = e.target.checked; saveSettings(); });
$("keepLocation").addEventListener("change", e => { settings.keepLocation = e.target.checked; saveSettings(); });
$("keepCreation").addEventListener("change", e => { settings.keepCreation = e.target.checked; saveSettings(); });

/* 版本号点击：在外部浏览器打开 GitHub 仓库主页 */
$("versionTag").addEventListener("click", () => {
  openUrl(GITHUB_REPO_URL).catch(e => showNotice(`无法打开链接：${e}`));
});

function maybeStart() { if (!running && [...files.values()].some(f => f.status === "pending")) startCompression(); }
function askChoice() {
  return new Promise(resolve => {
    const m = $("choiceModal");
    const remember = $("choiceRemember");
    remember.checked = false;
    m.hidden = false;
    const done = v => { m.hidden = true; m.querySelectorAll("[data-choice]").forEach(b => b.onclick = null); remember.onchange = null; if (v && remember.checked) { settings.outputMode = v; saveSettings(); } resolve(v); };
    remember.onchange = () => {};
    m.querySelectorAll("[data-choice]").forEach(b => b.onclick = () => done(b.dataset.choice));
  });
}
async function resolveMode() {
  if (settings.outputMode === "replace") {
    const dim = settings.targetWidth ? `宽度 ${settings.targetWidth}px` : settings.targetHeight ? `高度 ${settings.targetHeight}px` : null;
    if (dim && !confirm(`替换原文件并缩放到${dim}，原文件将被覆盖。确定继续吗？`)) return null;
    if (!confirm("危险操作：将替换每个原文件。确定继续吗？")) return null;
  }
  return settings.outputMode;
}
async function startCompression() {
  if (!files.size || running) return;
  const mode = await resolveMode();
  if (!mode) return;
  running = true;
  drop.classList.add("busy");
  progressWrap.hidden = false;
  progressFill.style.width = "0%";
  for (const f of files.values()) { if (f.status === "pending") { f.status = "running"; f.new = null; f.outputPath = null; } }
  render();
  const backendMode = mode === "ask" ? "new_file" : mode;
  const unlisten = await listen("progress", ev => {
    const { done, total, item, phase } = ev.payload;
    if (item) {
      const f = files.get(item.path);
      if (f) { Object.assign(f, { size: item.orig || f.size, new: item.new, outputPath: item.output_path, error: item.error || null, status: phase === "compress" ? "running" : statusOf(item) }); updateRowEl(item.path); }
    }
    progressFill.style.width = `${done / Math.max(total, 1) * 100}%`;
    progressText.textContent = `${phase === "deliver" ? "写入" : "压缩"} ${done} / ${total}`;
  });
  const payload = {
    scalePercent: 100,
    outputMode: backendMode,
    suffix: settings.suffix,
    targetWidth: settings.targetWidth ? Number(settings.targetWidth) : null,
    targetHeight: settings.targetHeight ? Number(settings.targetHeight) : null,
    keepCopyright: settings.keepCopyright,
    keepLocation: settings.keepLocation,
    keepCreation: settings.keepCreation,
  };
  try {
    const result = await invoke("compress_batch", { paths: [...files.keys()], settings: payload });
    applyResult(result);
    const hasSuccess = [...files.values()].some(f => f.status === "ok" || f.status === "keep");
    if (mode === "ask" && !result.cancelled && hasSuccess) {
      const choice = await askChoice();
      if (choice === "replace") {
        const mapping = result.files.filter(f => f.output_path && f.error == null).map(f => ({ path: f.path, output_path: f.output_path }));
        try { const committed = await invoke("replace_output_files", { mapping }); applyReplace(committed, result); }
        catch (err) { showNotice(`替换失败：${err}`); }
      }
    }
    if (result.cancelled) showNotice("已取消，原文件未改动");
  } catch (e) {
    for (const f of files.values()) if (f.status === "running") { f.status = "err"; f.error = String(e); }
    showNotice(`出错：${e}`);
    render();
  } finally {
    unlisten();
    running = false;
    drop.classList.remove("busy");
    progressWrap.hidden = true;
    render();
    maybeStart();
  }
}
function applyResult(result) {
  for (const r of result.files || []) {
    const f = files.get(r.path);
    if (f) { Object.assign(f, { size: r.orig || f.size, new: r.new, outputPath: r.output_path, error: r.error || null, status: statusOf(r), width: r.width || f.width, height: r.height || f.height }); if (r.width && r.height) setRefDims(r.width, r.height); updateRowEl(r.path); }
  }
  $("outbar").hidden = false;
  updateOutPath();
  updateSummary();
}
/* 收集所有不同的输出目录（按各自原目录） */
function distinctOutputDirs() {
  const dirs = new Set();
  for (const f of files.values()) {
    const p = f.outputPath || f.path;
    if (!p) continue;
    const parts = p.split(/[\\/]/); parts.pop();
    if (parts.length) dirs.add(parts.join("\\"));
  }
  return [...dirs];
}
function updateOutPath() {
  const dirs = distinctOutputDirs();
  const el = $("outPath");
  if (!el) return;
  if (dirs.length === 0) el.textContent = "";
  else if (dirs.length === 1) el.textContent = dirs[0];
  else el.textContent = `多个文件夹 · 共 ${dirs.length} 个来源目录`;
}
function applyReplace(committed, result) {
  const byPath = new Map(result.files.map(f => [f.path, f]));
  for (const c of committed) {
    const f = files.get(c.path);
    if (!f) continue;
    const orig = byPath.get(c.path);
    if (c.error) { f.status = "err"; f.error = c.error; }
    else { f.status = "ok"; f.error = null; f.outputPath = c.path; if (orig) { f.new = orig.new; } }
    updateRowEl(c.path);
  }
  updateSummary();
}
$("openBtn").addEventListener("click", async () => {
  const dirs = distinctOutputDirs();
  if (!dirs.length) { showNotice("没有可打开的输出位置"); return; }
  if (dirs.length === 1) {
    openPath(dirs[0]).catch(e => showNotice(`无法打开文件夹：${e}`));
    return;
  }
  // 多个来源目录：逐个打开（最多 6 个，避免一次弹出太多资源管理器）
  const cap = 6;
  const toOpen = dirs.slice(0, cap);
  await Promise.all(toOpen.map(d => openPath(d).catch(e => console.error(e))));
  showNotice(dirs.length > cap ? `已打开前 ${cap} 个文件夹（共 ${dirs.length} 个来源目录）` : `已打开 ${dirs.length} 个文件夹`);
});
function clearList() {
  if (running) { showNotice("当前有压缩任务进行中，无法清空"); return; }
  files.clear();
  render();
  $("outbar").hidden = true;
}
$("clearBtn").addEventListener("click", clearList);
render();
syncSizeUI();
(async () => { try { const v = await getVersion(); const tag = $("versionTag"); if (tag) tag.textContent = "v" + v; } catch (e) {} })();
