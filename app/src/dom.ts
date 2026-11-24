/* 通用小工具：取元素、转义、时间格式、图标、提示和弹窗。 */
import { moving, spring } from "./motion";

export function $<T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document): T {
  const el = root.querySelector<T>(selector);
  if (!el) throw new Error(`missing element ${selector}`);
  return el;
}

export function $maybe<T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document): T | null {
  return root.querySelector<T>(selector);
}

export function $$<T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document): T[] {
  return [...root.querySelectorAll<T>(selector)];
}

export const main = (): HTMLElement => $("#main");

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
export const esc = (s: unknown): string => String(s ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]);

export function fmt(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

export function tsToMs(t: string): number {
  const p = t.split(":").map(Number);
  return (p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1]) * 1000;
}

const COLORS = ["#1F4BFF", "#12B3A8", "#F79009", "#E04F9C", "#7A5AF8", "#0BA5EC", "#66C61C", "#EF6820"];
export const color = (i: number): string => COLORS[i % COLORS.length];

/** 头像上的字："说话人3" 显示 3，真名显示第一个字。 */
export function initial(name: string): string {
  const m = /^说话人(\d+)$/.exec(name);
  return m ? m[1] : [...name][0] || "?";
}

export const ICON = {
  watch: '<svg viewBox="0 0 24 24"><rect x="6" y="6" width="12" height="12" rx="4"/><path d="m8 6 1-4h6l1 4m-8 12 1 4h6l1-4M12 9v3l2 1m5-3v3"/></svg>',
  cloud: '<svg viewBox="0 0 24 24"><path d="M7 18.5H6a4.5 4.5 0 0 1-.6-9 6.8 6.8 0 0 1 13-1.3 5.2 5.2 0 0 1-.4 10.3h-1M12 13v8m-3-3 3 3 3-3"/></svg>',
  trash: '<svg viewBox="0 0 24 24"><path d="M4.5 7h15M9 7V4.5h6V7M6.5 7l.8 12.5h9.4L17.5 7M10 10.5v5.5m4-5.5v5.5"/></svg>',
  restore: '<svg viewBox="0 0 24 24"><path d="M5 9a7.5 7.5 0 1 1-.4 7M5 4.5V9h4.5M12 8v4.5l3 1.5"/></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="m5 12 4.5 4.5L19 7"/></svg>',

  chevron: '<svg viewBox="0 0 24 24"><path d="m9 6 6 6-6 6"/></svg>',
  back: '<svg viewBox="0 0 24 24"><path d="m15 6-6 6 6 6"/></svg>',
  monitor: '<svg viewBox="0 0 24 24"><rect x="3" y="4.5" width="18" height="12" rx="2"/><path d="M8.5 20h7M12 16.5V20"/></svg>',
  mic: '<svg viewBox="0 0 24 24"><rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21"/></svg>',
  upload: '<svg viewBox="0 0 24 24"><path d="M12 15V4M7.5 8.5 12 4l4.5 4.5M5 14v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4"/></svg>',
  globe: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.3 2.4 3.5 5.2 3.5 8.5s-1.2 6.1-3.5 8.5c-2.3-2.4-3.5-5.2-3.5-8.5S9.7 5.9 12 3.5z"/></svg>',
  download: '<svg viewBox="0 0 24 24"><path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 14v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4"/></svg>',
  file: '<svg viewBox="0 0 24 24"><path d="M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z"/><path d="M14 3.5v5h5M9 13h6M9 16.5h4"/></svg>',
  spark:
    '<svg viewBox="0 0 24 24"><path d="M12 3.5c.6 3.8 2.7 5.9 6.5 6.5-3.8.6-5.9 2.7-6.5 6.5-.6-3.8-2.7-5.9-6.5-6.5 3.8-.6 5.9-2.7 6.5-6.5z"/><path d="M18.5 16.5c.25 1.4 1 2.15 2.5 2.5-1.5.35-2.25 1.1-2.5 2.5-.25-1.4-1-2.15-2.5-2.5 1.5-.35 2.25-1.1 2.5-2.5z"/></svg>',
  send: '<svg viewBox="0 0 24 24"><path d="M20.5 3.5 10 14M20.5 3.5 14 20.5l-4-6.5-6.5-4z"/></svg>',
  copy: '<svg viewBox="0 0 24 24"><rect x="8" y="8" width="12" height="12" rx="2.5"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>',
  folder: '<svg viewBox="0 0 24 24"><path d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/></svg>',
  redo: '<svg viewBox="0 0 24 24"><path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4.5v4h-4"/></svg>',
  edit: '<svg viewBox="0 0 24 24"><path d="M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16z"/></svg>',
  search: '<svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/></svg>',
  play: '<svg viewBox="0 0 24 24"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.5-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z"/></svg>',
  pause:
    '<svg viewBox="0 0 24 24"><rect x="6.5" y="5" width="4" height="14" rx="1.2"/><rect x="13.5" y="5" width="4" height="14" rx="1.2"/></svg>',
  people:
    '<svg viewBox="0 0 24 24"><circle cx="9" cy="8.5" r="3.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0M16 5.6a3.2 3.2 0 0 1 0 5.8M18 13.8c1.6.8 2.6 2.6 2.6 5.2"/></svg>',
  vault:
    '<svg viewBox="0 0 24 24"><path d="M9.5 3 17 6.5l2 8.5-6 6.5-6.5-4L5 9.5z"/><path d="M9.5 3 12 13.5l7 1.5M12 13.5l-5.5 4"/></svg>',
  draw: '<svg viewBox="0 0 24 24"><path d="M3.5 19.5c2.5-.5 3.5-3 6-3s3 2.5 5.5 2"/><path d="M14 5.5 18.5 10M16 3.5a2.1 2.1 0 0 1 3 3L10 15.5H7v-3z"/></svg>',
  bot: '<svg viewBox="0 0 24 24"><path d="M6 4.5h12A2.5 2.5 0 0 1 20.5 7v7.5A2.5 2.5 0 0 1 18 17h-6.5l-4.5 3.5V17H6a2.5 2.5 0 0 1-2.5-2.5V7A2.5 2.5 0 0 1 6 4.5z"/><path d="M12 7.6c.35 1.65 1.25 2.55 2.9 2.9-1.65.35-2.55 1.25-2.9 2.9-.35-1.65-1.25-2.55-2.9-2.9 1.65-.35 2.55-1.25 2.9-2.9z"/></svg>',
} as const;

let toastTimer: number | undefined;
export function toast(msg: string, error = false): void {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.toggle("error", error);
  t.classList.add("show");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => t.classList.remove("show"), error ? 4200 : 2400);
}

export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  if (e && typeof e === "object") {
    const value = e as Record<string, unknown>;
    for (const key of ["message", "error", "detail", "description"]) {
      if (typeof value[key] === "string" && value[key]) return value[key] as string;
    }
    if (value.error && typeof value.error === "object" && value.error !== e) {
      const nested = value.error as Record<string, unknown>;
      if (typeof nested.message === "string") return nested.message;
    }
  }
  return document.documentElement.lang.startsWith("en") ? "Something went wrong. Please try again." : "操作失败，请重试";
}

let modalVersion = 0;
let modalFocus: HTMLElement | null = null;
export function openModal(html: string): void {
  ++modalVersion;
  const modal = $("#modal");
  const card = $(".modal-card", modal);
  const body = $("#modalBody");
  modalFocus = document.activeElement as HTMLElement | null;
  [modal, card].forEach((el) => el.getAnimations().forEach((a) => a.cancel()));
  card.style.transform = "";
  card.querySelector(":scope > .modal-foot")?.remove();
  body.innerHTML = html;
  // Keep actions outside the scrolling content, including when AI adds long evidence.
  const footer = body.querySelector(".modal-foot");
  if (footer) card.append(footer);
  body.scrollTop = 0;
  card.classList.toggle("structured-dialog", !!$maybe(".dialog-layout", card));
  modal.hidden = false;
  if (moving()) {
    modal.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 180 });
    // 手机上弹窗贴底，从下往上滑出来
    if (phone()) card.animate([{ transform: "translateY(100%)" }, { transform: "none" }], { duration: 300, easing: spring });
    else card.animate([{ transform: "translateY(12px) scale(.985)" }, { transform: "none" }], { duration: 220, easing: spring });
  }
  $$("[data-close]", $("#modal")).forEach((b) => (b.onclick = closeModal));
  const heading = $maybe<HTMLElement>(".dialog-header h3", card);
  if (heading) heading.tabIndex = -1;
  // 结构化弹窗先聚焦标题，避免先拉起键盘再失焦，让操作按钮在首次触摸时移位。
  (heading ?? $maybe<HTMLInputElement>("#modalBody input") ?? $maybe<HTMLButtonElement>("#modalBody button"))?.focus({ preventScroll: true });
}

/** 共用弹窗框架：标题、可滚动正文和固定操作区。正文/按钮传入已转义的模板。 */
export function openDialog(options: { title: string; description?: string; body: string; actions: string }): void {
  openModal(`<div class="dialog-layout"><header class="dialog-header"><h3>${esc(options.title)}</h3>${options.description ? `<p>${esc(options.description)}</p>` : ""}</header><div class="dialog-content">${options.body}</div><footer class="dialog-actions">${options.actions}</footer></div>`);
}

export function closeModal(): void {
  const modal = $("#modal");
  if (modal.hidden) return;
  const version = ++modalVersion;
  const card = $(".modal-card", modal);
  const finish = () => {
    if (version !== modalVersion) return;
    modal.hidden = true;
    card.style.transform = "";
    modalFocus?.isConnected && modalFocus.focus({ preventScroll: true });
  };
  if (!moving()) return finish();
  const from = getComputedStyle(card).transform;
  [modal, card].forEach((el) => el.getAnimations().forEach((a) => a.cancel()));
  modal.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 200, fill: "forwards" });
  const animation = card.animate([{ transform: from }, { transform: phone() ? "translateY(100%)" : "translateY(8px) scale(.99)" }], { duration: phone() ? 220 : 160, easing: "ease-in" });
  void animation.finished.then(finish).catch(() => {});
}

/** 窄屏：弹窗、操作菜单贴底显示 */
const phone = (): boolean => matchMedia("(max-width: 720px)").matches;

/* 键盘弹出时可见区域变矮，贴底的弹窗跟着往上抬，输入框和按钮不被键盘挡住 */
function trackKeyboard(): void {
  const vv = window.visualViewport;
  if (!vv) return;
  const update = () => {
    const covered = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
    document.documentElement.style.setProperty("--keyboard", `${Math.round(covered)}px`);
  };
  vv.addEventListener("resize", update);
  vv.addEventListener("scroll", update);
  update();
}

export function setupModal(): void {
  trackKeyboard();
  // 换页（包括手势返回）时收起弹窗和操作菜单，不留在新页面上
  window.addEventListener("hashchange", () => {
    closeModal();
    document.querySelector(".sheet")?.remove();
  });
  const card = $(".modal-card");
  $("#modal").addEventListener("pointerdown", (e) => {
    if ((e.target as HTMLElement).id === "modal") closeModal();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeModal();
    if (e.key === "Tab" && !$("#modal").hidden) {
      const nodes = $$<HTMLElement>("button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href]", card).filter((el) => el.getClientRects().length);
      const first = nodes[0], last = nodes.at(-1);
      if (first && (e.shiftKey && document.activeElement === first || !e.shiftKey && document.activeElement === last)) {
        e.preventDefault();
        (e.shiftKey ? last : first)?.focus();
      }
    }
  });
}

/** 底部操作菜单（手机）：一组组按钮 + 取消，点外面、按 Esc、选了某项都会收起 */
export interface SheetItem {
  label: string;
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  run: () => void;
}

export function openSheet(title: string, groups: SheetItem[][]): void {
  document.querySelector(".sheet")?.remove();
  const sheet = document.createElement("div");
  sheet.className = "sheet";
  const items = groups.map((g) => g.filter(Boolean)).filter((g) => g.length);
  const cancel = document.documentElement.lang.startsWith("en") ? "Cancel" : "取消";
  sheet.innerHTML = `<div class="sheet-card" role="dialog" aria-modal="true" aria-label="${esc(title)}">
    <div class="sheet-title">${esc(title)}</div>
    ${items
      .map(
        (g, gi) =>
          `<div class="sheet-group">${g
            .map((it, i) => `<button class="sheet-item ${it.danger ? "danger" : ""}" data-g="${gi}" data-i="${i}" ${it.disabled ? "disabled" : ""}>${it.icon ?? ""}<span>${esc(it.label)}</span></button>`)
            .join("")}</div>`,
      )
      .join("")}
    <button class="sheet-item sheet-cancel">${cancel}</button>
  </div>`;
  document.body.append(sheet);
  const card = $(".sheet-card", sheet);
  const focus = document.activeElement as HTMLElement | null;
  let closed = false;
  const close = (after?: () => void) => {
    if (closed) return;
    closed = true;
    document.removeEventListener("keydown", onKey);
    const finish = () => {
      sheet.remove();
      if (focus?.isConnected) focus.focus({ preventScroll: true });
      after?.();
    };
    if (!moving()) return finish();
    sheet.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 180, fill: "forwards" });
    void card.animate([{ transform: "none" }, { transform: "translateY(100%)" }], { duration: 200, easing: "ease-in", fill: "forwards" }).finished.then(finish).catch(finish);
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("keydown", onKey);
  sheet.addEventListener("pointerdown", (e) => {
    if (e.target === sheet) close();
  });
  $(".sheet-cancel", sheet).onclick = () => close();
  $$<HTMLButtonElement>(".sheet-item[data-g]", sheet).forEach(
    (b) => (b.onclick = () => close(items[Number(b.dataset.g)][Number(b.dataset.i)].run)),
  );
  if (moving()) {
    sheet.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 180 });
    card.animate([{ transform: "translateY(100%)" }, { transform: "none" }], { duration: 280, easing: spring });
  }
  $<HTMLButtonElement>(".sheet-item:not(:disabled)", sheet).focus({ preventScroll: true });
}
