import { haptic, moving, spring } from "./motion";

const bindings = new WeakMap<HTMLElement, AbortController>();
/** 多选模式下不能左滑：去掉列表上的手势 */
export function unbindSwipeRows(box: HTMLElement): void {
  bindings.get(box)?.abort();
  bindings.delete(box);
}

/** 每次只展开一行；纵向滚动让给列表，滑动本身不触发打开或删除。按住约半秒不动触发 `onLongPress`。 */
export function bindSwipeRows(box: HTMLElement, onLongPress?: (row: HTMLElement) => void): void {
  bindings.get(box)?.abort();
  const controller = new AbortController(), { signal } = controller;
  bindings.set(box, controller);
  let opened: HTMLElement | undefined;
  let touch: { row: HTMLElement; x: number; y: number; offset: number; wasOpen: boolean; dx: number; lastX: number; lastTime: number; velocity: number; active: boolean } | undefined;
  let suppressUntil = 0;
  let hold: number | undefined;
  const cancelHold = () => { window.clearTimeout(hold); hold = undefined; };
  const width = (row: HTMLElement) => row.querySelector<HTMLElement>(".meeting-row-actions")!.offsetWidth;
  const offset = (row: HTMLElement, value: number) => {
    row.querySelector<HTMLElement>(".m-card")!.style.transform = `translateX(${value}px)`;
    // 卡片没滑开时把按钮藏起来，不然圆角抗锯齿会透出底下的红色
    if (value) row.classList.add("revealed");
    row.querySelector<HTMLElement>(".meeting-more")!.style.opacity = value ? "0" : "";
  };
  const freeze = (row: HTMLElement): number => {
    const card = row.querySelector<HTMLElement>(".m-card")!;
    const transform = getComputedStyle(card).transform;
    const x = transform === "none" ? 0 : new DOMMatrixReadOnly(transform).m41;
    card.getAnimations().forEach(a => a.cancel());
    offset(row, x);
    return x;
  };
  const settle = (row: HTMLElement, open: boolean) => {
    const card = row.querySelector<HTMLElement>(".m-card")!;
    const from = getComputedStyle(card).transform;
    card.getAnimations().forEach(a => a.cancel());
    offset(row, open ? -width(row) : 0);
    const hide = () => { if (!row.classList.contains("actions-open")) row.classList.remove("revealed"); };
    if (moving()) {
      const animation = card.animate([{ transform: from }, { transform: card.style.transform }], {duration: 220, easing: spring});
      if (!open) void animation.finished.then(hide).catch(() => {});
    } else if (!open) hide();
    row.classList.toggle("actions-open", open);
    const actions = row.querySelector<HTMLElement>(".meeting-row-actions")!;
    actions.inert = !open;
    actions.setAttribute("aria-hidden", String(!open));
    row.querySelector(".meeting-more")?.setAttribute("aria-expanded", String(open));
    if (open) { opened = row; haptic(); } else if (opened === row) opened = undefined;
  };
  const close = () => { if (opened) settle(opened, false); };
  box.addEventListener("contextmenu", e => { if ((e.target as Element).closest(".meeting-row")) e.preventDefault(); }, { signal });
  box.addEventListener("dragstart", e => e.preventDefault(), { signal });
  box.addEventListener("touchstart", e => {
    suppressUntil = 0;
    if (touch?.active) settle(touch.row, touch.wasOpen);
    touch = undefined;
    if (e.touches.length !== 1) return;
    const row = (e.target as Element).closest<HTMLElement>(".meeting-row");
    if (!row || (e.target as Element).closest("button")) return;
    if (opened && opened !== row) close();
    const p = e.touches[0];
    touch = { row, x: p.clientX, y: p.clientY, offset: freeze(row), wasOpen: opened === row, dx: 0, lastX: p.clientX, lastTime: performance.now(), velocity: 0, active: false };
    cancelHold();
    if (onLongPress && !touch.wasOpen) {
      hold = window.setTimeout(() => {
        if (!touch || touch.active || touch.row !== row) return;
        touch = undefined;
        suppressUntil = performance.now() + 600; // 松手时的点击不要再打开会议
        row.querySelector(".m-card")?.classList.remove("is-pressed");
        haptic();
        onLongPress(row);
      }, 450);
    }
  }, { passive: true, signal });
  box.addEventListener("touchmove", e => {
    if (!touch) return;
    if (e.touches.length !== 1) { settle(touch.row, touch.wasOpen); touch = undefined; return; }
    const dx = e.touches[0].clientX - touch.x, dy = e.touches[0].clientY - touch.y;
    if (Math.abs(dx) > 8 || Math.abs(dy) > 8) cancelHold();
    if (!touch.active) {
      if ((Math.abs(dy) > 8 && Math.abs(dy) >= Math.abs(dx)) || (!touch.offset && dx > 8)) { settle(touch.row, touch.wasOpen); touch = undefined; return; }
      if (Math.abs(dx) < 6 || Math.abs(dx) < Math.abs(dy) * 1.2) return;
      touch.active = true;
      touch.row.querySelector(".m-card")?.getAnimations().forEach(a => a.cancel());
      touch.row.querySelector(".m-card")?.classList.remove("is-pressed");
    }
    if (e.cancelable) e.preventDefault();
    const now = performance.now();
    touch.velocity = (e.touches[0].clientX - touch.lastX) / Math.max(1, now - touch.lastTime);
    touch.lastX = e.touches[0].clientX; touch.lastTime = now;
    touch.dx = dx;
    offset(touch.row, Math.max(-width(touch.row), Math.min(0, touch.offset + dx)));
  }, { passive: false, signal });
  box.addEventListener("touchend", () => {
    cancelHold();
    if (touch?.active) {
      suppressUntil = performance.now() + 400;
      const velocity = performance.now() - touch.lastTime < 100 ? touch.velocity : 0;
      const projected = touch.offset + touch.dx + velocity * 100;
      settle(touch.row, projected < -width(touch.row) * .5);
    } else if (touch) settle(touch.row, touch.wasOpen);
    touch = undefined;
  }, { passive: true, signal });
  box.addEventListener("touchcancel", () => {
    cancelHold(); if (touch) settle(touch.row, touch.wasOpen); touch = undefined; }, { signal });
  box.addEventListener("pointerdown", () => { suppressUntil = 0; }, { passive: true, signal });
  box.addEventListener("click", e => {
    if (performance.now() < suppressUntil) { e.preventDefault(); e.stopImmediatePropagation(); return; }
    const row = (e.target as Element).closest<HTMLElement>(".meeting-row");
    if (!row) return;
    if ((e.target as Element).closest(".meeting-more")) {
      e.preventDefault(); if (opened !== row) close(); settle(row, opened !== row);
    } else if (opened === row && !(e.target as Element).closest(".meeting-row-actions")) {
      e.preventDefault(); e.stopImmediatePropagation(); close();
    }
  }, { capture: true, signal });
  box.addEventListener("keydown", e => { if (e.key === "Escape") close(); }, { signal });
  document.addEventListener("pointerdown", e => { if (opened && !opened.contains(e.target as Node)) close(); }, { passive: true, signal });
  signal.addEventListener("abort", cancelHold);
  window.addEventListener("hashchange", () => { close(); controller.abort(); }, { once: true, signal });
}
