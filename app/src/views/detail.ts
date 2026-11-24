/* 会议详情：纪要 / 转写两个视图、回听播放器、说话人改名、编辑纪要、重新生成。 */

import { api, isIOS, type Job, type MeetingDetail, meetingPath, share, url } from "../api";
import { $, $$, $maybe, closeModal, color, errorMessage, esc, fmt, ICON, initial, main, openModal, openSheet, type SheetItem, toast, tsToMs } from "../dom";
import { fishboneToText, turnResolver } from "../fishbone";
import { renderMarkdown } from "../markdown";
import { minutesDocument, wordDocument } from "../pdf";
import { confirmBox, deleteAudio, deleteMeeting, deletesToRecent, editTags, fmtBytes, renameBox, setMeta } from "../manage";
import { showTag } from "./meetings";
import type { Config } from "../api";
import { poll } from "../poll";
import { refreshMeetings } from "../sidebar";
import { isBusy, jobOf, minutesProgress, progressOf, S, sectionLabels } from "../state";
import { speakerName, t } from "../i18n";
import { dismissJob } from "../tasks";
import { cancelLocalMinutes, localMinutes, usesLocalMinutes } from "../local-minutes";
import { markedIndexes } from "./recording";

type SpeakerSuggestion = { index: number; name: string; evidence?: string };
type SpeakerGuess = { started: number; elapsed?: number; suggestions?: SpeakerSuggestion[]; error?: string };
const speakerGuesses = new Map<string, SpeakerGuess>();

function isCurrentMeeting(id: string): boolean {
  return S.detail?.id === id && decodeURIComponent(location.hash.slice(1)).split("?")[0] === `/m/${id}`;
}

function updateSpeakerGuess(id: string): void {
  if (!isCurrentMeeting(id)) return;
  const button = $maybe<HTMLButtonElement>("#aGuess");
  const status = $maybe("#speakerGuessStatus");
  if (!button || !status) return;
  const guess = speakerGuesses.get(id);
  const pending = !!guess && guess.elapsed === undefined;
  button.disabled = pending;
  button.innerHTML = ICON.spark + (pending ? t("正在猜名字…", "Guessing names…") : guess?.suggestions?.length ? t("查看 AI 猜名结果", "Review AI names") : t("让 AI 根据对话猜名字", "Let AI guess names from the conversation"));
  status.hidden = !guess;
  status.classList.toggle("bad", !!guess?.error);
  status.classList.toggle("pending", pending);
  status.textContent = !guess ? "" : pending
    ? t(`正在分析对话 · 已用 ${fmt(Date.now() - guess.started)} · 耗时取决于模型`, `Analyzing conversation · Elapsed ${fmt(Date.now() - guess.started)} · Time depends on the model`)
    : guess.error ? guess.error
    : t(`已完成 · 用时 ${fmt(guess.elapsed!)} · ${guess.suggestions?.length ? "请确认猜名结果" : "未找到名字线索"}`, `Finished in ${fmt(guess.elapsed!)} · ${guess.suggestions?.length ? "Review the suggested names" : "No name clues found"}`);
}

async function guessSpeakers(): Promise<void> {
  const d = S.detail;
  if (!d) return;
  const existing = speakerGuesses.get(d.id);
  if (existing && existing.elapsed === undefined) return;
  if (existing?.suggestions?.length) {
    renameSpeakers(existing.suggestions);
    return;
  }
  const guess: SpeakerGuess = { started: Date.now() };
  speakerGuesses.set(d.id, guess);
  updateSpeakerGuess(d.id);
  const timer = window.setInterval(() => updateSpeakerGuess(d.id), 1000);
  try {
    const result = await api<{ suggestions: SpeakerSuggestion[] }>(meetingPath(d.id, "/speakers/guess"), { method: "POST" });
    guess.suggestions = result.suggestions;
    // 不打断其他会议或正在编辑的弹窗；结果保留在这场会议的按钮里。
    if (isCurrentMeeting(d.id) && $("#modal").hidden && result.suggestions.length) renameSpeakers(result.suggestions);
  } catch (e) {
    guess.error = errorMessage(e);
  } finally {
    window.clearInterval(timer);
    guess.elapsed = Date.now() - guess.started;
    updateSpeakerGuess(d.id);
  }
}
import { continueMeeting } from "./home";

let audioEl: HTMLAudioElement | null = null;
let detailTab: "minutes" | "transcript" = "minutes";

export function stopAudio(): void {
  if (audioEl) {
    audioEl.pause();
    audioEl.src = "";
    audioEl = null;
  }
}

const MARK_FLAG = '<svg viewBox="0 0 24 24"><path d="M5 21V4M5 4h11l-2 4 2 4H5"/></svg>';

const backLink = () => `<a class="back" href="#/meetings">${ICON.back}${t("会议记录", "Meetings")}</a>`;

/** `at`：从搜索结果点进来时要跳到的时间点（毫秒），打开转写并定位到那一句 */
export async function renderDetail(id: string, keepTab = false, at?: number): Promise<void> {
  if (!keepTab) detailTab = at === undefined ? "minutes" : "transcript";
  main().innerHTML = `<div class="page">${backLink()}</div>`;
  const page = main().firstElementChild;
  let d: MeetingDetail;
  try {
    d = await api<MeetingDetail>(meetingPath(id));
  } catch (e) {
    if (main().firstElementChild !== page) return;
    main().innerHTML = `<div class="page">${backLink()}<div class="empty"><h3>${t("打不开这场会议", "Couldn't open this meeting")}</h3><div>${esc(errorMessage(e))}</div></div></div>`;
    return;
  }
  if (main().firstElementChild !== page || decodeURIComponent(location.hash.slice(1)).split("?")[0] !== `/m/${id}`) return; // 加载期间已经切走（地址后面可能带 ?t=）
  S.detail = d;
  const jobHere = jobOf(id);
  // 打开会议就算看过结果了：收起这场会议已结束的任务
  S.jobs.filter((j) => j.meeting === id && j.kind !== "models" && !isBusy(j) && j.id).forEach((j) => dismissJob(j.id!));
  main().innerHTML = `
    <div class="page">
      ${backLink()}
      <div class="detail-head">
        <h1>${esc(d.title)}${d.archived ? `<span class="arch-badge">${t("已归档", "Archived")}</span>` : ""}<button class="icon-btn" id="aTitle" title="${t("修改标题", "Rename")}">${ICON.edit}</button></h1>
        <button class="sheet-btn phone-only" id="aSheet" aria-label="${t("会议操作", "Meeting actions")}">···</button>
        <div class="detail-actions">
          ${
            d.minutes
              ? `<div class="menu-wrap"><button class="btn" id="aExport">${ICON.download}${t("导出", "Export")}${ICON.chevron}</button>
                  <div class="menu" id="exportMenu" hidden>
                    ${exportMenuItems()}
                  </div></div><button class="btn" id="aPush">${ICON.send}${t("推送到群", "Share to chat")}</button>`
              : ""
          }
          ${d.has_transcript && !jobHere ? `<button class="btn" id="aRegen">${ICON.redo}${d.minutes ? t("重新生成", "Regenerate") : t("生成纪要", "Write minutes")}</button>` : ""}
          ${d.audio && S.env?.merge && !jobHere ? `<button class="btn" id="aContinue" title="${t("这场会还没开完：再录一段，结束后自动接成一场", "The meeting isn't over: record another part and it's joined into this one")}">${ICON.mic}${t("接着录", "Continue")}</button>` : ""}
          <button class="btn ghost desk-only" id="aFolder" title="${t("打开文件夹", "Open folder")}">${ICON.folder}</button>
          <div class="menu-wrap"><button class="btn ghost" id="aMore" title="${t("更多", "More")}">···</button>
            <div class="menu" id="moreMenu" hidden>
              <button data-act="tags">${ICON.spark}<span><b>${t("编辑标签", "Edit tags")}</b><small>${t("按项目、类型给会议分类", "Group meetings by project or type")}</small></span></button>
              <button data-act="archive">${ICON.folder}<span><b>${d.archived ? t("取消归档", "Unarchive") : t("归档", "Archive")}</b><small>${d.archived ? t("放回会议列表", "Move back to the meeting list") : t("从会议列表收起，搜索时照样能找到", "Hide from the list; still searchable")}</small></span></button>
              ${d.audio ? `<button data-act="audio">${ICON.mic}<span><b>${t("只删录音", "Delete recording")}</b><small>${t("保留转写和纪要，腾出空间", "Keep transcript & minutes, free up space")}</small></span></button>` : ""}
              <button data-act="delete" class="danger">${ICON.trash}<span><b>${t("删除会议", "Delete meeting")}</b><small>${deletesToRecent() ? t("移到「最近删除」，可以恢复", "Moves to Recently deleted; restorable") : t("移到废纸篓，可以恢复", "Moves to the Trash; restorable")}</small></span></button>
            </div></div>
        </div>
      </div>
      <div class="meta">
        <span>${esc(d.date)} ${esc(d.time)}</span>
        ${(d.tags ?? []).map((tg) => `<a class="tag-chip sm" href="#/meetings" data-tag="${esc(tg)}">${esc(tg)}</a>`).join("")}
        ${d.duration ? `<span class="sep"></span><span>${t("时长", "Duration")} ${esc(d.duration)}</span>` : ""}
        ${
          d.speaker_list.length
            ? `<span class="sep"></span>
          <span class="speakers">${d.speaker_list
            .map((s) => `<span class="chip"><span class="dot" style="background:${color(s.index)}"></span>${esc(speakerName(s.name))}</span>`)
            .join("")}
          <span class="speaker-actions"><button class="btn" id="aRename">${ICON.people}${t("改名", "Rename")}</button><button class="btn" id="aGuess">${ICON.spark}${t("让 AI 根据对话猜名字", "Let AI guess names from the conversation")}</button></span><span id="speakerGuessStatus" class="speaker-guess-status" hidden></span></span>`
            : ""
        }
      </div>
      ${
        d.has_transcript
          ? `
        <div class="tabs-row">
          <div class="seg"><button data-tab="minutes">${t("纪要", "Minutes")}</button><button data-tab="transcript">${t("转写", "Transcript")}</button></div>
          <span class="grow"></span>
          <button class="btn ghost" id="aHistory" aria-label="${t("历史版本", "Version history")}">${ICON.file}<span class="lbl">${t("历史版本", "Version history")}</span></button>
          <button class="btn ghost" id="aEdit" aria-label="${t("编辑", "Edit")}" ${detailTab === "minutes" && d.minutes ? "" : "hidden"}>${ICON.edit}<span class="lbl">${t("编辑", "Edit")}</span></button>
        </div>
        <div class="card" id="pane"></div>`
          : `
        ${
          jobHere
            ? `<div class="card doc" id="pane"></div>`
            : `<div class="card"><div class="empty"><h3>${t("录音还没有转写", "Not transcribed yet")}</h3><div>${t("可能是上次处理被打断了", "The last run may have been interrupted")}</div>
          ${d.audio ? `<button class="btn primary" id="aProcess">${t("开始转写", "Transcribe")}</button>` : ""}</div></div>`
        }`
      }
      ${d.audio ? playerHTML() : ""}
    </div>`;

  $$<HTMLButtonElement>(".seg button").forEach(
    (b) =>
      (b.onclick = () => {
        detailTab = b.dataset.tab as typeof detailTab;
        drawPane();
      }),
  );
  const on = (sel: string, fn: () => void) => {
    const el = $maybe(sel);
    if (el) el.onclick = fn;
  };
  on("#aPush", pushMinutes);
  bindMoreMenu(d);
  $$<HTMLAnchorElement>(".meta .tag-chip[data-tag]").forEach(
    (a) =>
      (a.onclick = (e) => {
        e.preventDefault();
        showTag(a.dataset.tag ?? "");
      }),
  );
  on("#aTitle", () => renameTitle(d));
  on("#aSheet", () => openActions(d));
  bindExportMenu(d);
  on("#aRegen", regenerate);
  on("#aContinue", () => void continueMeeting({ id, title: d.title }));
  on("#aRename", () => renameSpeakers());
  on("#aGuess", guessSpeakers);
  updateSpeakerGuess(id);
  on("#aEdit", editMinutes);
  on("#aHistory", showMinutesHistory);
  on("#aFolder", () => api(meetingPath(id, "/reveal"), { method: "POST" }));
  on("#aProcess", async () => {
    try {
      const job = await api<Job>(meetingPath(id, "/process"), { method: "POST" });
      S.jobs = [...S.jobs.filter((j) => j.id !== job.id), job];
      renderDetail(id, true);
      poll();
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
  if (d.audio) bindPlayer(id);
  if (d.has_transcript) drawPane();
  if (jobHere) updateDetailJob(jobHere, { state: "idle" });
  if (at !== undefined && d.has_transcript) jumpTo(at);
}

/** 定位到某个时间点：播放器跳过去（不自动播放），转写里那一句高亮并滚到中间 */
function jumpTo(ms: number): void {
  if (audioEl) audioEl.currentTime = ms / 1000;
  const turns = $$<HTMLElement>(".turn");
  let hit: HTMLElement | undefined;
  for (const tn of turns) {
    if (Number(tn.dataset.ms) <= ms + 200) hit = tn;
    else break;
  }
  if (!hit) return;
  hit.classList.add("found");
  hit.scrollIntoView({ block: "center" });
  setTimeout(() => hit?.classList.remove("found"), 4000);
}

function drawPane(): void {
  const d = S.detail;
  const pane = $maybe("#pane");
  if (!d || !pane) return;
  $$<HTMLButtonElement>(".seg button").forEach((b) => b.classList.toggle("on", b.dataset.tab === detailTab));
  const edit = $maybe("#aEdit");
  if (edit) edit.hidden = !(detailTab === "minutes" && d.minutes) || !!jobOf(d.id); // 生成中改了也会被新纪要覆盖
  if (detailTab === "minutes") {
    pane.className = "card doc";
    pane.innerHTML = d.minutes
      ? `<div class="md">${renderMarkdown(d.minutes, { skipHead: true, resolve: turnResolver(d.turns, d.duration ? tsToMs(d.duration) : Infinity) })}</div>`
      : `<div class="minutes-empty">${t("还没有纪要。", "No minutes yet.")}<br><br><button class="btn primary" id="paneGen">${ICON.spark}${t("生成纪要", "Write minutes")}</button></div>`;
    const gen = $maybe("#paneGen");
    if (gen) gen.onclick = regenerate;
  } else {
    pane.className = "card turns";
    // 录音中标记的重点：标记落在哪段说话期间就高亮哪段（和录音工作台同一规则）
    const marks = d.marks ?? [];
    const markedSet = markedIndexes(d.turns.map((tn) => tn.start_ms), marks);
    const marked = (i: number) => markedSet.has(i);
    pane.innerHTML = (marks.length ? `<div class="marks-note">${MARK_FLAG}${t(`录音时标记了 ${marks.length} 处重点`, `${marks.length} moments marked while recording`)}</div>` : "") + d.turns
      .map(
        (tn, i) => `
      <div class="turn ${marked(i) ? "marked" : ""}" data-i="${i}" data-ms="${tn.start_ms}">
        <span class="av" style="background:${color(tn.color)}">${esc(initial(tn.speaker))}</span>
        <div><div class="who">${esc(speakerName(tn.speaker))}<button class="ts" data-ms="${tn.start_ms}">${esc(tn.ts)}</button>${marked(i) ? `<span class="turn-flag" title="${t("录音时标记的重点", "Marked while recording")}">${MARK_FLAG}</span>` : ""}</div><div class="text">${esc(tn.text)}</div></div>
      </div>`,
      )
      .join("");
  }
  $$<HTMLButtonElement>(".ts", pane).forEach(
    (b) =>
      (b.onclick = (e) => {
        e.stopPropagation(); // 鱼骨图分支里的时间戳：只跳转，不收起分支
        seek(Number(b.dataset.ms));
      }),
  );
  $$<HTMLButtonElement>(".fb-draw", pane).forEach((b) => (b.onclick = toExcalidraw));
  // 人名与术语：一键加进热词，以后识别、写纪要都按这个写法
  $$<HTMLButtonElement>(".terms-add", pane).forEach(
    (b) =>
      (b.onclick = async () => {
        const words = $$<HTMLElement>(".term", pane).map((x) => x.dataset.term ?? "").filter(Boolean);
        try {
          const c = await api<Config>("/api/config");
          const add = words.filter((w) => !c.hotwords.includes(w));
          if (!add.length) return toast(t("这些词都已经在热词里了", "These are already in your hotwords"));
          await api("/api/config", {
            method: "PUT",
            json: { base_url: c.base_url, model: c.model, api_key: null, hotwords: [...c.hotwords, ...add], webhook: c.webhook, secret: null },
          });
          toast(t(`已加入 ${add.length} 个热词`, `Added ${add.length} hotword${add.length > 1 ? "s" : ""}`));
        } catch (e) {
          toast(errorMessage(e), true);
        }
      }),
  );
  // 鱼骨图：点分支展开它引用的原话
  $$<HTMLElement>(".fb-branch[role=button]", pane).forEach((br) => {
    const toggle = () => {
      const q = $maybe(".fb-quotes", br);
      if (q) {
        q.hidden = !q.hidden;
        br.classList.toggle("open", !q.hidden);
      }
    };
    br.onclick = toggle;
    br.onkeydown = (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    };
  });
}

/* 生成过程：转写 → 生成纪要（按模板一节一节写），实时显示写到哪、写了多少、用了多久，纪要边写边显示。 */
const genSkeleton = () => `
  <div class="gen" id="genBox">
    <div class="gen-head"><span class="live-dot"></span><b id="genTitle"></b><span class="grow"></span><span class="gen-meta" id="genMeta"></span></div>
    <div class="gen-bar"><i id="genBar"></i></div>
    <div class="gen-steps" id="genSteps"></div>
    <div class="md gen-stream" id="genStream"></div>
  </div>`;

function stepChip(label: string, st: "done" | "now" | "todo"): string {
  return `<span class="gen-step ${st}">${st === "done" ? "✓" : st === "now" ? "<i></i>" : ""}${esc(label)}</span>`;
}

function drawGen(job: Job): void {
  const summarizing = job.state === "summarizing";
  const mp = minutesProgress(job);
  const llm = job.local ? "MiniCPM5-2B" : S.env?.llm_model ?? t("大模型", "The LLM");
  $("#genTitle").textContent = job.local ? `${t("手机离线", "On-device")} · ${job.stage || t("正在生成纪要", "Writing minutes")}` : summarizing
    ? mp.chunk
      ? t(`会议较长，先分段提炼（${mp.chunk[0]}/${mp.chunk[1]}）`, `Long meeting — summarizing part ${mp.chunk[0]}/${mp.chunk[1]}`)
      : mp.merging
        ? t("把各段要点合并成纪要", "Merging the parts into minutes")
        : mp.section < 0
          ? job.thinking
            ? t(`${llm} 正在思考`, `${llm} is thinking`)
            : t(`${llm} 正在读转写`, `${llm} is reading the transcript`)
          : t(`${llm} 正在写纪要`, `${llm} is writing the minutes`)
    : job.state === "transcribing"
      ? `${t("正在本地转写", "Transcribing on-device")}${job.stage ? ` · ${job.stage}` : ""}`
      : job.state === "finalizing"
        ? t("转写最后一段，区分说话人", "Transcribing the last part and separating speakers")
        : job.state === "loading"
          ? t("加载识别模型", "Loading the speech model")
          : t("正在保存录音", "Saving the recording");
  const pct = Math.round(progressOf(job) * 100);
  const elapsed = fmt((job.elapsed ?? 0) * 1000);
  const wrote =
    summarizing && mp.chars
      ? t(` · 已写 ${mp.chars} 字`, ` · ${mp.chars} chars written`)
      : summarizing && job.thinking
        ? t(` · 已思考 ${job.thinking} 字`, ` · ${job.thinking} chars of thinking`)
        : "";
  $("#genMeta").textContent = t(`用时 ${elapsed}${wrote} · 约 ${pct}%`, `${elapsed}${wrote} · ~${pct}%`);
  $("#genBar").style.width = `${Math.max(2, pct)}%`;

  // 步骤：转写 → 纪要的各节
  const transcribed = summarizing;
  const steps = [
    stepChip(t("转写", "Transcribe"), transcribed ? "done" : "now"),
    stepChip(t("通读与思考", "Read & think"), !summarizing ? "todo" : mp.section >= 0 ? "done" : "now"),
  ];
  sectionLabels().forEach((label, i) => {
    const st = !summarizing || mp.chunk ? "todo" : i < mp.section ? "done" : i === mp.section ? "now" : "todo";
    steps.push(stepChip(label, st));
  });
  const html = steps.join(`<span class="gen-arrow"></span>`);
  const box = $("#genSteps");
  if (box.dataset.html !== html) {
    box.dataset.html = html;
    box.innerHTML = html;
  }

  const gs = $("#genStream");
  const minutes = summarizing && !mp.chunk ? (job.minutes ?? "") : "";
  const key = `${job.state}:${minutes.length}:${minutes ? 0 : (job.thinking ?? 0)}`;
  if (gs.dataset.key !== key) {
    gs.dataset.key = key;
    gs.innerHTML = minutes
      ? renderMarkdown(minutes) + `<span class="gen-caret"></span>`
      : `<p class="gen-wait">${
          summarizing
            ? mp.chunk
              ? t("长会议分成几段，每段先提炼要点，最后合并成一份纪要。", "Long meetings are split into parts; each part is summarized first, then merged into one set of minutes.")
              : job.thinking
                ? t(
                    `大模型在动笔前先思考（已思考 ${job.thinking} 字），长会议这一步可能要一两分钟，开始写后内容会一节一节出现在这里。`,
                    `The model is thinking before it writes (${job.thinking} chars so far). For long meetings this can take a minute or two; sections will appear here as they're written.`,
                  )
                : t("大模型先通读整场转写，开始动笔后内容会一节一节出现在这里。", "The model reads the whole transcript first; sections will appear here as they're written.")
            : t(
                "识别在这台电脑上进行，录音不会上传。转写完成后接着生成纪要，不用守着，可以去开下一场会。",
                "Recognition runs on this Mac — audio is never uploaded. Minutes follow right after; no need to wait here, go ahead and start the next meeting.",
              )
        }</p>`;
    const near = main().scrollHeight - main().scrollTop - main().clientHeight < 160;
    if (near && minutes) main().scrollTop = main().scrollHeight; // 停在底部时跟着往下滚
  }
}

export function updateDetailJob(job: Job, prev: Job): void {
  if (!S.detail || job.meeting !== S.detail.id) return;
  if (isBusy(job)) {
    const pane = $maybe("#pane");
    if (!pane) return;
    detailTab = "minutes";
    $$<HTMLButtonElement>(".seg button").forEach((b) => b.classList.toggle("on", b.dataset.tab === "minutes"));
    pane.className = "card doc";
    if (!$maybe("#genBox")) pane.innerHTML = genSkeleton();
    const edit = $maybe("#aEdit");
    if (edit) edit.hidden = true;
    drawGen(job);
    if (job.local && !$maybe("#cancelLocalJob")) {
      const cancel = document.createElement("button");
      cancel.id = "cancelLocalJob";
      cancel.className = "btn";
      cancel.textContent = t("取消生成", "Cancel");
      cancel.onclick = cancelLocalMinutes;
      $("#genBox").appendChild(cancel);
    }
  } else if (isBusy(prev) && (job.state === "done" || job.state === "error")) {
    if (job.state === "done") toast(t("纪要已生成", "Minutes ready"));
    else toast(job.error || t("生成失败", "Couldn't write the minutes"), true);
    if (job.id) dismissJob(job.id);
    renderDetail(S.detail.id, true);
  }
}

async function showMinutesHistory(): Promise<void> {
  const d = S.detail;
  if (!d) return;
  openModal(`<h3>${t("纪要历史版本", "Minutes history")}</h3><p class="hint" id="historyLoading">${t("正在读取…", "Loading…")}</p><div class="modal-foot"><button class="btn" data-close>${t("关闭", "Close")}</button></div>`);
  const loading = $("#historyLoading");
  try {
    const result = await api<{ versions: { id: string; saved_at: number; text: string }[] }>(meetingPath(d.id, "/minutes/history"));
    if (!loading.isConnected || $("#modal").hidden || !isCurrentMeeting(d.id)) return;
    let versions = result.versions;
    openModal(`<h3>${t("纪要历史版本", "Minutes history")}</h3>
      <p class="hint">${t("自动保留每次修改前的纪要，可随时回看。", "Previous minutes are saved automatically for you to revisit.")}</p>
      <div class="history-layout">
        <nav class="history-sidebar" aria-label="${t("历史版本", "Version history")}"><div class="history-list-label">${t("已保存的版本", "Saved versions")}</div><div id="historyList"></div></nav>
        <section class="history-content"><div class="history-preview-head"><span id="historyDate"></span><span class="history-readonly">${t("只读预览", "Read-only")}</span></div><div id="historyPreview" class="md history-preview"></div></section>
      </div>
      <div class="modal-foot"><span class="test-msg" id="historyMessage" role="status"></span><button class="btn" id="historyDelete">${t("删除此版本", "Delete this version")}</button><details class="menu-wrap history-export" id="historyExport"><summary class="btn">${ICON.download}${t("导出", "Export")}${ICON.chevron}</summary><div class="menu">${exportMenuItems()}</div></details><button class="btn" id="historyPush">${ICON.send}${t("推送到群", "Share to chat")}</button><button class="btn" data-close>${t("关闭", "Close")}</button></div>`);
    const list = $("#historyList");
    let selected: string | undefined = versions[0]?.id;
    let deleting = false;
    const preview = $("#historyPreview");
    const remove = $<HTMLButtonElement>("#historyDelete");
    const message = $("#historyMessage");
    const exportMenu = $<HTMLDetailsElement>("#historyExport");
    const push = $<HTMLButtonElement>("#historyPush");
    const setBusy = (busy: boolean) => {
      deleting = busy;
      remove.disabled = busy;
      push.disabled = busy;
      $$<HTMLButtonElement>(".history-version", list).forEach(button => button.disabled = busy);
      $$<HTMLButtonElement>("button", exportMenu).forEach(button => button.disabled = busy);
    };
    const act = async (action: string) => {
      const version = versions.find(v => v.id === selected);
      exportMenu.open = false;
      if (!version || deleting) return;
      const snapshot = { ...d, minutes: version.text };
      const suffix = `?version=${encodeURIComponent(version.id)}`;
      setBusy(true);
      message.textContent = t("正在处理所选版本…", "Processing selected version…");
      try {
        if (action === "copy") {
          await copyText(fishboneToText(version.text));
        } else {
          const path = action === "srt" ? "/srt" : `/${action}${suffix}`;
          const json = action === "pdf" ? { html: minutesDocument(snapshot) }
            : action === "docx" ? { html: wordDocument(snapshot) }
            : action === "obsidian" ? { open: true } : undefined;
          const result = await api<{ path?: string; message?: string }>(meetingPath(d.id, path), { method: "POST", json });
          if (result.path) {
            if (isIOS()) await share(result.path);
            else toast(t(`已导出 ${result.path.split("/").pop()}`, `Exported ${result.path.split("/").pop()}`));
          }
          if (result.message) toast(result.message);
        }
        message.textContent = t("已完成", "Done");
      } catch (e) {
        message.textContent = errorMessage(e);
      } finally {
        setBusy(false);
      }
    };
    $$<HTMLButtonElement>("button[data-act]", exportMenu).forEach(button => button.onclick = () => act(button.dataset.act!));
    push.onclick = () => act("push");
    $('[data-act="srt"] small', exportMenu).textContent = t("整场会议的转写，不随纪要版本变化", "Meeting transcript, shared across minutes versions");
    let confirming = false;
    const draw = () => {
      confirming = false;
      remove.textContent = t("删除此版本", "Delete this version");
      remove.hidden = !versions.length;
      exportMenu.hidden = !versions.length;
      exportMenu.open = false;
      push.hidden = !versions.length;
      const version = versions.find(v => v.id === selected);
      $("#historyDate").textContent = version ? new Date(version.saved_at).toLocaleString() : t("暂无历史", "No history");
      $$<HTMLButtonElement>(".history-version", list).forEach(button => {
        const active = button.dataset.version === selected;
        button.classList.toggle("active", active);
        button.setAttribute("aria-current", String(active));
      });
      preview.innerHTML = version ? renderMarkdown(version.text) : `<p class="hint">${t("暂无历史版本，下次重新生成或编辑保存后会自动保留。", "No history yet. Previous minutes will be kept after the next regeneration or edit.")}</p>`;
      preview.scrollTop = 0;
      message.textContent = "";
    };
    const options = () => {
      if (!versions.some(v => v.id === selected)) selected = versions[0]?.id;
      list.innerHTML = versions.map((v, i) => {
        const date = new Date(v.saved_at);
        return `<button class="history-version" data-version="${esc(v.id)}"><span class="history-version-title">${esc(date.toLocaleDateString())}${i === 0 ? `<span class="history-latest">${t("最近", "Latest")}</span>` : ""}</span><span class="history-version-time">${esc(date.toLocaleTimeString())}</span></button>`;
      }).join("") || `<p class="hint history-list-empty">${t("还没有旧版本", "No saved versions")}</p>`;
      $$<HTMLButtonElement>(".history-version", list).forEach(button => button.onclick = () => {
        if (deleting) return;
        selected = button.dataset.version;
        draw();
      });
      draw();
    };
    remove.onclick = async () => {
      if (!confirming) {
        confirming = true;
        remove.textContent = t("确认删除", "Confirm deletion");
        message.textContent = t("删除后无法恢复，再点一次确认。", "This cannot be undone. Click again to confirm.");
        return;
      }
      const id = selected;
      if (!id) return;
      setBusy(true);
      try {
        await api(meetingPath(d.id, `/minutes/history/${encodeURIComponent(id)}`), { method: "DELETE" });
        versions = versions.filter(v => v.id !== id);
        options();
        message.textContent = t("已删除历史版本", "Version deleted");
      } catch (e) {
        message.textContent = errorMessage(e);
      } finally {
        setBusy(false);
      }
    };
    options();
  } catch (e) {
    if (loading.isConnected) loading.textContent = errorMessage(e);
  }
}

async function regenerate(): Promise<void> {
  if (!S.detail) return;
  if (isIOS() && usesLocalMinutes()) {
    if (S.jobs.some(isBusy)) return toast(t("请等待录音或转写任务完成", "Wait for recording or transcription to finish"));
    const id = S.detail.id;
    await localMinutes(S.detail, () => { void renderDetail(id, true); });
    return;
  }
  try {
    const job = await api<Job>(meetingPath(S.detail.id, "/summarize"), { method: "POST" });
    S.jobs = [...S.jobs.filter((j) => j.id !== job.id), job];
    updateDetailJob(job, { state: "idle" });
    poll();
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function pushMinutes(): Promise<void> {
  if (!S.detail) return;
  try {
    toast((await api<{ message: string }>(meetingPath(S.detail.id, "/push"), { method: "POST" })).message);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

/** 下拉菜单：点按钮开关，点外面收起 */
function bindMenu(btnSel: string, menuSel: string, onPick: (act: string) => void): void {
  const btn = $maybe(btnSel);
  const menu = $maybe(menuSel);
  if (!btn || !menu) return;
  const close = () => {
    menu.hidden = true;
    document.removeEventListener("pointerdown", outside);
  };
  const outside = (e: PointerEvent) => {
    if (!menu.contains(e.target as Node) && !btn.contains(e.target as Node)) close();
  };
  btn.onclick = () => {
    menu.hidden = !menu.hidden;
    if (!menu.hidden) document.addEventListener("pointerdown", outside);
  };
  $$<HTMLButtonElement>("button", menu).forEach(
    (b) =>
      (b.onclick = () => {
        close();
        onPick(b.dataset.act ?? "");
      }),
  );
}

async function renameTitle(d: MeetingDetail): Promise<void> {
  const v = await renameBox(d.title);
  if (v === undefined) return;
  try {
    await setMeta(d.id, { title: v });
    await refreshMeetings();
    renderDetail(d.id, true);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

/** 手机：详情页所有操作收进一个底部菜单，和桌面版的按钮、下拉菜单走同一套处理 */
function openActions(d: MeetingDetail): void {
  const busy = !!jobOf(d.id);
  const minutes: SheetItem[] = d.minutes
    ? [
        { label: t("导出 PDF", "Export PDF"), icon: ICON.file, run: () => toPdf(d) },
        { label: t("复制为 Markdown", "Copy as Markdown"), icon: ICON.copy, run: () => copyText(fishboneToText(d.minutes)) },
        { label: t("推送到群", "Share to chat"), icon: ICON.send, run: pushMinutes },
      ]
    : [];
  const content: SheetItem[] = [
    ...(d.has_transcript && !busy ? [{ label: d.minutes ? t("重新生成纪要", "Regenerate minutes") : t("生成纪要", "Write minutes"), icon: ICON.redo, run: regenerate }] : []),
    ...(d.has_transcript ? [{ label: t("导出 SRT 字幕", "Export SRT subtitles"), icon: ICON.play, run: () => exportFile(d, "/srt") }] : []),
    // 这场会还没开完（被打断、App 被关掉）：再录一段，结束后自动接成一场
    ...(d.audio && S.env?.merge && !busy ? [{ label: t("接着录", "Continue recording"), icon: ICON.mic, run: () => void continueMeeting({ id: d.id, title: d.title }) }] : []),
  ];
  const people: SheetItem[] = d.speaker_list.length
    ? [
        { label: t("给说话人改名", "Rename speakers"), icon: ICON.people, run: () => renameSpeakers() },
        { label: t("让 AI 猜名字", "Let AI guess names"), icon: ICON.spark, disabled: !!speakerGuesses.get(d.id) && speakerGuesses.get(d.id)!.elapsed === undefined, run: guessSpeakers },
      ]
    : [];
  const manage: SheetItem[] = [
    { label: t("修改标题", "Rename"), icon: ICON.edit, run: () => renameTitle(d) },
    { label: t("编辑标签", "Edit tags"), icon: ICON.spark, run: () => moreAction(d, "tags") },
    { label: d.archived ? t("取消归档", "Unarchive") : t("归档", "Archive"), icon: ICON.folder, run: () => moreAction(d, "archive") },
  ];
  const danger: SheetItem[] = [
    ...(d.audio ? [{ label: t("只删录音", "Delete recording"), icon: ICON.mic, danger: true, disabled: busy, run: () => moreAction(d, "audio") }] : []),
    { label: t("删除会议", "Delete meeting"), icon: ICON.trash, danger: true, disabled: busy, run: () => moreAction(d, "delete") },
  ];
  openSheet(d.title, [minutes, content, people, manage, danger]);
}

function bindMoreMenu(d: MeetingDetail): void {
  bindMenu("#aMore", "#moreMenu", (act) => moreAction(d, act));
}

async function moreAction(d: MeetingDetail, act: string): Promise<void> {
  try {
    if (act === "tags") {
      const tags = await editTags(d.tags ?? []);
      if (!tags) return;
      await setMeta(d.id, { tags });
    } else if (act === "archive") {
      await setMeta(d.id, { archived: !d.archived });
      toast(d.archived ? t("已放回会议列表", "Moved back to the list") : t("已归档", "Archived"));
    } else if (act === "audio") {
      if (!(await confirmBox(t("删除这场会议的录音？", "Delete this meeting's recording?"), isIOS() ? t("录音会移到 Tingo 文件夹的“已删除录音”，转写和纪要保留，以后就不能回听了。", "The recording moves to “已删除录音” in the Tingo folder; the transcript and minutes stay, but you can't play it back.") : t("录音会移到废纸篓，转写和纪要保留，以后就不能回听了。", "The recording moves to the Trash; the transcript and minutes stay, but you can't play it back."), t("删除录音", "Delete recording"))))
        return;
      stopAudio();
      const r = await deleteAudio(d.id);
      toast(t(`录音已删除，腾出 ${fmtBytes(r.freed_bytes)}`, `Recording deleted — freed ${fmtBytes(r.freed_bytes)}`));
    } else if (act === "delete") {
      if (!(await confirmBox(t(`删除「${esc(d.title)}」？`, `Delete “${esc(d.title)}”?`), deletesToRecent() ? t("录音、转写和纪要会一起移到「最近删除」，可以随时恢复。", "The recording, transcript and minutes move to Recently deleted — you can restore them anytime.") : t("录音、转写和纪要会一起移到废纸篓，需要的话可以从废纸篓恢复。", "The recording, transcript and minutes move to the Trash — you can restore them from there."), deletesToRecent() ? t("删除会议", "Delete meeting") : t("移到废纸篓", "Move to Trash"))))
        return;
      stopAudio();
      await deleteMeeting(d.id);
      toast(deletesToRecent() ? t("已移到「最近删除」", "Moved to Recently deleted") : t("已移到废纸篓", "Moved to the Trash"));
      await refreshMeetings();
      location.hash = "#/meetings";
      return;
    }
    await refreshMeetings();
    renderDetail(d.id, true);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

function exportMenuItems(): string {
  return `<button data-act="pdf">${ICON.file}<span><b>${t("导出 PDF", "Export PDF")}</b><small>${isIOS() ? t("A4 排版，发给别人或存到「文件」", "A4 layout — send it or save to Files") : t("A4 排版，存进会议文件夹", "A4 layout, saved in the meeting folder")}</small></span></button>
                    <button data-act="docx" class="desk-only">${ICON.edit}<span><b>${t("导出 Word", "Export Word")}</b><small>${t("可以接着改的 .docx", "An editable .docx")}</small></span></button>
                    <button data-act="srt">${ICON.play}<span><b>${t("导出 SRT 字幕", "Export SRT subtitles")}</b><small>${t("带说话人的逐句字幕", "Line-by-line subtitles with speakers")}</small></span></button>
                    <button data-act="obsidian" class="desk-only">${ICON.vault}<span><b>${t("存到 Obsidian", "Save to Obsidian")}</b><small>${t("存进笔记库并打开", "Save to your vault and open it")}</small></span></button>
                    <button data-act="copy">${ICON.copy}<span><b>${t("复制为 Markdown", "Copy as Markdown")}</b><small>${t("粘贴到文档、邮件里", "Paste into docs or email")}</small></span></button>`;
}

function bindExportMenu(d: MeetingDetail): void {
  const btn = $maybe("#aExport");
  const menu = $maybe("#exportMenu");
  if (!btn || !menu) return;
  const close = () => {
    menu.hidden = true;
    document.removeEventListener("pointerdown", outside);
  };
  const outside = (e: PointerEvent) => {
    if (!menu.contains(e.target as Node) && !btn.contains(e.target as Node)) close();
  };
  btn.onclick = () => {
    menu.hidden = !menu.hidden;
    if (!menu.hidden) document.addEventListener("pointerdown", outside);
  };
  $$<HTMLButtonElement>("button", menu).forEach(
    (b) =>
      (b.onclick = () => {
        close();
        if (b.dataset.act === "pdf") toPdf(d);
        else if (b.dataset.act === "docx") exportFile(d, "/docx", { html: wordDocument(d) });
        else if (b.dataset.act === "srt") exportFile(d, "/srt");
        else if (b.dataset.act === "obsidian") toObsidian();
        else copyText(fishboneToText(d.minutes));
      }),
  );
}

/** 导出完：桌面上已经在访达里选中了；iOS 弹系统分享面板 */
async function exported(path: string): Promise<void> {
  const name = path.split("/").pop();
  if (!isIOS()) return toast(t(`已导出 ${name}，已在访达中选中`, `Exported ${name} — shown in Finder`));
  try {
    await share(path);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function exportFile(d: MeetingDetail, path: string, json?: unknown): Promise<void> {
  try {
    const r = await api<{ path: string }>(meetingPath(d.id, path), json === undefined ? { method: "POST" } : { json });
    await exported(r.path);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function toPdf(d: MeetingDetail): Promise<void> {
  toast(t("正在排版 PDF…", "Laying out the PDF…"));
  try {
    const r = await api<{ path: string }>(meetingPath(d.id, "/pdf"), { json: { html: minutesDocument(d) } });
    await exported(r.path);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function toObsidian(): Promise<void> {
  if (!S.detail) return;
  try {
    await api(meetingPath(S.detail.id, "/obsidian"), { json: { open: true } });
    toast(t("已存到 Obsidian", "Saved to Obsidian"));
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function toExcalidraw(): Promise<void> {
  if (!S.detail) return;
  try {
    const r = await api<{ paths: string[] }>(meetingPath(S.detail.id, "/excalidraw"), { method: "POST" });
    if (isIOS() && r.paths?.[0]) return await share(r.paths[0]);
    const name = r.paths?.[0]?.split("/").pop() ?? "";
    toast(t(`已导出${name}，拖进 excalidraw.com 即可编辑`, `Exported ${name} — drop it into excalidraw.com to edit`));
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  toast(t("已复制纪要 Markdown", "Minutes copied as Markdown"));
}

function editMinutes(): void {
  const d = S.detail;
  if (!d) return;
  const pane = $("#pane");
  const page = pane.closest(".page");
  $("#aEdit").hidden = true;
  pane.className = "card doc";
  // 手机上 .page.editing 是全屏编辑：取消、保存在顶部，播放条和标签栏收起（见 style.css）
  page?.classList.add("editing");
  pane.innerHTML = `<textarea class="textarea" id="mdEdit" style="min-height:520px;font-size:14px">${esc(d.minutes)}</textarea>
    <div class="modal-foot edit-bar"><button class="btn" id="mdCancel">${t("取消", "Cancel")}</button><b class="edit-title">${t("编辑纪要", "Edit minutes")}</b><button class="btn primary" id="mdSave">${t("保存", "Save")}</button></div>`;
  main().scrollTop = 0;
  $("#mdCancel").onclick = () => {
    page?.classList.remove("editing");
    drawPane();
  };
  $("#mdSave").onclick = async () => {
    const text = $<HTMLTextAreaElement>("#mdEdit").value;
    try {
      await api(meetingPath(d.id, "/minutes"), { method: "PUT", json: { text } });
      toast(t("已保存", "Saved"));
      renderDetail(d.id, true);
      refreshMeetings();
    } catch (e) {
      toast(errorMessage(e), true);
    }
  };
}

function renameSpeakers(suggestions: SpeakerSuggestion[] = []): void {
  const d = S.detail;
  if (!d) return;
  openModal(`
    <h3>${t("给说话人改名", "Rename speakers")}</h3>
    <p class="hint">${t("按第一次发言的顺序排列。点转写里的时间戳可以回听确认是谁。", "Listed in order of first appearance. Click a timestamp in the transcript to hear who it is.")}</p>
    ${d.speaker_list
      .map(
        (s) =>
          `<div class="sp-row"><span class="av" style="background:${color(s.index)}">${esc(initial(s.name))}</span><input class="input" data-i="${s.index}" value="${s.custom ? esc(s.name) : ""}" placeholder="${esc(t(`说话人${s.index + 1}`, `Speaker ${s.index + 1}`))}"></div>`,
      )
      .join("")}
    ${suggestions.length ? `<p class="hint">${t("AI 猜测仅供参考，已命名的说话人保持原名。请核对后保存。", "AI suggestions may be inaccurate. Existing names are preserved. Review before saving.")}</p>` : ""}
    <div class="modal-foot"><button class="btn" data-close>${t("取消", "Cancel")}</button><button class="btn" id="spSave">${t("仅保存", "Save")}</button>${d.minutes ? `<button class="btn primary" id="spRegen">${t("保存并重新生成纪要", "Save & regenerate minutes")}</button>` : ""}</div>`);
  const save = async (regen: boolean) => {
    const names = $$<HTMLInputElement>("#modalBody input").map((x) => x.value.trim());
    try {
      S.detail = await api<MeetingDetail>(meetingPath(d.id, "/speakers"), { method: "PUT", json: { names } });
      if (speakerGuesses.get(d.id)?.elapsed !== undefined) speakerGuesses.delete(d.id);
      closeModal();
      await renderDetail(d.id, true);
      refreshMeetings();
      if (regen) regenerate();
      else toast(t("已更新说话人", "Speakers updated"));
    } catch (e) {
      toast(errorMessage(e), true);
    }
  };
  $("#spSave").onclick = () => save(false);
  for (const sg of suggestions) {
    const input = $maybe<HTMLInputElement>(`#modalBody input[data-i="${sg.index}"]`);
    if (!input || input.value.trim()) continue;
    input.value = sg.name;
    if (sg.evidence) input.closest(".sp-row")?.insertAdjacentHTML("beforeend", `<small class="sp-ev">${esc(sg.evidence)}</small>`);
  }
  const regen = $maybe("#spRegen");
  if (regen) regen.onclick = () => save(true);
}

/* ---------- 播放器 ---------- */

export const playerHTML = () => `
  <div class="player" id="player">
    <button class="play" id="pPlay" title="${t("播放（空格）", "Play (Space)")}">${ICON.play}</button>
    <span class="p-time" id="pTime">00:00 / --:--</span>
    <input class="audio-seek" id="pTrack" type="range" min="0" max="0" step="0.1" value="0" disabled aria-label="${t("播放进度", "Playback position")}">
    <button class="speed" id="pSpeed" aria-label="${t("播放速度 1 倍", "Playback speed 1 times")}">1×</button>
    <span></span>
  </div>`;

export function bindPlayer(id: string, source?: string): void {
  stopAudio();
  const audio = new Audio(source ?? url(meetingPath(id, "/audio")));
  audioEl = audio;
  audio.preload = "metadata";
  const speeds = [1, 1.25, 1.5, 2];
  let si = 0;
  const draw = () => {
    if (audioEl !== audio) return;
    const dur = Number.isFinite(audio.duration) ? audio.duration : 0;
    const position = audio.currentTime;
    const pct = dur ? (position / dur) * 100 : 0;
    const track = $<HTMLInputElement>("#pTrack");
    track.disabled = !dur; track.max = String(dur); track.value = String(position);
    track.style.setProperty("--played", `${pct}%`);
    track.setAttribute("aria-valuetext", fmt(position * 1000));
    $("#pTime").textContent = `${fmt(position * 1000)} / ${dur ? fmt(dur * 1000) : "--:--"}`;
    $("#pPlay").innerHTML = audio.paused ? ICON.play : ICON.pause;
    $("#pPlay").setAttribute("aria-label", audio.paused ? t("播放录音", "Play recording") : t("暂停录音", "Pause recording"));
    highlightTurn(position * 1000);
  };
  audio.ontimeupdate = draw;
  audio.onloadedmetadata = draw;
  audio.onplay = draw;
  audio.onpause = draw;
  $("#pPlay").onclick = () => { if (audio.paused) void audio.play().catch(error => toast(errorMessage(error), true)); else audio.pause(); };
  audio.onerror = () => { if (audioEl === audio) toast(t("录音暂时无法播放，请稍后重试", "Recording unavailable. Please try again."), true); };
  const speed = $("#pSpeed");
  speed.onclick = () => {
    si = (si + 1) % speeds.length;
    audio.playbackRate = speeds[si];
    speed.textContent = speeds[si] + "×";
    speed.setAttribute("aria-label", t(`播放速度 ${speeds[si]} 倍`, `Playback speed ${speeds[si]} times`));
  };
  $<HTMLInputElement>("#pTrack").oninput = (event) => {
    audio.currentTime = Number((event.currentTarget as HTMLInputElement).value);
  };
  draw();
}

export function seek(ms: number): void {
  if (!audioEl) return toast(t("这场会议没有录音文件", "This meeting has no recording"), true);
  const audio = audioEl;
  const jump = () => {
    if (audioEl !== audio) return;
    audio.currentTime = Math.max(0, Math.min(ms / 1000, Number.isFinite(audio.duration) ? audio.duration : ms / 1000));
    void audio.play().catch(error => toast(errorMessage(error), true));
  };
  if (audio.readyState >= 1) jump();
  else { audio.addEventListener("loadedmetadata", jump, { once: true }); audio.load(); }
}

let lastHi = -1;
function highlightTurn(ms: number): void {
  const turns = $$(".turn");
  if (!turns.length) return;
  let idx = -1;
  for (const t of turns) {
    if (Number(t.dataset.ms) <= ms + 200) idx = Number(t.dataset.i);
    else break;
  }
  if (idx === lastHi) return;
  lastHi = idx;
  turns.forEach((t) => t.classList.toggle("playing", Number(t.dataset.i) === idx && !!audioEl && !audioEl.paused));
}

export function setupPlayerKeys(): void {
  document.addEventListener("keydown", (e) => {
    const focused = document.activeElement as HTMLElement | null;
    if (e.code !== "Space" || e.repeat || !audioEl || focused?.closest("input, textarea, select, button, a, summary, [contenteditable=true], [role=tab]") || $maybe("#modal:not([hidden])")) return;
    e.preventDefault();
    if (audioEl.paused) void audioEl.play().catch(error => toast(errorMessage(error), true));
    else audioEl.pause();
  });
}
