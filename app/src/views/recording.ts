/* 录音工作台：开始录音后首页整页换成这里。
   顶栏固定（计时、电平、标记重点、结束），下面是整场的实时转写：同一人连续说的话合成一段，
   默认跟着最新一句；往上翻就停下，底部出现「↓ N 句新内容」。点某一句、顶栏按钮或 ⌘M 标记重点，
   结束时存进会议文件夹，详情页的转写里高亮。 */

import { api, type Job } from "../api";
import { $, $maybe, color, errorMessage, esc, fmt, ICON, main, toast } from "../dom";
import { speakerName, t } from "../i18n";

interface Line {
  start_ms: number;
  speaker?: number | null;
  text: string;
}

/** 同一人隔这么久再开口，另起一段 */
const NEW_PARAGRAPH_MS = 30_000;

const FLAG = '<svg viewBox="0 0 24 24"><path d="M5 21V4M5 4h11l-2 4 2 4H5"/></svg>';

let state: {
  job: string;
  lines: Line[];
  /** 说话人版本：录音中整场重新聚类改过前面句子的说话人时，后端从头给，这里整场重画 */
  rev: number;
  marks: number[];
  /** 后端没有 /api/live（Python 版）：退回任务里的最近几句 */
  legacy: boolean;
  fetching: boolean;
  lastFetch: number;
  following: boolean;
  /** 停下跟随时已经有多少句，用来算「N 句新内容」 */
  seen: number;
  /** 程序自己滚动后这段时间内的滚动事件不算用户往上翻（平滑滚动会连续触发好几次） */
  autoScrollUntil: number;
} | null = null;

const sourceName = (s: string) => (s === "system" ? t("系统声音", "System audio") : t("麦克风", "Microphone"));
const who = (speaker: number) => speakerName(t(`说话人${speaker + 1}`, `Speaker ${speaker + 1}`));
/** 会议文件夹名「2026-11-08_1400_周会」里的会议名 */
const titleOf = (job: Job) => (job.meeting ?? "").replace(/^\d{4}-\d{2}-\d{2}_\d{4}_?/, "").replace(/_/g, " ") || t("会议", "Meeting");

export const isRecordingPage = (): boolean => !!$maybe("#recWork");

export function renderRecording(job: Job): void {
  state = { job: job.id ?? "", lines: [], rev: 0, marks: [], legacy: false, fetching: false, lastFetch: 0, following: true, seen: 0, autoScrollUntil: 0 };
  const started = job.started_at ? new Date(job.started_at * 1000) : new Date(Date.now() - (job.elapsed ?? 0) * 1000);
  const mod = navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl+";
  main().innerHTML = `
    <div class="page rec-page" id="recWork">
      <header class="rec-bar">
        <div class="rec-who">
          <span class="rec-state"><span class="live-dot"></span>${t("录音中", "Recording")}</span>
          <b class="rec-title">${esc(titleOf(job))}</b>
          <small>${esc((job.sources ?? []).map(sourceName).join(" + "))}</small>
        </div>
        <div class="rec-timer" id="timer">00:00</div>
        <div class="rec-meters">${(job.sources ?? [])
          .map((s) => `<div class="rec-meter" id="meter-${s}" title="${sourceName(s)}">${s === "mic" ? ICON.mic : ICON.monitor}<span class="meter-bars">${"<i></i>".repeat(12)}</span></div>`)
          .join("")}</div>
        <div class="rec-actions">
          <button class="btn" id="markBtn" title="${t(`标记现在说的内容（${mod}M）`, `Mark this moment (${mod}M)`)}">${FLAG}<span>${t("标记重点", "Mark")}</span><b class="mark-count" id="markCount" hidden></b></button>
          <button class="btn rec-stop" id="stopBtn" aria-label="${t("结束录音", "Stop recording")}">${t("结束录音", "Stop")}</button>
        </div>
      </header>
      <p class="rec-note" id="recNote"></p>
      <section class="rec-transcript" id="recLines" aria-live="polite">
        <div class="rec-start">${t(`${started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })} 开始录音`, `Started at ${started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`)}</div>
      </section>
      <div class="rec-listening" id="recListening"><i></i><i></i><i></i><span id="recListenText">${t("正在听，说完一句一两秒内出现在这里", "Listening — each sentence appears a second or two after it's said")}</span></div>
      <div class="rec-follow" id="recFollow" hidden><button class="rec-follow-btn" id="recFollowBtn"></button></div>
    </div>`;
  $("#markBtn").onclick = () => void mark();
  $("#recFollowBtn").onclick = () => follow(true);
  $("#recLines").onclick = (e) => {
    const line = (e.target as HTMLElement).closest<HTMLElement>(".rec-line");
    if (line) void mark(Number(line.dataset.ms));
  };
  main().onscroll = onScroll;
  document.addEventListener("keydown", onKey);
}

/** 离开录音工作台（录音结束或切到别的页面） */
export function leaveRecording(): void {
  document.removeEventListener("keydown", onKey);
  if (main().onscroll === onScroll) main().onscroll = null;
  state = null;
}

function onKey(e: KeyboardEvent): void {
  if (!isRecordingPage()) return leaveRecording();
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "m") {
    e.preventDefault();
    void mark();
  }
}

/** 只有用户自己往上翻才停下跟随；内容变长不会触发滚动事件 */
function onScroll(): void {
  if (!state) return;
  if (Date.now() < state.autoScrollUntil) return;
  const el = main();
  const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
  if (distance < 48) {
    if (!state.following) follow(false);
  } else if (state.following && distance > 96) {
    state.following = false;
    state.seen = state.lines.length;
  }
}

function follow(scroll: boolean): void {
  if (!state) return;
  state.following = true;
  $("#recFollow").hidden = true;
  if (scroll) scrollToEnd(true);
}

function scrollToEnd(smooth = false): void {
  if (!state) return;
  const el = main();
  if (el.scrollHeight - el.scrollTop - el.clientHeight < 2) return;
  state.autoScrollUntil = Date.now() + (smooth ? 700 : 150);
  el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "instant" });
}

export function updateRecording(job: Job): void {
  if (!state || state.job !== (job.id ?? "")) renderRecording(job);
  $("#timer").textContent = fmt((job.elapsed ?? 0) * 1000);
  let peak = 0;
  for (const [k, v] of Object.entries(job.levels ?? {})) {
    peak = Math.max(peak, v);
    const bars = Array.from(document.querySelectorAll<HTMLElement>(`#meter-${k} i`));
    const on = Math.round(v * bars.length);
    bars.forEach((b, i) => b.classList.toggle("on", i < on));
  }
  $("#recListening").classList.toggle("hearing", peak > 0.08);
  const silentMin = Math.floor((job.silent_s ?? 0) / 60);
  const note = $("#recNote");
  note.textContent =
    silentMin >= 1
      ? job.auto_stop_s !== undefined
        ? t(`已经 ${silentMin} 分钟没人说话，${Math.max(1, Math.ceil(job.auto_stop_s / 60))} 分钟后自动结束录音`, `No one has spoken for ${silentMin} min — stopping automatically in ${Math.max(1, Math.ceil(job.auto_stop_s / 60))} min`)
        : t(`已经 ${silentMin} 分钟没人说话`, `No one has spoken for ${silentMin} min`)
      : job.model_loaded === false
        ? t("正在准备识别模型…", "Preparing the speech model…")
        : "";
  note.hidden = !note.textContent;
  if (state!.legacy) return setLines(job.live ?? [], true);
  if (!state!.fetching && Date.now() - state!.lastFetch > 400) void fetchLines();
}

async function fetchLines(): Promise<void> {
  const s = state;
  if (!s) return;
  s.fetching = true;
  s.lastFetch = Date.now();
  try {
    const r = await api<{ job: string; from: number; total: number; rev?: number; lines: Line[]; marks: number[] }>(`/api/live?from=${s.lines.length}&rev=${s.rev}`);
    if (state !== s || r.job !== s.job) return;
    // 说话人重新区分过：后端从头给了整场，清掉重画
    if ((r.rev ?? 0) !== s.rev && r.from === 0) {
      s.rev = r.rev ?? 0;
      s.lines = [];
      $("#recLines").querySelectorAll(".rec-para").forEach((p) => p.remove());
    }
    // 起点对不上（比如重连后后端重启过）：整场重取
    if (r.from !== s.lines.length) {
      s.lines = [];
      $("#recLines").querySelectorAll(".rec-para").forEach((p) => p.remove());
      return;
    }
    s.marks = r.marks;
    setLines(r.lines, false);
  } catch {
    // 旧后端（Python 版）没有这个接口
    if (state === s) s.legacy = true;
  } finally {
    if (state === s) s.fetching = false;
  }
}

/** `replace` 为 true 时 `lines` 是最近几句（旧接口），整段重画；否则是新增的句子，接在后面 */
function setLines(lines: Line[], replace: boolean): void {
  const s = state;
  if (!s) return;
  const box = $("#recLines");
  if (replace) {
    if (lines.map((l) => l.start_ms).join() === s.lines.map((l) => l.start_ms).join()) return;
    box.querySelectorAll(".rec-para").forEach((p) => p.remove());
    s.lines = [];
  }
  for (const line of lines) {
    const last = s.lines.at(-1);
    let para = box.lastElementChild as HTMLElement | null;
    const same = last && para?.classList.contains("rec-para") && (line.speaker ?? -1) === (last.speaker ?? -1) && line.start_ms - last.start_ms < NEW_PARAGRAPH_MS;
    if (!same) {
      para = document.createElement("div");
      para.className = "rec-para";
      const head = line.speaker == null ? "" : `<span class="rec-who-name" style="--c:${color(line.speaker)}">${esc(who(line.speaker))}</span>`;
      para.innerHTML = `<div class="rec-para-head">${head}<span class="rec-time">${fmt(line.start_ms)}</span></div><div class="rec-para-body"></div>`;
      box.append(para);
    }
    const span = document.createElement("span");
    span.className = "rec-line";
    span.dataset.ms = String(line.start_ms);
    span.title = t("点一下标记为重点", "Click to mark");
    span.textContent = line.text;
    para!.querySelector(".rec-para-body")!.append(span);
    s.lines.push(line);
  }
  if (s.lines.length) $("#recListenText").textContent = t("正在听", "Listening");
  paintMarks();
  if (s.following) scrollToEnd();
  else if (s.lines.length > s.seen) {
    const btn = $("#recFollowBtn");
    btn.innerHTML = `<b>↓ ${t(`${s.lines.length - s.seen} 句新内容`, `${s.lines.length - s.seen} new`)}</b><span>${esc(s.lines.at(-1)!.text)}</span>`;
    $("#recFollow").hidden = false;
  }
}

/** 每个标记只归一句：落在哪句说话期间（这句开头到下一句开头）就标哪句，比第一句还早的归第一句 */
export function markedIndexes(starts: number[], marks: number[]): Set<number> {
  const out = new Set<number>();
  for (const m of marks) {
    let i = 0;
    while (i + 1 < starts.length && starts[i + 1] <= m) i++;
    if (starts.length) out.add(i);
  }
  return out;
}

function paintMarks(): void {
  const s = state;
  if (!s) return;
  const spans = Array.from(document.querySelectorAll<HTMLElement>("#recLines .rec-line"));
  const marked = markedIndexes(spans.map((x) => Number(x.dataset.ms)), s.marks);
  spans.forEach((span, i) => span.classList.toggle("marked", marked.has(i)));
  const count = $("#markCount");
  count.hidden = !s.marks.length;
  count.textContent = String(s.marks.length);
}

/** 标记重点；`ms` 不给就是现在。再点同一处取消 */
async function mark(ms?: number): Promise<void> {
  const s = state;
  if (!s) return;
  try {
    const r = await api<{ marks: number[] }>("/api/record/mark", { json: ms === undefined ? {} : { ms } });
    if (state !== s) return;
    const added = r.marks.length > s.marks.length;
    s.marks = r.marks;
    paintMarks();
    if (ms === undefined && added) toast(t("已标记，这句话出来后会高亮", "Marked — it will be highlighted once transcribed"));
  } catch (e) {
    toast(errorMessage(e), true);
  }
}
