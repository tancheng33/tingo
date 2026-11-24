import { invoke } from "@tauri-apps/api/core";
import { api, meetingPath, type MeetingDetail, type Job } from "./api";
import { errorMessage, toast } from "./dom";
import { putLocalJob } from "./local-jobs";
import { poll } from "./poll";
import { refreshEnv } from "./sidebar";

let running = false;
export async function localMinutes(d: MeetingDetail, _saved: () => void): Promise<void> {
  if (running) return toast("离线纪要正在生成，请稍候");
  running = true;
  const id = `local:${d.id}`;
  const began = Date.now();
  let job: Job = { id, local: true, kind: "summarize", meeting: d.id, state: "summarizing", stage: "MiniCPM · 正在加载模型", minutes: "", elapsed: 0 };
  let timer: ReturnType<typeof setInterval> | undefined;
  let finished = false;
  try {
    const state = await invoke<{ready: boolean; busy: boolean}>("plugin:recorder|localMinutesStatus");
    if (state.busy) throw new Error("离线模型正在处理其他任务");
    if (!state.ready) { toast("请先在设置里下载离线模型"); location.hash = "/settings/llm"; return; }
    putLocalJob(job);
    await poll();
    timer = setInterval(() => {
      void invoke<{stage: string; partial?: string}>("plugin:recorder|localMinutesStatus").then(state => {
        if (finished) return;
        job = {...job, stage: state.stage, minutes: state.partial ?? "", elapsed: (Date.now() - began) / 1000};
        putLocalJob(job);
      }).catch(() => {});
    }, 500);
    const text = d.turns.map(turn => `[${turn.ts}] ${turn.speaker}：${turn.text}`).join("\n");
    const result = await invoke<{text: string; complete: boolean}>("plugin:recorder|localMinutesGenerate", {text});
    finished = true;
    localStorage.setItem(`local-minutes:${d.id}`, result.text);
    if (!result.complete) throw new Error("输出达到上限，未覆盖原纪要。已保留草稿，请重试。");
    await api(meetingPath(d.id, "/minutes"), {method: "PUT", json: {text: result.text}});
    localStorage.removeItem(`local-minutes:${d.id}`);
    job = {...job, state: "done", minutes: result.text, elapsed: (Date.now() - began) / 1000};
    putLocalJob(job);
  } catch (e) {
    finished = true;
    putLocalJob({...job, state: "error", error: errorMessage(e)});
    toast(errorMessage(e), true);
  } finally {
    finished = true;
    if (timer) clearInterval(timer);
    running = false;
    void poll();
  }
}

export const usesLocalMinutes = (): boolean => localStorage.getItem("minutes-mode") === "offline";

export async function bindLocalMinutesSettings(box: HTMLElement, online: HTMLElement): Promise<void> {
  box.innerHTML = `<h2>纪要生成</h2><p class="hint">选择在手机上整理，或使用在线服务。会议里仍然点「生成纪要」。</p><div class="minutes-mode"><button class="btn" data-mode="online">在线服务</button><button class="btn" data-mode="offline">手机离线</button></div><div id="offlineMinutesConfig"><div class="local-model"><b>MiniCPM5-2B</b><p class="hint">约 1.56 GB · 下载后无需联网<br>下载前自动测速，选择更快线路</p><label class="field">下载线路<select class="input" id="modelSource"><option value="auto">自动选择</option><option value="modelscope">国内 · ModelScope</option><option value="mirror">国内 · HF 镜像</option><option value="official">国外 · Hugging Face</option></select></label><p id="modelStatus" role="status">检查模型…</p><button class="btn" id="modelDownload" disabled>下载模型</button><button class="btn" id="modelCancel" hidden>取消下载</button></div></div>`;
  const offline = box.querySelector<HTMLElement>("#offlineMinutesConfig")!;
  const download = box.querySelector<HTMLButtonElement>("#modelDownload")!;
  const cancel = box.querySelector<HTMLButtonElement>("#modelCancel")!;
  const status = box.querySelector<HTMLElement>("#modelStatus")!;
  const drawMode = () => {
    const local = usesLocalMinutes(); online.hidden = local; offline.hidden = !local;
    box.querySelectorAll<HTMLButtonElement>("[data-mode]").forEach(b => { const selected = (b.dataset.mode === "offline") === local; b.classList.toggle("primary", selected); b.setAttribute("aria-pressed", String(selected)); });
  };
  box.querySelectorAll<HTMLButtonElement>("[data-mode]").forEach(b => { b.onclick = () => { localStorage.setItem("minutes-mode", b.dataset.mode!); drawMode(); void refreshEnv(); }; });
  drawMode();
  let timer: ReturnType<typeof setTimeout>;
  const refresh = async () => {
    if (!box.isConnected) return;
    try {
      const state = await invoke<{ ready: boolean; busy: boolean; stage: string; error?: string }>("plugin:recorder|localMinutesStatus");
      if (!box.isConnected) return;
      status.textContent = state.busy ? state.stage : state.ready ? "已下载，可以离线生成纪要" : state.error || "未下载 · 建议连接 Wi-Fi";
      download.hidden = state.ready; download.disabled = state.busy; cancel.hidden = !state.busy || !(state.stage.includes("下载") || state.stage.includes("测速"));
      if (state.busy) timer = setTimeout(() => { void refresh(); }, 1000);
    } catch (e) { status.textContent = errorMessage(e); }
  };
  download.onclick = async () => {
    download.disabled = true; status.textContent = "准备下载…";
    const source = box.querySelector<HTMLSelectElement>("#modelSource")!.value;
    const task = invoke("plugin:recorder|localMinutesDownload", { source });
    timer = setTimeout(() => { void refresh(); }, 500);
    try { await task; clearTimeout(timer); await refresh(); }
    catch (e) { clearTimeout(timer); if (box.isConnected) { status.textContent = errorMessage(e); download.disabled = false; cancel.hidden = true; } }
  };
  cancel.onclick = () => { void invoke("plugin:recorder|localMinutesCancel").catch(e => toast(errorMessage(e), true)); };
  await refresh();
}

export function cancelLocalMinutes(): void { void invoke("plugin:recorder|localMinutesCancel").catch(e => toast(errorMessage(e), true)); }
