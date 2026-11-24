import { $$, main } from "./dom";
import { S } from "./state";
import { renderDetail, stopAudio } from "./views/detail";
import { renderHome } from "./views/home";
import { renderMeetings, meetingsPreview } from "./views/meetings";
import { renderSettings, settingsPreview } from "./views/settings";
import { leaveRecording } from "./views/recording";
import { renderWelcome } from "./views/welcome";
import { mobile, moving, spring } from "./motion";

let current = "";
let sequence = 0;
const positions = new Map<string, number>();
const snapshots = new Map<string, HTMLElement>();
let cleanupTransition: (() => void) | undefined;
let pendingBack: { destination: string; layer: HTMLElement; cleanup: () => void } | undefined;

function snapshot(el: HTMLElement): HTMLElement {
  const copy = el.cloneNode(true) as HTMLElement;
  const originals = [el, ...el.querySelectorAll<HTMLElement>("*")];
  const copies = [copy, ...copy.querySelectorAll<HTMLElement>("*")];
  copies.forEach((node, i) => {
    node.removeAttribute("id");
    node.scrollTop = originals[i].scrollTop;
    if (node instanceof HTMLInputElement && originals[i] instanceof HTMLInputElement) node.value = (originals[i] as HTMLInputElement).value;
  });
  copy.inert = true;
  copy.setAttribute("aria-hidden", "true");
  return copy;
}
function placeLayer(copy: HTMLElement): HTMLElement {
  const rect = main().getBoundingClientRect();
  copy.classList.add("navigation-layer");
  Object.assign(copy.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` });
  document.body.append(copy);
  return copy;
}
function cloneLayer(el: HTMLElement, top = el.scrollTop): HTMLElement {
  const layer = placeLayer(snapshot(el));
  layer.scrollTop = top;
  return layer;
}
const parentRoute = (h: string): string | undefined => {
  if (h.startsWith("/settings/")) return "/settings";
  if (h.startsWith("/m/")) return "/meetings";
  return undefined;
};

export function beginBackPreview(destination: string): import("./motion").BackPreview | undefined {
  if (cleanupTransition || pendingBack) return;
  const source = snapshots.get(destination);
  const backdrop = source ? cloneLayer(source, positions.get(destination) ?? 0) : placeLayer(document.createElement("main"));
  if (!source) {
    backdrop.classList.add("main");
    backdrop.innerHTML = destination === "/meetings" ? meetingsPreview() : settingsPreview();
    backdrop.inert = true;
  }
  backdrop.style.zIndex = "20";
  const el = main(), width = el.clientWidth;
  el.classList.add("navigation-front");
  let dx = 0, finished = false;
  const clean = () => {
    backdrop.remove(); el.classList.remove("navigation-front");
    el.style.transform = ""; el.style.transition = "";
    cleanupTransition = undefined;
  };
  cleanupTransition = clean;
  const move = (x: number) => {
    dx = Math.min(width, x);
    el.style.transform = `translateX(${dx}px)`;
    backdrop.style.transform = `translateX(${(dx - width) * .25}px)`;

  };
  move(0);
  return { move, finish(commit) {
    if (finished) return;
    finished = true;
    const end = commit ? width : 0;
    const duration = moving() ? Math.max(120, Math.min(240, Math.abs(end - dx) / width * 280)) : 0;
    const a = el.animate([{ transform: `translateX(${dx}px)` }, { transform: `translateX(${end}px)` }], { duration, easing: spring, fill: "forwards" });
    const b = backdrop.animate([{ transform: backdrop.style.transform }, { transform: `translateX(${(end - width) * .25}px)` }], { duration, easing: spring, fill: "forwards" });
    cleanupTransition = () => { a.cancel(); b.cancel(); clean(); };
    void a.finished.then(() => {
      if (!commit) { cleanupTransition?.(); return; }
      // 完成滑动后仍保留上一级快照，等真实页面绘制完成再揭开，避免空白闪烁。
      el.style.transform = `translateX(${width}px)`;
      backdrop.style.transform = "none"; backdrop.style.filter = "";
      a.cancel(); b.cancel();
      pendingBack = { destination, layer: backdrop, cleanup: clean };
      location.hash = destination;
    }).catch(() => {});
  } };
}

export function route(): void {
  const h = decodeURIComponent(location.hash.slice(1) || "/");
  const seq = ++sequence;
  const committed = pendingBack?.destination === h ? pendingBack : undefined;
  if (!committed) cleanupTransition?.();
  pendingBack = undefined;
  if (current && mobile()) {
    positions.set(current, main().scrollTop);
    if (["/meetings", "/settings"].includes(current)) snapshots.set(current, snapshot(main()));
  }
  const direction = parentRoute(current) === h ? "pop" : parentRoute(h) && h !== current ? "push" : "tab";
  const old = !committed && moving() && current && direction !== "tab" ? cloneLayer(main()) : undefined;
  current = h;
  if (committed) {
    main().style.transform = "";
    main().classList.remove("navigation-front");
  }
  const drawn = renderRoute(h);
  const finish = () => {
    if (seq !== sequence) { old?.remove(); committed?.layer.remove(); return; }
    main().scrollTo({ top: mobile() ? positions.get(h) ?? 0 : 0, behavior: "instant" });
    if (committed) { committed.cleanup(); return; }
    if (!old) return;
    const width = main().clientWidth;
    const push = direction === "push";
    old.style.zIndex = push ? "20" : "31";
    main().classList.add("navigation-front");
    const a = main().animate([{ transform: `translateX(${push ? width : -width * .25}px)` }, { transform: "none" }], { duration: 280, easing: spring });
    const b = old.animate([{ transform: "none" }, { transform: `translateX(${push ? -width * .25 : width}px)` }], { duration: 280, easing: spring });
    const clean = () => { a.cancel(); b.cancel(); old.remove(); main().classList.remove("navigation-front"); cleanupTransition = undefined; };
    cleanupTransition = clean;
    void Promise.all([a.finished, b.finished]).then(clean).catch(() => {});
  };
  // 异步内容加载时保留旧页；用户另行导航可立即清理。
  if (old) cleanupTransition = () => { old.remove(); cleanupTransition = undefined; };
  void Promise.resolve(drawn).then(finish).catch(() => { old?.remove(); committed?.cleanup(); });
}

function renderRoute(h: string): void | Promise<void> {
  let ready: void | Promise<void> = undefined;
  stopAudio();
  leaveRecording();
  leaveRecording();
  S.detail = null;
  let nav = "home";
  if (h.startsWith("/m/")) {
    nav = "meetings";
    const [id, query] = h.slice(3).split("?");
    const at = Number(new URLSearchParams(query ?? "").get("t"));
    ready = renderDetail(id, false, Number.isFinite(at) && query ? at : undefined);
  } else if (h === "/meetings") {
    nav = "meetings";
    renderMeetings();
  } else if (h.startsWith("/welcome")) {
    renderWelcome(h.split("/")[2]);
  } else if (h === "/settings" || h.startsWith("/settings/")) {
    nav = "settings";
    ready = renderSettings(h.split("/")[2]);
  } else {
    renderHome();
  }
  $$<HTMLAnchorElement>("#sidebar [data-nav]").forEach((a) => a.classList.toggle("active", a.dataset.nav === nav));
  $$<HTMLAnchorElement>("#recentList a").forEach((a) => a.classList.toggle("active", h.split("?")[0] === `/m/${a.dataset.id}`));
  return ready;
}

/** 主标签页不参与返回栈；只有子页可以回到所属列表。 */
export function swipeBackTarget(): string | undefined {
  const path = location.hash.slice(1).split("?")[0];
  return parentRoute(path);
}
