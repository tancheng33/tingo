/* 任务列表：侧边栏"进行中"和首页玻璃面板下方都用它。录音之外的任务都在后台跑，这里看进度、点进去看过程。 */

import { t } from "./i18n";
import { api, type Job } from "./api";
import { $maybe, esc, fmt } from "./dom";
import { isBusy, jobDetail, jobTitle, progressOf, S } from "./state";

const linkOf = (j: Job) =>
  j.state === "recording"
    ? "#/"
    : j.kind === "models"
      ? "#/settings/asr"
      : j.kind === "update"
        ? "#/settings/about"
        : `#/m/${encodeURIComponent(j.meeting ?? "")}`;

const statusIcon = (j: Job): string =>
  j.state === "recording"
    ? `<span class="live-dot"></span>`
    : isBusy(j)
      ? `<span class="task-spin"></span>`
      : j.state === "done"
        ? `<span class="task-ok">✓</span>`
        : `<span class="task-err">!</span>`;

function item(j: Job, big: boolean): string {
  const busy = isBusy(j);
  const rec = j.state === "recording";
  const pct = Math.round(progressOf(j) * 100);
  const detail = rec ? `${t("录音中", "Recording")} ${fmt((j.elapsed ?? 0) * 1000)}` : jobDetail(j);
  return `
    <a class="task ${j.state} ${big ? "big" : ""}" href="${linkOf(j)}" data-id="${esc(j.id ?? "")}">
      <span class="task-ico">${statusIcon(j)}</span>
      <span class="task-main">
        <span class="task-title">${esc(jobTitle(j))}</span>
        <span class="task-detail">${esc(detail)}</span>
        ${busy && !rec ? `<span class="task-bar"><i style="width:${Math.max(3, pct)}%"></i></span>` : ""}
      </span>
      ${busy && !rec && big ? `<span class="task-pct">${pct}%</span>` : ""}
      ${!busy && j.id ? `<button class="task-x" data-dismiss="${esc(j.id)}" title="${t("收起", "Dismiss")}">×</button>` : ""}
    </a>`;
}

export async function dismissJob(id: string): Promise<void> {
  S.jobs = S.jobs.filter((j) => j.id !== id);
  renderTasks();
  await api(`/api/jobs/${encodeURIComponent(id)}/dismiss`, { method: "POST" }).catch(() => {});
}

function bind(box: HTMLElement): void {
  box.querySelectorAll<HTMLButtonElement>("[data-dismiss]").forEach(
    (b) =>
      (b.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        dismissJob(b.dataset.dismiss ?? "");
      }),
  );
}

/** 侧边栏：所有任务的紧凑列表；首页：后台任务的大卡片（录音本身在玻璃面板里，不重复显示） */
export function renderTasks(): void {
  const recording = S.job.state === "recording";
  const homeLabel = $maybe('#sidebar [data-nav="home"] [data-t]');
  if (homeLabel) {
    const [zh, en] = recording ? ["返回录音", "Back to recording"] : ["新会议", "New meeting"];
    const label = t(zh, en);
    if (homeLabel.textContent !== label) {
      homeLabel.dataset.t = `${zh}|${en}`;
      homeLabel.textContent = label;
    }
    homeLabel.parentElement?.classList.toggle("recording", recording);
  }
  // 侧边栏收成图标栏时：后台任务并成一个图标（录音本身显示在「新会议」按钮上）
  const rail = $maybe<HTMLAnchorElement>("#railTasks");
  if (rail) {
    const list = S.jobs.filter((j) => j.state !== "idle" && j.state !== "recording");
    const top = list.find(isBusy) ?? list.find((j) => j.state === "error") ?? list[0];
    rail.hidden = !top;
    if (top) {
      const html = `<span class="side-ico">${statusIcon(top)}</span><span class="side-label">${esc(jobTitle(top))}</span>${list.length > 1 ? `<b class="rail-count">${list.length}</b>` : ""}`;
      if (rail.dataset.html !== html) {
        rail.dataset.html = html;
        rail.innerHTML = html;
      }
      rail.href = linkOf(top);
      rail.dataset.tip = `${jobTitle(top)} · ${jobDetail(top)}${list.length > 1 ? t(`（共 ${list.length} 个任务）`, ` (${list.length} tasks)`) : ""}`;
    }
  }
  const side = $maybe("#sideTasks");
  if (side) {
    const list = S.jobs.filter((j) => j.state !== "idle");
    side.hidden = !list.length;
    const html = list.length ? `<div class="recent-head">${t("进行中", "In progress")}</div>${list.map((j) => item(j, false)).join("")}` : "";
    if (side.dataset.html !== html) {
      side.dataset.html = html;
      side.innerHTML = html;
      bind(side);
    }
  }
  const home = $maybe("#homeTasks");
  if (home) {
    const list = S.jobs.filter((j) => j.state !== "recording" && j.state !== "idle" && !(j.kind === "models" && S.job === j));
    const html = list.length ? list.map((j) => item(j, true)).join("") : "";
    if (home.dataset.html !== html) {
      home.dataset.html = html;
      home.innerHTML = html;
      bind(home);
    }
  }
}
