import { thisMacEn, yourMacEn } from "../platform";
import { usesLocalMinutes } from "../local-minutes";
/* 首页：录音表单 → 录音中（计时、电平、实时文字）→ 结束后立刻回到录音表单，转写和生成纪要在后台进行，
   进度显示在玻璃面板下方和侧边栏。第一次使用时玻璃面板里下载识别模型。另含拖拽导入。 */

import { t } from "../i18n";
import { api, isIOS, type Job, type MeetingSummary } from "../api";
import { $, $maybe, errorMessage, esc, ICON, main, toast } from "../dom";
import { poll } from "../poll";
import { isBusy, needsModels, S } from "../state";
import { bindSourcePicker, downloadInit, sourcePicker, sourceSizeMb } from "../source";
import { dismissJob, renderTasks } from "../tasks";
import { animatePanel, haptic } from "../motion";
import { isRecordingPage, leaveRecording, renderRecording, updateRecording } from "./recording";

type Phase = "idle" | "recording" | "downloading" | "error";

const INIT: Job = { state: "idle" };
const phaseOf = (j: Job): Phase =>
  j.state === "recording" ? "recording" : j.kind === "models" && isBusy(j) ? "downloading" : j.kind === "models" && j.state === "error" ? "error" : "idle";

export function renderHome(): void {
  // 录音中：整页换成录音工作台
  if (phaseOf(S.job) === "recording") {
    renderRecording(S.job);
    bindStop();
    updateRecording(S.job);
    return;
  }
  leaveRecording();
  main().innerHTML = `
    <div class="page home-page">
      <div class="hero-head">
        <span class="hero-tag"><b></b>${isIOS() ? t("本地转写 · 录音不出手机", "On-device transcription · audio never leaves your iPhone") : t(`本地转写 · 录音不出本机`, `On-device transcription · audio never leaves ${yourMacEn()}`)}</span>
        <h1>${t("开个会，纪要交给我", "Have the meeting. Tingo takes the notes.")}</h1>
        <p>${t("录下会议，自动转写、区分说话人，整理出议题、决议和待办", "Record, transcribe, tell speakers apart — and get topics, decisions and action items")}</p>
      </div>
      <section class="aurora" id="aurora">
        <div class="aurora-colors" aria-hidden="true"><i class="blob b1"></i><i class="blob b2"></i><i class="blob b3"></i><i class="blob b4"></i><i class="grain"></i></div>
        <div class="glass-ring"><div class="glass" id="glass"></div></div>
      </section>
      <div class="home-tasks" id="homeTasks"></div>
      <div class="quick">
        <div class="quick-item" id="qImport"><span class="ico">${ICON.upload}</span><span><b>${t("导入录音", "Import recording")}</b><small>${isIOS() ? t("从「文件」里选录音或视频", "Pick audio or video from Files") : t("拖进窗口，或点这里选文件", "Drop it in, or click to choose a file")}</small></span>${ICON.chevron}</div>
        <a class="quick-item" href="#/meetings"><span class="ico">${ICON.file}</span><span><b>${t("会议记录", "Meetings")}</b><small id="qCount">${t("查看全部会议", "See all meetings")}</small></span>${ICON.chevron}</a>
        <a class="quick-item" href="#/settings/llm"><span class="ico">${ICON.bot}</span><span><b>${t("大模型", "LLM")}</b><small id="qModel">${t("设置纪要用的模型", "Choose the model that writes minutes")}</small></span>${ICON.chevron}</a>
        <a class="quick-item" href="#/settings/push"><span class="ico">${ICON.send}</span><span><b>${t("推送到群", "Share to chat")}</b><small>${t("飞书 · 钉钉 · 企业微信", "Feishu · DingTalk · WeCom")}</small></span>${ICON.chevron}</a>
      </div>
    </div>`;
  $("#qImport").onclick = () => $("#fileInput").click();
  if (S.meetings.length) $("#qCount").textContent = t(`共 ${S.meetings.length} 场会议`, `${S.meetings.length} meetings`);
  if (S.env) $("#qModel").textContent = isIOS() && usesLocalMinutes() ? "当前：MiniCPM5-2B · 手机离线" : S.env.llm_ready ? t(`当前：${S.env.llm_model}`, `Using ${S.env.llm_model}`) : t("还没有配置", "Not set up yet");
  updateHome(S.job, INIT, true);
  renderTasks();
}

export function updateHome(job: Job, prev: Job, forceRender = false): void {
  const phase = phaseOf(job);
  if (phase === "recording") {
    if (!isRecordingPage()) {
      renderRecording(job);
      bindStop();
    }
    return updateRecording(job);
  }
  if (isRecordingPage()) return renderHome();
  const glass = $maybe("#glass");
  if (!glass) return;
  const aurora = $("#aurora");
  aurora.classList.toggle("live", false);
  if (forceRender || phase !== phaseOf(prev) || glass.dataset.phase !== phase) {
    const height = glass.getBoundingClientRect().height;
    const hadPhase = !!glass.dataset.phase;
    glass.dataset.phase = phase;
    glass.innerHTML = VIEWS[phase](job);
    bindHome(phase);
    if (hadPhase) animatePanel(glass, height);
  }
  if (phase === "downloading") updateDownloading(job);
}


function downloadPanel(): string {
  const mb = sourceSizeMb();
  return `
    <div class="live-head" style="color:var(--blue)">${t("第一次使用", "First run")}</div>
    <div class="timer" style="font-size:22px;margin:12px 0 6px;font-family:var(--font)">${t(`先下载识别模型（约 <span id="dlSize">${mb}</span> MB）`, `Download the speech model first (~<span id="dlSize">${mb}</span> MB)`)}</div>
    <div class="proc-note">${isIOS() ? t("只需下载一次，建议连着 Wi-Fi。识别、区分说话人都在这台手机上完成，录音不会上传。", "One-time download — Wi-Fi recommended. Recognition and speaker separation run on this iPhone — your audio is never uploaded.") : t(`只需下载一次。识别、区分说话人都在这台电脑上完成，录音不会上传。`, `One-time download. Recognition and speaker separation run on ${thisMacEn()} — your audio is never uploaded.`)}</div>
    ${sourcePicker()}
    <div class="glass-foot">
      <span class="spacer"></span>
      <button class="btn primary" id="dlBtn">${t("下载模型", "Download")}</button>
    </div>`;
}

/* 录音时 App 被系统关掉，下次打开时中断前的声音会恢复成一场会议。会多半还没开完：
   首页直接问要不要接着录，接着录的这一段结束后自动和它合成一场。 */
const RESUME_DISMISSED = "tingo.resume.dismissed";

function interruptedMeeting(): MeetingSummary | undefined {
  if (!S.env?.merge) return undefined;
  const m = S.meetings.find((x) => x.interrupted && !x.has_transcript);
  if (!m) return undefined;
  try {
    if (localStorage.getItem(RESUME_DISMISSED) === m.id) return undefined;
  } catch {
    /* 读不了就照常提示 */
  }
  // 隔了大半天的就不是"还没开完的会"了
  const started = new Date(`${m.date}T${m.time || "00:00"}:00`).getTime();
  return Date.now() - started < 12 * 3600_000 ? m : undefined;
}

function idleForm(): string {
  if (needsModels()) return downloadPanel();
  const sys = S.env ? S.env.system_audio : true;
  const cut = interruptedMeeting();
  return `
    ${
      cut
        ? `<div class="resume-note">
            <span><b>${t("上次的录音被中断了", "Your last recording was cut off")}</b><small>${t(`「${esc(cut.title)}」${esc(cut.time)} 开始，中断前的内容已经保住`, `“${esc(cut.title)}”, started ${esc(cut.time)} — everything before the cut is safe`)}</small></span>
            <button class="btn primary" id="resumeGo">${t("接着录", "Continue")}</button>
            <button class="btn ghost" id="resumeNo" title="${t("不用了", "Dismiss")}">×</button>
          </div>`
        : ""
    }
    <input class="title-input" id="fTitle" placeholder="${t("会议名称，比如：周会、项目评审", "Meeting name, e.g. Weekly sync, Design review")}" value="${esc(S.form.title)}" maxlength="60">
    <input class="hot-input" id="fHot" placeholder="${t("热词（可选）：人名、项目代号、专有名词，用逗号隔开", "Hotwords (optional): names, project codes, jargon — comma separated")}" value="${esc(S.form.hotwords)}">
    <div class="glass-foot">
      <button class="toggle desk-only ${S.form.system && sys ? "on" : ""}" id="tSys" aria-pressed="${Boolean(S.form.system && sys)}" ${sys ? "" : `disabled title='${t("需要 macOS 15+", "Requires macOS 15+")}'`}>${ICON.monitor}${t("系统声音", "System audio")}</button>
      <button class="toggle desk-only ${S.form.mic ? "on" : ""}" id="tMic" aria-pressed="${Boolean(S.form.mic)}">${ICON.mic}${t("麦克风", "Microphone")}</button>
      <span class="spacer"></span>
      <span class="rec-label">${t("开始录音", "Record")}</span>
      <button class="rec-btn" id="recBtn" aria-label="${t("开始录音", "Start recording")}" title="${t("开始录音", "Start recording")}"><span class="core" aria-hidden="true"></span></button>
    </div>`;
}

const downloadingView = () => `
  <div class="live-head" style="color:var(--blue)"><span class="live-dot" style="background:var(--cyan)"></span>${t("下载识别模型", "Downloading the speech model")}</div>
  <div class="timer" id="dlText" style="font-size:22px;margin:12px 0 10px">0 MB</div>
  <div class="steps" style="grid-template-columns:1fr"><div class="step now"><div class="bar"><div id="dlBar" style="position:absolute;inset:0 auto 0 0;width:0;background:linear-gradient(90deg,var(--cyan),var(--blue));border-radius:99px"></div></div></div></div>
  <div class="proc-note">${t("识别模型来自 ModelScope，标点和声纹模型来自 GitHub；网络中断会自动续传", "Speech model from ModelScope, punctuation and voiceprint models from GitHub — interrupted downloads resume automatically")}</div>`;

function updateDownloading(job: Job): void {
  const m = /([\d.]+)\s*\/\s*([\d.]+)/.exec(job.stage ?? "");
  $("#dlText").textContent = job.stage || t("准备下载…", "Starting download…");
  if (m) $("#dlBar").style.width = `${Math.min(100, (Number(m[1]) / Number(m[2])) * 100).toFixed(1)}%`;
}

const errorView = (job: Job) => `
  <div class="live-head">${t("模型下载没有完成", "The model download didn't finish")}</div>
  <div class="err-box" style="margin-top:12px">${esc(job.error)}</div>
  <div class="proc-note">${t("已下载的部分会保留，重试时接着下", "What's downloaded is kept — retrying resumes from there")}</div>
  <div class="glass-foot">
    <button class="btn" id="againBtn">${t("关闭", "Close")}</button>
    <span class="spacer"></span>
    <button class="btn primary" id="dlBtn">${t("重试", "Retry")}</button>
  </div>`;

const VIEWS: Record<Phase, (job: Job) => string> = {
  idle: idleForm,
  recording: () => "", // 录音中整页换成录音工作台（recording.ts）
  downloading: downloadingView,
  error: errorView,
};

function bindHome(phase: Phase): void {
  const dl = $maybe("#dlBtn");
  if (dl) {
    bindSourcePicker($("#glass"), (src) => ($("#dlSize").textContent = String(sourceSizeMb(src))));
    dl.onclick = async () => {
      (dl as HTMLButtonElement).disabled = true;
      try {
        S.job = await api<Job>("/api/models/download", downloadInit());
        poll();
      } catch (e) {
        toast(errorMessage(e), true);
        (dl as HTMLButtonElement).disabled = false;
      }
    };
  }
  if (phase === "idle" && !needsModels()) {
    const title = $<HTMLInputElement>("#fTitle");
    title.oninput = () => (S.form.title = title.value);
    title.onkeydown = (e) => {
      if (e.key === "Enter") void startRecording();
    };
    const hot = $<HTMLInputElement>("#fHot");
    hot.oninput = () => (S.form.hotwords = hot.value);
    $("#tSys").onclick = (e) => {
      S.form.system = !S.form.system;
      (e.currentTarget as HTMLElement).classList.toggle("on", S.form.system);
      (e.currentTarget as HTMLElement).setAttribute("aria-pressed", String(S.form.system));
    };
    $("#tMic").onclick = (e) => {
      S.form.mic = !S.form.mic;
      (e.currentTarget as HTMLElement).classList.toggle("on", S.form.mic);
      (e.currentTarget as HTMLElement).setAttribute("aria-pressed", String(S.form.mic));
    };
    $("#recBtn").onclick = () => startRecording();
    const cut = interruptedMeeting();
    if (cut && $maybe("#resumeGo")) {
      $("#resumeGo").onclick = () => continueMeeting(cut);
      $("#resumeNo").onclick = () => {
        try {
          localStorage.setItem(RESUME_DISMISSED, cut.id);
        } catch {
          /* 存不了的话下次还会再问一次 */
        }
        updateHome(S.job, INIT, true);
      };
    }
  } else if (phase === "error") {
    $("#againBtn").onclick = async () => {
      const id = S.job.id;
      S.job = { state: "idle" };
      updateHome(S.job, INIT, true);
      if (id) await dismissJob(id);
    };
  }
}

/** 接着录：新录的这一段结束后自动接到这场会议后面，合成一场（会被打断、App 被关掉之后用）。 */
export async function continueMeeting(m: { id: string; title: string }): Promise<void> {
  if (S.jobs.some((j) => j.state === "recording")) {
    toast(t("正在录音，先结束这一场", "A recording is in progress — stop it first"), true);
    return;
  }
  if (await startRecording(m)) location.hash = "#/";
}

/** 开始录音；`continueOf` 有值时是接着那场会议录。返回有没有录起来。 */
async function startRecording(continueOf?: { id: string; title: string }): Promise<boolean> {
  const sys = S.form.system && (!S.env || S.env.system_audio);
  const mic = S.form.mic || isIOS(); // iOS 只能录麦克风，没有开关
  if (!sys && !mic) {
    toast(t("系统声音和麦克风至少开一个", "Turn on system audio or the microphone"), true);
    return false;
  }
  const btn = $maybe<HTMLButtonElement>("#recBtn");
  if (btn?.disabled) return false;
  if (btn) {
    btn.disabled = true;
    btn.setAttribute("aria-busy", "true");
    const label = btn.previousElementSibling;
    if (label) label.textContent = t("正在启动…", "Starting…");
  }
  try {
    S.job = await api<Job>("/api/record/start", {
      json: {
        title: continueOf?.title || S.form.title.trim() || t("会议", "Meeting"),
        system: sys,
        mic,
        hotwords: S.form.hotwords.split(/[,，、\s]+/).filter(Boolean),
        continue_of: continueOf?.id,
      },
    });
    S.form.title = "";
    haptic("record");
    poll();
    return true;
  } catch (e) {
    toast(errorMessage(e), true);
    if (btn) {
      btn.disabled = false;
      btn.removeAttribute("aria-busy");
      const label = btn.previousElementSibling;
      if (label) label.textContent = t("开始录音", "Record");
    }
    return false;
  }
}

function bindStop(): void {
  $("#stopBtn").onclick = stopRecording;
}

async function stopRecording(): Promise<void> {
  const btn = $<HTMLButtonElement>("#stopBtn");
  if (btn.disabled) return;
  btn.disabled = true;
  btn.setAttribute("aria-busy", "true");
  btn.textContent = t("正在保存…", "Saving…");
  btn.setAttribute("aria-label", btn.textContent);
  try {
    await api("/api/record/stop", { method: "POST" });
    haptic("success");
    toast(t("录音已保存，转写和纪要在后台进行，可以开始下一场", "Saved. Transcription and minutes continue in the background — you can start the next meeting"));
    S.job = { state: "idle" };
    updateHome(S.job, INIT, true);
    poll();
  } catch (e) {
    toast(errorMessage(e), true);
    btn.disabled = false;
    btn.removeAttribute("aria-busy");
    btn.textContent = t("结束录音", "Stop");
    btn.setAttribute("aria-label", t("结束录音", "Stop recording"));
  }
}

/* ---------- 导入 ---------- */

export async function importFile(file: File | undefined): Promise<void> {
  if (!file) return;
  const fd = new FormData();
  fd.append("file", file);
  fd.append("title", file.name.replace(/\.[^.]+$/, ""));
  fd.append("modified", String(file.lastModified || 0));
  toast(t(`正在导入 ${file.name}`, `Importing ${file.name}`));
  try {
    await api<Job>("/api/import", { method: "POST", body: fd });
    toast(t(`已导入 ${file.name}，转写和纪要在后台进行`, `Imported ${file.name} — transcribing in the background`));
    poll();
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

export function setupImport(): void {
  const input = $<HTMLInputElement>("#fileInput");
  input.onchange = () => {
    importFile(input.files?.[0]);
    input.value = "";
  };
  const overlay = $("#dropOverlay");
  let depth = 0;
  window.addEventListener("dragenter", (e) => {
    if (e.dataTransfer?.types?.includes("Files")) {
      depth++;
      overlay.classList.add("show");
    }
  });
  window.addEventListener("dragleave", () => {
    if (--depth <= 0) {
      depth = 0;
      overlay.classList.remove("show");
    }
  });
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    depth = 0;
    overlay.classList.remove("show");
    importFile(e.dataTransfer?.files[0]);
  });
}
