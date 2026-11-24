/* 手机交互：短转场、跟手按压和原生触感。桌面保持现有行为。 */
export const mobile = (): boolean => document.documentElement.classList.contains("ios");
export const reducedMotion = (): boolean => matchMedia("(prefers-reduced-motion: reduce)").matches;
export const moving = (): boolean => mobile() && !reducedMotion();
export const spring = "cubic-bezier(.2,.85,.25,1)";

export function haptic(style: "selection" | "record" | "success" = "selection"): void {
  if (!mobile() || !("__TAURI_INTERNALS__" in window)) return;
  void import("@tauri-apps/api/core").then(({ invoke }) => invoke("plugin:recorder|haptic", { style })).catch(() => {});
}

export function setupMotion(): void {
  let pressed: { el: HTMLElement; x: number; y: number } | undefined;
  const release = () => {
    pressed?.el.classList.remove("is-pressed");
    pressed = undefined;
  };
  document.addEventListener("pointerdown", (e) => {
    if (!mobile() || !e.isPrimary || e.button !== 0) return;
    release();
    const el = (e.target as Element).closest<HTMLElement>("button, .nav a, .m-card, .quick-item, .set-nav a, .settings-list a, .storage-row, .back, .fb-branch[role=button]");
    if (!el || el.matches(":disabled, [aria-disabled=true]")) return;
    pressed = { el, x: e.clientX, y: e.clientY };
    el.classList.add("is-pressed");
  }, { passive: true });
  document.addEventListener("pointermove", (e) => {
    if (pressed && Math.hypot(e.clientX - pressed.x, e.clientY - pressed.y) > 10) release();
  }, { passive: true });
  document.addEventListener("pointerup", release, { passive: true });
  document.addEventListener("pointercancel", release, { passive: true });
  window.addEventListener("blur", release);
  document.addEventListener("click", (e) => {
    const tab = (e.target as Element).closest<HTMLElement>(".nav a");
    if (!mobile() || !tab || tab.classList.contains("active")) return;
    haptic();
    if (moving()) tab.querySelector("svg")?.animate([
      { transform: "scale(.94)" }, { transform: "scale(1.025)", offset: .65 }, { transform: "scale(1)" },
    ], { duration: 220, easing: spring });
  });
}

/** 状态变化只动一次；电平轮询不重启动画。 */
export function animatePanel(el: HTMLElement, height: number): void {
  if (!moving() || !height) return;
  el.getAnimations().forEach((a) => a.cancel());
  const next = el.getBoundingClientRect().height;
  el.animate([{ height: `${height}px` }, { height: `${next}px` }], { duration: 260, easing: spring });
  const children = [...el.children];
  children.forEach((child, i) => child.animate([
    { opacity: 0, transform: "translateY(4px)" }, { opacity: 1, transform: "none" },
  ], { duration: 180, delay: Math.min(i, 3) * 12, easing: spring }));
}

/** 页面层由路由持有，手势只负责方向锁定与完成/取消。 */
export interface BackPreview {
  move(dx: number): void;
  finish(commit: boolean): void;
}
export function setupBackGesture(target: () => string | undefined, begin: (destination: string) => BackPreview | undefined): void {
  let gesture: { x: number; y: number; dx: number; lastX: number; lastTime: number; velocity: number; destination: string; preview?: BackPreview } | undefined;
  let blockClickUntil = 0;
  const cancel = () => { gesture?.preview?.finish(false); gesture = undefined; };
  document.addEventListener("touchstart", e => {
    blockClickUntil = 0; // 新的一次触摸不是上一段滑动产生的合成 click
    cancel();
    if (!mobile() || e.touches.length !== 1 || !document.querySelector<HTMLElement>("#modal")?.hidden) return;
    const touch = e.touches[0], destination = target();
    if (!destination || touch.clientX > 28 || (e.target as Element).closest("input, textarea, select, [contenteditable=true]")) return;
    gesture = { x: touch.clientX, y: touch.clientY, dx: 0, lastX: touch.clientX, lastTime: performance.now(), velocity: 0, destination };
  }, { passive: true });
  document.addEventListener("touchmove", e => {
    if (!gesture) return;
    if (e.touches.length !== 1) { cancel(); return; }
    const x = e.touches[0].clientX, dx = x - gesture.x, dy = e.touches[0].clientY - gesture.y;
    if (!gesture.preview) {
      if (Math.abs(dy) > 12 || dx < -8) { cancel(); return; }
      if (dx < 10 || dx < Math.abs(dy) * 1.5) return;
      gesture.preview = begin(gesture.destination);
      if (!gesture.preview) { gesture = undefined; return; }
    }
    if (e.cancelable) e.preventDefault();
    const now = performance.now();
    gesture.velocity = (x - gesture.lastX) / Math.max(1, now - gesture.lastTime);
    gesture.lastX = x; gesture.lastTime = now;
    gesture.dx = Math.max(0, Math.min(dx, innerWidth));
    gesture.preview.move(gesture.dx);
  }, { passive: false });
  document.addEventListener("touchend", () => {
    const g = gesture; gesture = undefined;
    if (!g?.preview) return;
    blockClickUntil = performance.now() + 400;
    const fast = performance.now() - g.lastTime < 100 && g.velocity > .5 && g.dx > 35;
    const commit = g.dx > innerWidth * .33 || fast;
    if (commit) haptic();
    g.preview.finish(commit);
  }, { passive: true });
  document.addEventListener("pointerdown", () => { blockClickUntil = 0; }, { passive: true });
  document.addEventListener("click", e => {
    if (performance.now() < blockClickUntil) { e.preventDefault(); e.stopImmediatePropagation(); }
  }, true);
  document.addEventListener("touchcancel", cancel, { passive: true });
  window.addEventListener("hashchange", cancel);
  window.addEventListener("blur", cancel);
}
