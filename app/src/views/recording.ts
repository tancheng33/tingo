/* 录音工作台：开始录音后首页整页换成这里。
   顶栏固定（计时、电平、标记重点、结束），下面是整场的实时转写：同一人连续说的话合成一段，
   默认跟着最新一句；往上翻就停下，底部出现「↓ N 句新内容」。点某一句、顶栏按钮或 ⌘M 标记重点，
   结束时存进会议文件夹，详情页的转写里高亮。 */

import { api, isIOS, type Job } from "../api";
import { $, $$, $maybe, color, errorMessage, esc, fmt, ICON, main, toast } from "../dom";
import { speakerName, t } from "../i18n";
import { haptic } from "../motion";
import { S } from "../state";
import { renderMarkdown } from "../markdown";

interface Line {
  start_ms: number;
  speaker?: number | null;
  text: string;
}

/** 边开会边整理的速记版纪要（引擎每两分钟更新一次） */
interface LiveNotes {
  v: number;
  enabled: boolean;
  busy: boolean;
  error?: string | null;
  upto_ms: number;
  /** 只有版本比界面手上的新时才带 */
  markdown?: string;
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
  /** 正在看实时转写还是实时纪要 */
  tab: "lines" | "notes";
  /** 手上的实时纪要是第几版 */
  notesV: number;
} | null = null;

/** 没录到的时长，说成"40 秒""5 分钟" */
export const gapText = (s: number): string =>
  s < 60 ? t(`${Math.max(1, Math.round(s))} 秒`, `${Math.max(1, Math.round(s))} s`) : t(`${Math.round(s / 60)} 分钟`, `${Math.round(s / 60)} min`);

const sourceName = (s: string) => (s === "system" ? t("系统声音", "System audio") : t("麦克风", "Microphone"));
const who = (speaker: number) => speakerName(t(`说话人${speaker + 1}`, `Speaker ${speaker + 1}`));
/** 会议文件夹名「2026-11-08_1400_周会」里的会议名 */
const titleOf = (job: Job) => (job.meeting ?? "").replace(/^\d{4}-\d{2}-\d{2}_\d{4}_?/, "").replace(/_/g, " ") || t("会议", "Meeting");

export const isRecordingPage = (): boolean => !!$maybe("#recWork");

export function renderRecording(job: Job): void {
  state = { job: job.id ?? "", lines: [], rev: 0, marks: [], legacy: false, fetching: false, lastFetch: 0, following: true, seen: 0, autoScrollUntil: 0, tab: "lines", notesV: 0 };
  const started = job.started_at ? new Date(job.started_at * 1000) : new Date(Date.now() - (job.elapsed ?? 0) * 1000);
  const mod = navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl+";
  main().innerHTML = `
    <div class="page rec-page" id="recWork">
      <header class="rec-bar">
        <div class="rec-who">
          <span class="rec-state" id="recState"><span class="live-dot"></span><span>${t("录音中", "Recording")}</span></span>
          <b class="rec-title">${esc(titleOf(job))}</b>
          <small>${esc((job.sources ?? []).map(sourceName).join(" + "))}</small>
        </div>
        <div class="rec-timer" id="timer">00:00</div>
        <div class="rec-meters">${(job.sources ?? [])
          .map((s) => `<div class="rec-meter" id="meter-${s}" title="${sourceName(s)}">${s === "mic" ? ICON.mic : ICON.monitor}<span class="meter-bars">${"<i></i>".repeat(12)}</span></div>`)
          .join("")}</div>
        <div class="rec-actions">
          <button class="btn" id="markBtn" title="${isIOS() ? t("标记现在说的内容", "Mark this moment") : t(`标记现在说的内容（${mod}M）`, `Mark this moment (${mod}M)`)}">${FLAG}<span>${t("标记重点", "Mark")}</span><b class="mark-count" id="markCount" hidden></b></button>
          <button class="btn rec-stop" id="stopBtn" aria-label="${t("结束录音", "Stop recording")}">${t("结束录音", "Stop")}</button>
        </div>
        <div class="rec-tabs-row">
          <div class="seg rec-tabs" id="recTabs">
            <button class="on" data-v="lines">${t("实时转写", "Transcript")}</button>
            <button data-v="notes">${t("实时纪要", "Live minutes")}<i class="rec-tab-dot" id="recNotesDot" hidden></i></button>
          </div>
        </div>
      </header>
      <p class="rec-note" id="recNote"></p>
      <section class="rec-notes" id="recNotes" hidden>
        <p class="rec-notes-status" id="recNotesStatus">${t("说够一段话后开始整理，之后每两分钟更新一次", "Starts once there's enough said, then updates every two minutes")}</p>
        <div class="md" id="recNotesBody"></div>
      </section>
      <section class="rec-transcript" id="recLines" aria-live="polite">
        <div class="rec-start">${t(`${started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })} 开始录音`, `Started at ${started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`)}</div>
      </section>
      <div class="rec-listening" id="recListening"><i></i><i></i><i></i><span id="recListenText">${t("正在听，说完一句一两秒内出现在这里", "Listening — each sentence appears a second or two after it's said")}</span></div>
      <div class="rec-follow" id="recFollow" hidden><button class="rec-follow-btn" id="recFollowBtn"></button></div>
    </div>`;
  $("#markBtn").onclick = () => void mark();
  $$<HTMLButtonElement>("#recTabs button").forEach((b) => (b.onclick = () => showTab(b.dataset.v as "lines" | "notes")));
  // 纪要里的时间戳：切回转写，跳到那句
  $("#recNotesBody").onclick = (e) => {
    const ts = (e.target as HTMLElement).closest<HTMLElement>(".ts");
    if (ts) jumpTo(Number(ts.dataset.ms));
  };
  $("#recFollowBtn").onclick = () => follow(true);
  $("#recLines").onclick = (e) => {
    const line = (e.target as HTMLElement).closest<HTMLElement>(".rec-line");
    if (line) void mark(Number(line.dataset.ms));
  };
  main().onscroll = onScroll;
  document.addEventListener("keydown", onKey);
}

function showTab(tab: "lines" | "notes", toEnd = true): void {
  if (!state) return;
  state.tab = tab;
  $$<HTMLButtonElement>("#recTabs button").forEach((b) => b.classList.toggle("on", b.dataset.v === tab));
  const notes = tab === "notes";
  $("#recNotes").hidden = !notes;
  $("#recLines").hidden = notes;
  $("#recListening").hidden = notes;
  if (notes) {
    $("#recNotesDot").hidden = true;
    $("#recFollow").hidden = true;
    main().scrollTo({ top: 0 });
  } else if (toEnd) {
    follow(true);
  }
}

/** 切到转写，滚到 `ms` 那一刻说的那句（这句开头到下一句开头之间），闪一下 */
function jumpTo(ms: number): void {
  if (!state) return;
  showTab("lines", false);
  const spans = Array.from(document.querySelectorAll<HTMLElement>("#recLines .rec-line"));
  let hit = spans[0];
  for (const sp of spans) if (Number(sp.dataset.ms) <= ms) hit = sp;
  if (!hit) return;
  state.following = false;
  state.seen = state.lines.length;
  state.autoScrollUntil = Date.now() + 800;
  hit.scrollIntoView({ block: "center", behavior: "smooth" });
  hit.classList.remove("flash");
  void hit.offsetWidth;
  hit.classList.add("flash");
}

function setNotes(n: LiveNotes | undefined): void {
  const s = state;
  if (!s || !n) return;
  const status = $("#recNotesStatus");
  status.classList.toggle("bad", !!n.error && !n.busy);
  status.textContent = !n.enabled
    ? t("实时纪要没开：在「设置 → 大模型」里配好大模型、打开「开会时实时整理纪要」就能用。录完照样会生成纪要。", "Live minutes are off: set up a language model and turn on “Live minutes while recording” in Settings → Language model. Minutes are still written when you stop.")
    : n.busy
      ? t("正在整理新说的内容…", "Adding what was just said…")
      : n.error
        ? t(`这次没整理成：${n.error}。两分钟后再试`, `Couldn't update this time: ${n.error}. Trying again in two minutes`)
        : n.v === 0
          ? t("说够一段话后开始整理，之后每两分钟更新一次", "Starts once there's enough said, then updates every two minutes")
          : t(`整理到 ${fmt(n.upto_ms)}，每两分钟更新。这是速记版，录完会用整场转写写一份正式纪要`, `Covers up to ${fmt(n.upto_ms)}, updated every two minutes. This is a quick draft; full minutes are written from the whole transcript when you stop`);
  if (n.markdown !== undefined && n.v !== s.notesV) {
    s.notesV = n.v;
    $("#recNotesBody").innerHTML = renderMarkdown(n.markdown);
    if (s.tab !== "notes") $("#recNotesDot").hidden = false;
  }
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
  // 录音暂时停着：麦克风被来电或别的 App 占用，或者 App 被系统挂起过。录音组件会自己接着录
  const paused = job.paused_s !== undefined;
  $("#recWork").classList.toggle("paused", paused);
  $("#recState span:last-child").textContent = paused ? t("录音已暂停", "Recording paused") : t("录音中", "Recording");
  let peak = 0;
  for (const [k, level] of Object.entries(job.levels ?? {})) {
    // 暂停时音轨不再往后写，读到的还是停下前最后一刻的电平，不能当成现在的
    const v = paused ? 0 : level;
    peak = Math.max(peak, v);
    const bars = Array.from(document.querySelectorAll<HTMLElement>(`#meter-${k} i`));
    const on = Math.round(v * bars.length);
    bars.forEach((b, i) => b.classList.toggle("on", i < on));
  }
  $("#recListening").classList.toggle("hearing", peak > 0.08);
  const silentMin = Math.floor((job.silent_s ?? 0) / 60);
  // 「接着录」：这一段录完会接到哪场会议后面
  const continued = job.merging?.length ? (S.meetings.find((m) => m.id === job.merging![0])?.title ?? "") : "";
  const note = $("#recNote");
  note.classList.toggle("bad", !!job.error);
  note.classList.toggle("warn", paused && !job.error);
  note.textContent = job.error || (
    paused
      ? t(
          `麦克风被来电或别的 App 占用，已经 ${gapText(job.paused_s ?? 0)}没在录，正在尝试接着录…`,
          `The microphone is in use by a call or another app — nothing recorded for ${gapText(job.paused_s ?? 0)}. Trying to resume…`,
        )
      : silentMin >= 1
      ? job.auto_stop_s !== undefined
        ? t(`已经 ${silentMin} 分钟没人说话，${Math.max(1, Math.ceil(job.auto_stop_s / 60))} 分钟后自动结束录音`, `No one has spoken for ${silentMin} min — stopping automatically in ${Math.max(1, Math.ceil(job.auto_stop_s / 60))} min`)
        : t(`已经 ${silentMin} 分钟没人说话`, `No one has spoken for ${silentMin} min`)
      : job.model_loaded === false
        ? t("正在准备识别模型…", "Preparing the speech model…")
        : continued
          ? t(`接着「${continued}」录，结束后自动接成一场`, `Continuing “${continued}” — joined into one meeting when you stop`)
          : "");
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
    const r = await api<{ job: string; from: number; total: number; rev?: number; lines: Line[]; marks: number[]; notes?: LiveNotes }>(
      `/api/live?from=${s.lines.length}&rev=${s.rev}&notes_v=${s.notesV}`,
    );
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
    setNotes(r.notes);
  } catch {
    // 旧后端（Python 版）没有这个接口
    // iOS 的本地连接可能在切后台后短暂断开，继续请求整场转写。
    if (state === s && !isIOS()) s.legacy = true;
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
  if (s.tab === "notes") return;
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
    if (added) haptic("selection");
    if (ms === undefined && added) toast(t("已标记，这句话出来后会高亮", "Marked — it will be highlighted once transcribed"));
  } catch (e) {
    toast(errorMessage(e), true);
  }
}
