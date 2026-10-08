import { invoke } from "@tauri-apps/api/core";
import { listen, emitTo } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const MAIN_LABEL = "main";
const SUPPORTED = /\.(png|jpe?g|webp)$/i;
const IDLE_TEXT = "拖拽至此压缩";
const HOVER_TEXT = "松手即可压缩";
const RESULT_HOLD_MS = 8000; // 结果卡停留时长
const ASK_HOLD_MS = 20000; // 等待选择的最长时长，超时按「保留新文件」处理（安全的那一边）
const PILL_FLASH_MS = 2600; // 收拢回胶囊后结果文案的停留时长

// 悬浮窗不读 localStorage：跨 webview 的存储是不是同一份不是保证行为，
// 设置统一由主窗推到 Rust 侧镜像，这里每次压缩前重新取一次。
const DEFAULTS = { outputMode: "ask", suffix: "_tiny", targetWidth: "", targetHeight: "", keepCopyright: false, keepLocation: false, keepCreation: false };

const pill = document.getElementById("pill");
const textEl = document.getElementById("pillText");
const dropText = document.getElementById("dropText");
const cardHead = document.getElementById("cardHead");
const cardClose = document.getElementById("cardClose");
const workPhase = document.getElementById("workPhase");
const workCount = document.getElementById("workCount");
const workFill = document.getElementById("workFill");
const workFile = document.getElementById("workFile");
const resultTitle = document.getElementById("resultTitle");
const resultSub = document.getElementById("resultSub");
const resultHero = document.getElementById("resultHero");
const resultHeroNum = document.getElementById("resultHeroNum");
const resultActions = document.getElementById("resultActions");

let settings = { ...DEFAULTS };
/* 当前视图：idle 显示胶囊，drop / work / result 三种共用同一张展开卡片。 */
let view = "idle";
let busy = false;
let holdTimer = 0;
let pillTimer = 0;
let pressPoint = null;
/* 用户按了卡片上的「取消」：这次拖拽期间不再弹卡片，等下一次拖拽再恢复。 */
let dragSuppressed = false;
/* 「需要选择保存方式」时暂存这一批的结果，等用户点按钮。 */
let pending = null;

/* ===== 小工具 ===== */

function humanSize(value) {
  const units = ["B", "KB", "MB", "GB"];
  let size = Number(value) || 0;
  let index = 0;
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024;
    index += 1;
  }
  return index ? `${size.toFixed(1)} ${units[index]}` : `${Math.round(size)} B`;
}

function shortError(error) {
  return String(error ?? "操作失败").replace(/^Error:\s*/, "").trim() || "操作失败";
}

function replayEnter() {
  pill.classList.remove("enter");
  void pill.offsetWidth;
  pill.classList.add("enter");
}

/* ===== 视图切换 =====
   窗口尺寸由 Rust 侧改（前端不该拿 set_size 权限，也不方便换算阴影留白），
   这里只负责切视图，并告诉 Rust 该展开还是收拢。 */
async function setView(next) {
  if (view === next) return;
  const wasIdle = view === "idle";
  view = next;
  document.body.dataset.view = next;
  if (wasIdle !== (next === "idle")) {
    try {
      await invoke("float_card_mode", { active: next !== "idle" });
    } catch (error) {
      console.error(error);
    }
  }
}

/* 收拢回胶囊。flash 是一句结果文案，会在胶囊上再停留一会儿，免得结果一闪而过。 */
async function collapse(flash) {
  clearTimeout(holdTimer);
  holdTimer = 0;
  pending = null;
  document.body.classList.remove("drag-active", "is-err", "is-ask");
  await setView("idle");
  replayEnter();
  clearTimeout(pillTimer);
  if (flash) {
    textEl.textContent = flash;
    pillTimer = setTimeout(() => { textEl.textContent = IDLE_TEXT; }, PILL_FLASH_MS);
  } else {
    textEl.textContent = IDLE_TEXT;
  }
}

/* ===== 三个内容区各自的填充 ===== */

function setWork(done, total, phase, name) {
  const ratio = total ? Math.min(done / total, 1) : 0;
  workPhase.textContent = phase === "deliver" ? "写入文件" : "压缩中";
  workCount.textContent = `${done} / ${total}`;
  workFill.style.width = `${ratio * 100}%`;
  if (name) workFile.textContent = name;
}

/* kind: ok | err | ask；flash 是收拢回胶囊时显示的那句文案；
   savedPct 有值且 kind 为 ok 时，结果卡会把「省 X%」亮成大数字 */
function showResult(kind, title, sub, flash, savedPct) {
  clearTimeout(holdTimer);
  document.body.classList.toggle("is-err", kind === "err");
  document.body.classList.toggle("is-ask", kind === "ask");
  resultTitle.textContent = title;
  resultSub.textContent = sub || "";
  resultHeroNum.textContent = `${savedPct}%`;
  resultHero.hidden = !(kind === "ok" && savedPct != null);
  resultActions.hidden = kind !== "ask";
  setView("result");
  holdTimer = setTimeout(
    () => collapse(flash || ""),
    kind === "ask" ? ASK_HOLD_MS : RESULT_HOLD_MS,
  );
}

/* ===== 拖拽 ===== */

function initDragDrop() {
  getCurrentWebview().onDragDropEvent(event => {
    const payload = event.payload;
    if (payload.type === "enter" || payload.type === "over") {
      // enter 是一次新拖拽的开始：把上一轮「按了取消」的状态清掉
      if (payload.type === "enter") dragSuppressed = false;
      if (busy || dragSuppressed) return;
      openDrop();
      return;
    }
    if (payload.type === "drop") {
      document.body.classList.remove("drag-active");
      if (dragSuppressed) {
        dragSuppressed = false;
        return;
      }
      if (busy) return;
      handleDrop(payload.paths ?? []);
      return;
    }
    // leave：鼠标离开命中范围。只有还停在投放态时才收拢，
    // 压缩中 / 结果态要留在原地，不能被一次 leave 打回胶囊。
    dragSuppressed = false;
    if (busy) return;
    if (view === "drop") collapse();
  }).catch(error => console.error(error));
}

function openDrop() {
  clearTimeout(holdTimer);
  holdTimer = 0;
  clearTimeout(pillTimer);
  document.body.classList.remove("is-err", "is-ask");
  dropText.textContent = HOVER_TEXT;
  document.body.classList.add("drag-active");
  setView("drop");
}

async function handleDrop(paths) {
  const images = paths.filter(path => SUPPORTED.test(path));
  if (!images.length) {
    showResult("err", "格式不支持", "只能压缩 PNG / JPEG / WebP 图片");
    return;
  }
  await refreshSettings();
  // 「替换原文件」「存为新文件」直接按设置跑；「每次询问」或还没选过，也先把压缩跑起来
  // （先落成新文件），压完再在卡片里问要不要覆盖 —— 这样「松手即压缩」在任何设置下都成立，
  // 而不确定的那一步也不会弹出主界面。
  const replace = settings.outputMode === "replace";
  await compress(images, replace ? "replace" : "new_file", !replace && settings.outputMode !== "new_file");
}

/* ===== 压缩 ===== */

async function compress(paths, backendMode, askAfter) {
  clearTimeout(holdTimer);
  holdTimer = 0;
  clearTimeout(pillTimer);
  document.body.classList.remove("is-err", "is-ask", "drag-active");
  busy = true;
  workFile.textContent = "";
  setWork(0, paths.length, "compress", "");
  setView("work");

  const unlisten = await listen("progress", event => {
    const payload = event.payload ?? {};
    const done = Number(payload.done) || 0;
    const total = Number(payload.total) || paths.length;
    setWork(done, total, payload.phase, payload.item?.name);
  });

  let result = null;
  let failure = null;
  try {
    result = await invoke("compress_batch", { paths, settings: payloadFor(backendMode) });
  } catch (error) {
    failure = error;
  } finally {
    unlisten();
    busy = false;
  }

  if (failure) {
    showResult("err", "压缩失败", shortError(failure));
    return;
  }
  if (result?.cancelled) {
    showResult("err", "已取消", "原文件未改动");
    return;
  }
  // 主窗开着就让它把这批结果并进列表，否则双击回主界面看不到任何痕迹
  emitTo(MAIN_LABEL, "float-result", result).catch(() => {});
  summarize(result, backendMode, askAfter);
}

function summarize(result, backendMode, askAfter) {
  const files = result?.files ?? [];
  const done = files.filter(file => !file.error && file.new > 0);
  if (!done.length) {
    const failed = files.find(file => file.error);
    showResult("err", "压缩失败", shortError(failed?.error));
    return;
  }
  const before = done.reduce((sum, file) => sum + file.orig, 0);
  const after = done.reduce((sum, file) => sum + file.new, 0);
  const saved = before ? Math.max(Math.round((1 - after / before) * 100), 0) : 0;
  const failedCount = files.length - done.length;
  const title = failedCount ? `${done.length} 张完成 · ${failedCount} 张失败` : `${done.length} 张压缩完成`;
  const sizes = `${humanSize(before)} → ${humanSize(after)} · 省 ${saved}%`;
  const flash = `${done.length} 张 · 省 ${saved}%`;

  if (!askAfter) {
    showResult(
      "ok",
      title,
      backendMode === "replace" ? `${humanSize(before)} → ${humanSize(after)} · 已替换原图` : sizes,
      flash,
      saved,
    );
    return;
  }
  pending = { done, before, after, saved, result };
  showResult("ask", `${done.length} 张压缩完成，怎么保存？`, sizes, `${done.length} 张 · 省 ${saved}%`);
}

/* 卡片上那两个按钮：只对「每次询问」这一批生效，不去改用户设置里的长期选择。 */
async function chooseMode(mode) {
  if (!pending || busy) return;
  const { done, before, after, saved, result } = pending;
  pending = null;
  clearTimeout(holdTimer);
  holdTimer = 0;
  resultActions.hidden = true;

  const sizes = `${humanSize(before)} → ${humanSize(after)} · 省 ${saved}%`;
  const flash = `${done.length} 张 · 省 ${saved}%`;
  if (mode !== "replace") {
    showResult("ok", `${done.length} 张已存为新文件`, sizes, flash, saved);
    return;
  }

  // 覆盖原图不可逆：把刚压出来的文件原子替换回原路径。只在用户明确点了这个按钮时才做。
  busy = true;
  setView("work");
  workPhase.textContent = "正在替换原图";
  workCount.textContent = `${done.length} / ${done.length}`;
  workFill.style.width = "100%";
  workFile.textContent = "";
  try {
    const mapping = done.map(file => ({ path: file.path, output_path: file.output_path }));
    const committed = await invoke("replace_output_files", { mapping });
    const replaced = new Set(committed.filter(item => !item.error).map(item => item.path));
    const failedCount = done.length - replaced.size;
    if (!replaced.size) {
      showResult("err", "替换失败", shortError(committed.find(item => item.error)?.error));
      return;
    }
    // 回主界面的那份结果也要跟着改成「已替换」后的路径
    const updated = (result.files ?? []).map(file =>
      replaced.has(file.path) ? { ...file, output_path: file.path } : file);
    emitTo(MAIN_LABEL, "float-result", { ...result, files: updated }).catch(() => {});
    showResult(
      failedCount ? "err" : "ok",
      failedCount ? `${replaced.size} 个已替换 · ${failedCount} 个失败` : `已替换 ${replaced.size} 个原文件`,
      sizes,
      failedCount ? `${replaced.size} 个已替换` : `已替换 ${replaced.size} 个`,
      saved,
    );
  } catch (error) {
    showResult("err", "替换失败", shortError(error));
  } finally {
    busy = false;
  }
}

function payloadFor(mode) {
  const width = Number.parseInt(settings.targetWidth, 10);
  const height = Number.parseInt(settings.targetHeight, 10);
  return {
    scalePercent: 100,
    outputMode: mode,
    suffix: settings.suffix || "_tiny",
    targetWidth: Number.isFinite(width) && width > 0 ? width : null,
    targetHeight: Number.isFinite(height) && height > 0 ? height : null,
    keepCopyright: !!settings.keepCopyright,
    keepLocation: !!settings.keepLocation,
    keepCreation: !!settings.keepCreation,
  };
}

async function refreshSettings() {
  try {
    const stored = await invoke("get_settings");
    if (stored && typeof stored === "object" && !Array.isArray(stored)) {
      settings = { ...DEFAULTS, ...stored };
    }
  } catch (error) {
    console.error(error);
  }
}

/* ===== 拖动 / 双击 / 右键 ===== */

/* 手动判定「拖动」：位移超过 4px 才交给系统拖窗口，否则留给 dblclick */
function attachDrag(element) {
  element.addEventListener("mousedown", event => {
    if (event.button !== 0 || event.target.closest("button")) return;
    pressPoint = { x: event.clientX, y: event.clientY };
  });
  element.addEventListener("mousemove", async event => {
    if (!pressPoint) return;
    if (Math.hypot(event.clientX - pressPoint.x, event.clientY - pressPoint.y) < 4) return;
    pressPoint = null;
    element.classList.add("dragging");
    try {
      await getCurrentWindow().startDragging();
    } catch (error) {
      console.error(error);
    }
    setTimeout(() => element.classList.remove("dragging"), 200);
  });
}

function initMouseActions() {
  pill.addEventListener("dblclick", event => {
    event.preventDefault();
    invoke("show_main_window").catch(() => {});
  });

  cardClose.addEventListener("click", event => {
    event.stopPropagation();
    // 压缩中：这颗按钮是「取消压缩」；其余情况是关掉卡片
    if (view === "work") {
      workPhase.textContent = "正在取消";
      invoke("cancel_compress").catch(() => {});
      return;
    }
    if (view === "drop") dragSuppressed = true;
    collapse();
  });

  // 右键弹自定义菜单（独立小窗，见 desktop.rs 的 float_context_menu）
  window.addEventListener("contextmenu", event => {
    event.preventDefault();
    invoke("float_context_menu").catch(() => {});
  });

  // 系统拖放已由 Tauri 接管（dragDropEnabled），这里只是兜住浏览器默认行为
  window.addEventListener("dragover", event => event.preventDefault());
  window.addEventListener("drop", event => event.preventDefault());
}

async function boot() {
  await refreshSettings();
  initDragDrop();
  attachDrag(pill);
  attachDrag(cardHead);
  initMouseActions();
  // 先挂监听再让 Rust 显示窗口，否则进场动效会在窗口还没显示时播完
  await listen("float-shown", replayEnter).catch(error => console.error(error));
  try {
    await invoke("reveal_float");
  } catch (error) {
    console.error(error);
  }
}

boot();
