/* 检查更新（桌面版）：GitHub 上有新版本时侧栏版本号旁边亮起提示，点开看更新说明，
   桌面版下载签名更新包，再由用户选择重启安装（Windows 上由安装程序替换后自动重开）；浏览器入口保留安装包下载。 */

import { api, type Job } from "./api";
import { closeModal, errorMessage, esc, openModal, toast } from "./dom";
import { t } from "./i18n";
import { renderMarkdown } from "./markdown";
import { isWindows } from "./platform";
import { S } from "./state";

export interface UpdateInfo {
  current: string;
  latest?: string;
  newer: boolean;
  url?: string;
  notes?: string;
  dmg?: string;
  size_mb?: number;
  sha256?: string | null;
  error?: string;
  native?: boolean;
  phase?: "idle" | "available" | "downloading" | "verifying" | "ready" | "installing";
  downloaded?: number;
  total?: number | null;
}

let info: UpdateInfo | null = null;
let checked = 0;

/** 6 小时最多查一次（后端也有缓存） */
export async function checkUpdate(force = false): Promise<UpdateInfo | null> {
  if (!force && info && Date.now() - checked < 6 * 3600_000) return info;
  try {
    if ("__TAURI_INTERNALS__" in window) {
      const { invoke } = await import("@tauri-apps/api/core");
      try { info = await invoke<UpdateInfo>("native_update_check"); }
      catch (e) { info = { current: S.env?.version ?? "", newer: false, native: true, error: errorMessage(e) }; }
    } else {
      info = await api<UpdateInfo>(force ? "/api/update?force=true" : "/api/update");
    }
    checked = Date.now();
  } catch {
    info = null; // 旧版后端或 Python 版没有这个接口
  }
  return info;
}

export const updateInfo = (): UpdateInfo | null => info;

export function showUpdate(): void {
  const u = info;
  if (!u?.newer) return;
  if (u.native) { showNativeUpdate(u); return; }
  openModal(`
    <h3>${t(`Tingo ${u.latest} 可以更新了`, `Tingo ${u.latest} is available`)}</h3>
    <p class="hint">${t(`当前版本 ${u.current}`, `You have ${u.current}`)}${u.size_mb ? ` · ${u.size_mb} MB` : ""}</p>
    <div class="md update-notes">${renderMarkdown(u.notes ?? "")}</div>
    <p class="hint">${isWindows() ? t(
      `会下载到「下载」文件夹${u.sha256 ? "，并核对 SHA-256" : ""}，然后打开安装程序，按提示完成更新。配置、会议和识别模型都会保留。`,
      `It downloads to your Downloads folder${u.sha256 ? ", checks the SHA-256" : ""} and opens the installer — follow it to finish updating. Settings, meetings and models are kept.`,
    ) : t(
      `会下载到「下载」文件夹${u.sha256 ? "，并核对 SHA-256" : ""}，然后打开安装包，把 Tingo 拖进「应用程序」替换旧版即可。配置、会议和识别模型都会保留。`,
      `It downloads to your Downloads folder${u.sha256 ? ", checks the SHA-256" : ""} and opens the installer — drag Tingo into Applications to replace the old version. Settings, meetings and models are kept.`,
    )}</p>
    <div class="modal-foot"><a class="btn" href="${esc(u.url ?? "#")}" target="_blank" rel="noopener">${t("在 GitHub 查看", "View on GitHub")}</a><span class="grow"></span>
      <button class="btn" data-close>${t("以后再说", "Later")}</button><button class="btn primary" id="upGo">${t("下载并打开", "Download & open")}</button></div>`);
  const go = document.getElementById("upGo") as HTMLButtonElement;
  go.onclick = async () => {
    go.disabled = true;
    try {
      const job = await api<Job>("/api/update/download", { method: "POST" });
      S.jobs = [...S.jobs.filter((j) => j.id !== job.id), job];
      closeModal();
      toast(t("正在下载新版，进度在左侧「进行中」", "Downloading the update — see In progress on the left"));
    } catch (e) {
      toast(errorMessage(e), true);
      go.disabled = false;
    }
  };
}


function nativeProgress(): void {
  const u = info;
  const button = document.getElementById("nativeUpdateGo") as HTMLButtonElement | null;
  const label = document.getElementById("nativeUpdateStatus");
  const progress = document.getElementById("nativeUpdateProgress") as HTMLProgressElement | null;
  if (!u || !button || !label || !progress) return;
  const busy = ["downloading", "verifying", "installing"].includes(u.phase ?? "");
  button.disabled = busy;
  button.textContent = u.phase === "ready" ? t("重启并更新", "Restart and update") : busy ? t("处理中…", "Working…") : t("下载更新", "Download update");
  const mb = (n: number) => (n / 1e6).toFixed(1);
  const percent = u.total ? Math.min(100, Math.round((u.downloaded ?? 0) / u.total * 100)) : null;
  const states: Record<string, string> = {
    downloading: t(`正在下载 ${mb(u.downloaded ?? 0)}${u.total ? ` / ${mb(u.total)}` : ""} MB${percent !== null ? ` · ${percent}%` : ""}`, `Downloading ${mb(u.downloaded ?? 0)}${u.total ? ` / ${mb(u.total)}` : ""} MB${percent !== null ? ` · ${percent}%` : ""}`),
    verifying: t("正在验证更新包签名…", "Verifying update signature…"),
    ready: t("下载完成，签名校验通过，可以重启更新", "Download verified. Ready to restart and update."),
    installing: t("正在安装，即将重新打开 Tingo…", "Installing. Tingo will reopen shortly…"),
  };
  label.textContent = u.error || states[u.phase ?? ""] || t("下载完成后，由你决定何时重启更新", "Choose when to restart after the download completes.");
  progress.hidden = !busy;
  if (u.phase === "downloading" && percent !== null) progress.value = percent;
  else progress.removeAttribute("value");
}

function showNativeUpdate(u: UpdateInfo): void {
  openModal(`
    <h3>${t(`Tingo ${esc(u.latest ?? "")} 可以更新了`, `Tingo ${esc(u.latest ?? "")} is available`)}</h3>
    <p class="hint">${t(`当前版本 ${esc(u.current)}`, `You have ${esc(u.current)}`)}</p>
    <div class="md update-notes">${renderMarkdown(u.notes ?? "")}</div>
    <p class="hint">${t("下载并校验后，点击「重启并更新」自动安装。会议、配置和识别模型都会保留。请先完成录音和正在处理的任务。", "After verification, click Restart and update to install automatically. Meetings, settings and speech models are kept. Finish recording and other active tasks first.")}</p>
    <p id="nativeUpdateStatus" class="hint" role="status" aria-live="polite"></p>
    <progress id="nativeUpdateProgress" max="100" style="width:100%" hidden></progress>
    <div class="modal-foot"><a class="btn" href="${esc(u.url ?? "#")}" target="_blank" rel="noopener">${t("在 GitHub 查看", "View on GitHub")}</a><span class="grow"></span><button class="btn" data-close>${t("稍后", "Later")}</button><button class="btn primary" id="nativeUpdateGo"></button></div>`);
  nativeProgress();
  document.getElementById("nativeUpdateGo")!.onclick = async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const installing = info?.phase === "ready";
    if (!info) return;
    if (installing && S.jobs.some((j) => !["done", "error", "idle"].includes(j.state))) {
      info.error = t("请先完成录音和正在进行的任务，再重启更新", "Finish recording and active tasks before restarting.");
      nativeProgress();
      return;
    }
    info.error = undefined;
    info.phase = installing ? "installing" : "downloading";
    nativeProgress();
    let polling = false;
    let active = true;
    const timer = window.setInterval(async () => {
      if (polling) return;
      polling = true;
      try { const status = await invoke<UpdateInfo>("native_update_status"); if (active) { info = status; nativeProgress(); } }
      catch { /* the main operation reports errors */ }
      finally { polling = false; }
    }, 600);
    try {
      if (installing) { await invoke("native_update_install"); }
      else {
        info = await invoke<UpdateInfo>("native_update_download");
        nativeProgress();
        toast(t("新版已下载并校验，点击版本提示即可重启更新", "Update verified. Open the version notice to restart and update."));
      }
    } catch (e) {
      const error = errorMessage(e);
      try { info = await invoke<UpdateInfo>("native_update_status"); } catch { /* keep last known state */ }
      if (info) { info.error = error; info.phase = installing ? "ready" : "available"; }
      nativeProgress();
      toast(error, true);
    } finally { active = false; clearInterval(timer); }
  };
}
