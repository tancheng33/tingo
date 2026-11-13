/* 外框：侧边栏展开 / 收成图标栏，以及几个全局快捷键。
   窗口够宽时记住用户的选择；窗口窄时默认收成图标栏，展开后浮在内容上面，点别处、换页或按 Esc 收回。 */

import { $, $maybe } from "./dom";
import { t } from "./i18n";

const KEY = "tingo.sidebar";
/** 窗口比这窄时侧边栏默认收成图标栏，把宽度让给内容 */
const NARROW_BELOW = 1024;

const isMac = navigator.platform.startsWith("Mac");
const modKey = isMac ? "⌘" : "Ctrl+";
const narrowQuery = window.matchMedia(`(max-width: ${NARROW_BELOW - 1}px)`);

/** 宽窗口下用户选了收起 */
let closedPref = false;
/** 窄窗口下临时展开（浮层） */
let drawer = false;

function readPref(): boolean {
  try {
    return localStorage.getItem(KEY) === "closed";
  } catch {
    return false; // 无痕模式等拿不到 localStorage
  }
}

function savePref(): void {
  try {
    localStorage.setItem(KEY, closedPref ? "closed" : "open");
  } catch {
    /* 同上 */
  }
}

const isCollapsed = (): boolean => (narrowQuery.matches ? !drawer : closedPref);

/** 按当前状态更新外框；界面语言变了也要再调一次（按钮提示跟着换） */
export function refreshShell(): void {
  const narrow = narrowQuery.matches;
  const collapsed = isCollapsed();
  const app = $("#app");
  app.classList.toggle("side-collapsed", collapsed);
  app.classList.toggle("side-narrow", narrow);
  app.classList.toggle("side-drawer", narrow && drawer);
  $("#sideScrim").hidden = !(narrow && drawer);
  const btn = $("#sideToggle");
  const label = collapsed ? t("展开侧边栏", "Expand sidebar") : t("收起侧边栏", "Collapse sidebar");
  btn.setAttribute("aria-expanded", String(!collapsed));
  btn.setAttribute("aria-label", label);
  btn.dataset.tip = `${label} ${modKey}B`;
  hideTip();
}

export function toggleSidebar(): void {
  if (narrowQuery.matches) drawer = !drawer;
  else {
    closedPref = !closedPref;
    savePref();
  }
  refreshShell();
}

function closeDrawer(): void {
  if (!drawer) return;
  drawer = false;
  refreshShell();
}

/** 跳到会议记录并把光标放进搜索框 */
function focusSearch(): void {
  const focus = () => {
    const input = $maybe<HTMLInputElement>("#mSearch");
    input?.focus();
    input?.select();
  };
  if ($maybe("#mSearch")) return focus();
  // 换页是 hashchange 触发的；这个监听排在路由后面，轮到它时搜索框已经画出来了
  window.addEventListener("hashchange", focus, { once: true });
  location.hash = "#/meetings";
}

/* ---------- 图标栏的文字提示 ---------- */

let tip: HTMLElement | null = null;

function tipText(el: HTMLElement): string {
  if (el.dataset.tip) return el.dataset.tip;
  // 展开时文字就在旁边，不用再提示
  return isCollapsed() ? (el.querySelector(".side-label")?.textContent ?? "") : "";
}

function showTip(el: HTMLElement): void {
  const text = tipText(el);
  if (!text) return hideTip();
  tip ??= document.body.appendChild(Object.assign(document.createElement("div"), { className: "tip" }));
  tip.setAttribute("role", "tooltip");
  tip.textContent = text;
  const r = el.getBoundingClientRect();
  const side = $("#sidebar").getBoundingClientRect();
  tip.style.left = `${Math.max(r.right, side.right) + 8}px`;
  tip.style.top = `${r.top + r.height / 2}px`;
  tip.classList.add("show");
}

function hideTip(): void {
  tip?.classList.remove("show");
}

const tipTarget = (e: Event): HTMLElement | null => (e.target as HTMLElement).closest?.<HTMLElement>(".side-item, [data-tip]") ?? null;

export function setupShell(): void {
  closedPref = readPref();
  refreshShell();
  // 第一次定好状态之前不做过渡，免得一打开就看到侧边栏滑一下
  requestAnimationFrame(() => requestAnimationFrame(() => $("#app").classList.remove("booting")));

  $("#sideToggle").onclick = toggleSidebar;
  $("#sideScrim").onclick = closeDrawer;
  narrowQuery.addEventListener("change", () => {
    drawer = false;
    refreshShell();
  });
  window.addEventListener("hashchange", closeDrawer);

  const sidebar = $("#sidebar");
  sidebar.addEventListener("mouseover", (e) => {
    const el = tipTarget(e);
    if (el) showTip(el);
    else hideTip();
  });
  sidebar.addEventListener("mouseleave", hideTip);
  sidebar.addEventListener("focusin", (e) => {
    const el = tipTarget(e);
    if (el?.matches(":focus-visible")) showTip(el);
  });
  sidebar.addEventListener("focusout", hideTip);

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") return closeDrawer();
    // macOS 上 Ctrl+B / Ctrl+K 是输入框里的光标移动和删除，只认 ⌘
    if (!(isMac ? e.metaKey : e.ctrlKey) || e.altKey || e.shiftKey || !$("#modal").hidden) return;
    const key = e.key.toLowerCase();
    if (key === "b") {
      e.preventDefault();
      toggleSidebar();
    } else if (key === "k") {
      e.preventDefault();
      focusSearch();
    }
  });
}

/** 搜索框里显示的快捷键 */
export const searchShortcut = (): string => `${modKey}K`;
