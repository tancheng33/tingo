import { trashEn, trashZh } from "../platform";
import { api, isIOS, type Job, type MeetingSummary, meetingPath } from "../api";
import { isEn, speakerName, t } from "../i18n";
import { $, $$, $maybe, closeModal, color, errorMessage, esc, fmt, ICON, initial, main, openDialog, toast } from "../dom";
import { allTags, batch, type BatchAction, confirmBox, deletesToRecent, editTags, fmtBytes } from "../manage";
import { poll } from "../poll";
import { searchShortcut } from "../shell";
import { refreshMeetings } from "../sidebar";
import { jobOf, S } from "../state";
import { bindSwipeRows, unbindSwipeRows } from "../swipe-rows";

/* 列表状态：看全部还是已归档、按哪个标签筛、是否在多选 */
const view = { archived: false, tag: "", selecting: false, selected: new Set<string>(), visible: [] as string[] };

export function renderMeetings(): void {
  view.selecting = false;
  view.selected.clear();
  main().innerHTML = `
    <div class="page">
      <div class="page-head">
        <div><h1>${t("会议<em>.</em>记录", "Meetings<em>.</em>")}</h1><div class="sub" id="mCount"></div></div>
        <span class="grow"></span>
        <label class="search">${ICON.search}<input class="input" id="mSearch" placeholder="${t("搜索会议里说过的话", "Search everything that was said")}"><kbd aria-hidden="true">${searchShortcut()}</kbd></label>
      </div>
      <div class="m-tools" id="mTools"></div>
      <div id="mList"></div>
      <div class="batch-bar" id="mBatch" hidden></div>
    </div>`;
  const search = $<HTMLInputElement>("#mSearch");
  let timer: number | undefined;
  search.oninput = () => {
    window.clearTimeout(timer);
    ++searchSeq;
    const q = search.value.trim();
    if (!q) return drawMeetings("");
    timer = window.setTimeout(() => runSearch(q), 250);
  };
  refreshMeetings().then(() => { if (search.isConnected) drawMeetings(search.value.trim()); });
  drawMeetings("");
}

function drawTools(): void {
  const box = $maybe("#mTools");
  if (!box) return;
  const archivedCount = S.meetings.filter((m) => m.archived).length;
  const tags = allTags();
  box.innerHTML = `
    <div class="seg fit" id="mView">
      <button data-v="all" class="${view.archived ? "" : "on"}">${t("全部", "All")}</button>
      <button data-v="archived" class="${view.archived ? "on" : ""}">${t("已归档", "Archived")}${archivedCount ? ` ${archivedCount}` : ""}</button>
    </div>
    <div class="m-tags">${tags
      .map((x) => `<button class="tag-chip ${view.tag === x.tag ? "on" : ""}" data-t="${esc(x.tag)}">${esc(x.tag)}<small>${x.count}</small></button>`)
      .join("")}</div>
    <span class="grow"></span>
    <button class="btn ${view.selecting ? "primary" : ""}" id="mSelect">${view.selecting ? t("完成", "Done") : t("选择", "Select")}</button>`;
  $$<HTMLButtonElement>("#mView button").forEach(
    (b) =>
      (b.onclick = () => {
        view.archived = b.dataset.v === "archived";
        view.selected.clear();
        redraw();
      }),
  );
  $$<HTMLButtonElement>(".m-tags .tag-chip").forEach(
    (b) =>
      (b.onclick = () => {
        view.tag = view.tag === b.dataset.t ? "" : (b.dataset.t ?? "");
        redraw();
      }),
  );
  $("#mSelect").onclick = () => setSelecting(!view.selecting);
}

/** 进出多选：手机上底部标签栏换成批量操作栏（见 style.css 的 .page.selecting） */
function setSelecting(on: boolean, first?: string): void {
  view.selecting = on;
  view.selected.clear();
  if (on && first) view.selected.add(first);
  redraw();
}

/** 从详情页点标签过来：按这个标签筛 */
export function showTag(tag: string): void {
  view.tag = tag;
  view.archived = false;
  location.hash = "#/meetings";
}

const redraw = () => drawMeetings(($maybe<HTMLInputElement>("#mSearch")?.value ?? "").trim());

function drawBatch(): void {
  const bar = $maybe("#mBatch");
  if (!bar) return;
  bar.hidden = !view.selecting;
  if (!view.selecting) return;
  const n = view.selected.size;
  const dis = n ? "" : "disabled";
  const all = view.visible.length > 0 && view.visible.every((id) => view.selected.has(id));
  // 桌面上 .batch-head / .batch-actions 是 display: contents，还是一整行
  bar.innerHTML = `
    <div class="batch-head"><span class="batch-n">${n ? t(`已选 ${n} 场`, `${n} selected`) : t("选择会议", "Select meetings")}</span>
    <button class="btn ghost batch-all" data-b="all" ${view.visible.length ? "" : "disabled"}>${all ? t("取消全选", "Deselect all") : t("全选", "Select all")}</button></div>
    <div class="batch-actions">
    <button class="btn" data-b="tag" ${dis}>${ICON.spark}<span>${t("加标签", "Add tag")}</span></button>
    ${S.env?.merge ? `<button class="btn" data-b="merge" ${n >= 2 ? "" : "disabled"} title="${t("同一场会被打断后录成了几段时，按时间先后接成一场", "Join parts of one interrupted meeting, in time order")}">${ICON.redo}<span>${t("合并", "Merge")}</span></button>` : ""}
    <button class="btn" data-b="${view.archived ? "unarchive" : "archive"}" ${dis}>${ICON.folder}<span>${view.archived ? t("取消归档", "Unarchive") : t("归档", "Archive")}</span></button>
    <button class="btn" data-b="delete_audio" ${dis}>${ICON.mic}<span>${t("只删录音", "Delete recordings")}</span></button>
    <button class="btn danger" data-b="delete" ${dis}>${ICON.trash}<span>${t("删除", "Delete")}</span></button></div>`;
  $$<HTMLButtonElement>("button", bar).forEach(
    (b) => (b.onclick = () => (b.dataset.b === "all" ? toggleAll() : b.dataset.b === "merge" ? confirmMerge() : runBatch(b.dataset.b ?? ""))),
  );
}

function toggleAll(): void {
  const all = view.visible.every((id) => view.selected.has(id));
  view.visible.forEach((id) => (all ? view.selected.delete(id) : view.selected.add(id)));
  $$<HTMLElement>("#mList .m-card").forEach((a) => a.classList.toggle("picked", view.selected.has(a.dataset.id ?? "")));
  drawBatch();
}

async function runBatch(kind: string): Promise<void> {
  const ids = [...view.selected];
  if (!ids.length) return;
  let action = kind as BatchAction;
  let tag: string | undefined;
  if (kind === "tag") {
    const tags = await editTags([], t(`给 ${ids.length} 场会议加标签`, `Tag ${ids.length} meetings`));
    if (!tags?.length) return;
    action = "tag_add";
    tag = tags[0];
    for (const extra of tags.slice(1)) await batch(ids, "tag_add", extra).catch(() => {});
  }
  if (kind === "delete" && !(await confirmBox(t(`删除 ${ids.length} 场会议？`, `Delete ${ids.length} meetings?`), trashNote(), deletesToRecent() ? t("删除会议", "Delete meetings") : t(`移到${trashZh()}`, `Move to ${trashEn()}`))))
    return;
  if (
    kind === "delete_audio" &&
    !(await confirmBox(
      t(`删除 ${ids.length} 场会议的录音？`, `Delete the recordings of ${ids.length} meetings?`),
      isIOS()
        ? t("只把录音移到 Tingo 文件夹的“已删除录音”，转写和纪要都保留，以后就不能回听了。", "Only the recordings move to “已删除录音” in the Tingo folder; transcripts and minutes stay, but you can't play them back.")
        : t(`只把录音移到${trashZh()}，转写和纪要都保留，以后就不能回听了。`, `Only the recordings move to the ${trashEn()}; transcripts and minutes stay, but you can't play them back.`),
      t("删除录音", "Delete recordings"),
    ))
  )
    return;
  try {
    const r = await batch(ids, action, tag);
    const freed = r.freed_bytes ? t(`，腾出 ${fmtBytes(r.freed_bytes)}`, `, freed ${fmtBytes(r.freed_bytes)}`) : "";
    if (r.failed.length) toast(t(`${r.ok} 场完成，${r.failed.length} 场失败：${r.failed[0].error}`, `${r.ok} done, ${r.failed.length} failed: ${r.failed[0].error}`), true);
    else toast(t(`已处理 ${r.ok} 场${freed}`, `Done for ${r.ok}${freed}`));
    view.selected.clear();
    if (kind === "delete" || kind === "archive" || kind === "unarchive") view.selecting = false;
    await refreshMeetings();
    redraw();
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

/** iOS 没有系统废纸篓，删掉的会议在 App 里的「最近删除」 */
const trashNote = () =>
  deletesToRecent()
    ? t("录音、转写和纪要会移到「最近删除」，可以随时恢复。", "Recordings, transcripts and minutes move to Recently deleted — you can restore them anytime.")
    : t(`录音、转写和纪要都会移到${trashZh()}，需要的话可以从${trashZh()}恢复。`, `Recordings, transcripts and minutes move to the ${trashEn()} — you can restore them from there.`);

function drawMeetings(q: string): void {
  const box = $maybe("#mList");
  if (!box) return;
  const items = S.meetings.filter(
    (m) =>
      !!m.archived === view.archived &&
      (!view.tag || (m.tags ?? []).includes(view.tag)) &&
      (!q || (m.title + m.summary + m.speakers.join("") + (m.tags ?? []).join("")).includes(q)),
  );
  view.visible = items.map((m) => m.id);
  box.closest(".page")?.classList.toggle("selecting", view.selecting);
  drawTools();
  drawBatch();
  const root = isIOS()
    ? t("「文件」→ 我的 iPhone → Tingo", "Files → On My iPhone → Tingo")
    : S.env
      ? S.env.root.replace(/^\/Users\/[^/]+/, "~")
      : "~/Documents/tingo";
  $("#mCount").textContent = S.meetings.length
    ? t(`共 ${S.meetings.length} 场，存放在 ${root}`, `${S.meetings.length} meetings, stored in ${root}`)
    : "";
  if (!S.meetings.length) {
    box.innerHTML = `<div class="empty"><h3>${t("还没有会议", "No meetings yet")}</h3><div>${t("开一场会，或者把已有的录音拖进窗口", "Start a meeting, or drop an existing recording into the window")}</div><a class="btn primary" href="#/">${t("开始录音", "Start recording")}</a></div>`;
    return;
  }
  if (!items.length) {
    box.innerHTML = view.archived
      ? `<div class="empty"><h3>${t("没有归档的会议", "No archived meetings")}</h3><div>${t("开完的会可以归档，不占会议列表的位置，搜索时照样能搜到。", "Archive finished meetings to keep the list tidy — they still show up in search.")}</div></div>`
      : `<div class="empty"><h3>${t("没有找到", "Nothing found")}</h3><div>${t("换个关键词或标签试试", "Try another keyword or tag")}</div></div>`;
    return;
  }
  const groups: Record<string, MeetingSummary[]> = {};
  for (const m of items) (groups[m.date.slice(0, 7)] ||= []).push(m);
  box.innerHTML = Object.entries(groups)
    .map(
      ([ym, ms]) => `
    <div class="month">${isEn() ? new Date(Number(ym.slice(0, 4)), Number(ym.slice(5)) - 1, 1).toLocaleDateString("en-US", { year: "numeric", month: "long" }) : `${ym.slice(0, 4)} 年 ${Number(ym.slice(5))} 月`}</div>
    <div class="grid">${ms.map(card).join("")}</div>`,
    )
    .join("");
  if (!view.selecting) return bindCards(box);
  unbindSwipeRows(box);
  $$<HTMLAnchorElement>(".m-card", box).forEach(
    (a) =>
      (a.onclick = (e) => {
        e.preventDefault();
        const id = a.dataset.id ?? "";
        if (view.selected.has(id)) view.selected.delete(id);
        else view.selected.add(id);
        a.classList.toggle("picked", view.selected.has(id));
        drawBatch();
      }),
  );
}

/** 合并前确认：按时间先后列出选中的几场，可以改合并后的标题 */
function confirmMerge(): void {
  const parts = S.meetings
    .filter((m) => view.selected.has(m.id))
    .sort((a, b) => (a.date + a.time + a.id).localeCompare(b.date + b.time + b.id));
  if (parts.length < 2) return;
  if (parts.some((m) => jobOf(m.id))) {
    toast(t("有会议正在处理，完成后再合并", "Some of these meetings are still being processed — merge them afterwards"), true);
    return;
  }
  const untranscribed = parts.some((m) => !m.has_transcript);
  openDialog({
    title: t(`合并 ${parts.length} 场会议`, `Merge ${parts.length} meetings`),
    description: t(
      "按时间先后接成一场：录音接在一起，转写跟着拼接。原来的几场会移到“已删除会议”，不会立即抹除。",
      "They are joined in time order into one meeting: the recordings and the transcripts. The originals move to “已删除会议” and are not erased right away.",
    ),
    body: `
      <label for="mergeTitle">${t("合并后的标题", "Title of the merged meeting")}</label>
      <input class="input" id="mergeTitle" maxlength="80" value="${esc(parts[0].title)}">
      <ol class="merge-parts">${parts
        .map((m) => `<li><span class="when">${esc(m.date.slice(5))} ${esc(m.time)}</span><span class="name">${esc(m.title)}</span><span class="len">${esc(m.duration || t("未转写", "Not transcribed"))}</span></li>`)
        .join("")}</ol>
      <p class="merge-note">${
        untranscribed
          ? t("其中有还没转写的录音，合并后会整段重新转写，需要等一会儿。", "Some of these aren't transcribed yet, so the merged recording will be transcribed from scratch — this takes a while.")
          : t("说话人：改过名而且同名的算同一个人；没改过名的各段分开编号，合并后可以再改。纪要会按合并后的内容重新生成。", "Speakers: those renamed to the same name count as one person; unnamed ones stay separate per part and can be renamed afterwards. The minutes are written again from the merged meeting.")
      }</p>`,
    actions: `<button class="btn" data-close>${t("取消", "Cancel")}</button><span class="grow"></span><button class="btn primary" id="mergeConfirm">${t("合并", "Merge")}</button>`,
  });
  const confirm = $<HTMLButtonElement>("#mergeConfirm");
  confirm.onclick = async () => {
    confirm.disabled = true;
    try {
      const job = await api<Job>("/api/meetings/merge", {
        method: "POST",
        json: { ids: parts.map((m) => m.id), title: $<HTMLInputElement>("#mergeTitle").value.trim() },
      });
      S.jobs = [...S.jobs.filter((j) => j.id !== job.id), job];
      closeModal();
      view.selected.clear();
      view.selecting = false;
      redraw();
      toast(t("正在合并，完成后会提示", "Merging — you'll be told when it's done"));
      void poll();
    } catch (e) {
      toast(errorMessage(e), true);
      confirm.disabled = false;
    }
  };
}

interface SearchHit {
  ms: number;
  speaker: string;
  text: string;
  match: string;
}
interface SearchResult {
  id: string;
  title: string;
  date: string;
  time: string;
  minutes_hit: boolean;
  archived?: boolean;
  hits: SearchHit[];
}

let searchSeq = 0;

/* 跨会议搜索：后端会把术语表里记下的听错写法、只差一个字的写法一起搜出来。
   旧后端没有 /api/search 时退回按标题和摘要过滤。 */
async function runSearch(q: string): Promise<void> {
  const seq = ++searchSeq;
  let r: { expanded: string[]; results: SearchResult[] };
  try {
    r = await api(`/api/search?q=${encodeURIComponent(q)}`);
  } catch {
    if (seq === searchSeq) drawMeetings(q);
    return;
  }
  const box = $maybe("#mList");
  if (seq !== searchSeq || !box) return;
  const also = r.expanded.filter((x) => x.toLowerCase() !== q.toLowerCase());
  const head = `<div class="search-head">${t(`找到 ${r.results.length} 场会议`, `${r.results.length} meeting${r.results.length === 1 ? "" : "s"} found`)}${
    also.length ? ` · ${t("也搜了", "also searched")} ${also.map((x) => `<b>${esc(x)}</b>`).join(t("、", ", "))}` : ""
  }</div>`;
  if (!r.results.length) {
    box.innerHTML = head + `<div class="empty"><h3>${t("没有找到", "Nothing found")}</h3><div>${t("换个关键词试试", "Try another keyword")}</div></div>`;
    return;
  }
  const mark = (text: string, m: string) => {
    const i = m ? text.toLowerCase().indexOf(m.toLowerCase()) : -1;
    return i < 0 ? esc(text) : `${esc(text.slice(0, i))}<mark>${esc(text.slice(i, i + m.length))}</mark>${esc(text.slice(i + m.length))}`;
  };
  box.innerHTML =
    head +
    r.results
      .map(
        (m) => `
    <div class="card s-result">
      <a class="s-title" href="#/m/${encodeURIComponent(m.id)}"><b>${esc(m.title)}</b>${m.archived ? `<span class="arch-badge">${t("已归档", "Archived")}</span>` : ""}<span>${esc(m.date)} ${esc(m.time)}</span></a>
      ${m.hits
        .map(
          (h) => `<a class="s-hit" href="#/m/${encodeURIComponent(m.id)}?t=${h.ms}"><span class="ts">${fmt(h.ms)}</span><span class="who">${esc(speakerName(h.speaker))}</span><span class="txt">${mark(h.text, h.match)}</span></a>`,
        )
        .join("")}
      ${!m.hits.length && m.minutes_hit ? `<div class="s-note">${t("纪要里提到了", "Mentioned in the minutes")}</div>` : ""}
    </div>`,
      )
      .join("");
}

const card = (m: MeetingSummary, i: number) => `
  <div class="meeting-row" data-meeting="${esc(m.id)}">
    <div class="meeting-row-actions" aria-hidden="true" inert>
      <button class="row-edit" data-meeting-action="rename">${ICON.edit}<span>${t("改名", "Rename")}</span></button>
      <button class="row-delete" data-meeting-action="delete"><svg viewBox="0 0 24 24"><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 10v7M14 10v7"/></svg><span>${t("删除", "Delete")}</span></button>
    </div>
  <a draggable="false" class="card m-card ${view.selecting ? "selecting" : ""} ${view.selected.has(m.id) ? "picked" : ""}" data-id="${esc(m.id)}" href="#/m/${encodeURIComponent(m.id)}" style="animation-delay:${Math.min(i, 8) * 30}ms">
    ${view.selecting ? `<span class="pick-box"></span>` : ""}
    <div class="when"><span>${isEn() ? esc(new Date(`${m.date}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" })) : `${esc(m.date.slice(5).replace("-", " 月 "))} 日`} ${esc(m.time)}</span><span>${esc(m.duration)}</span></div>
    <h3>${esc(m.title)}</h3>
    ${m.tags?.length ? `<div class="card-tags">${m.tags.map((tg) => `<span class="tag-chip sm">${esc(tg)}</span>`).join("")}</div>` : ""}
    <p>${esc(m.summary || (m.has_transcript ? t("已转写，还没有生成纪要", "Transcribed — no minutes yet") : t("录音还没有处理", "Recording not processed yet")))}</p>
    <div class="foot">
      <span class="avatars">${m.speakers
        .slice(0, 4)
        .map((s, k) => `<span style="background:${color(k)}">${esc(initial(s))}</span>`)
        .join("")}</span>
      ${m.speakers.length ? `<span>${t(`${m.speakers.length} 位说话人`, `${m.speakers.length} speaker${m.speakers.length > 1 ? "s" : ""}`)}</span>` : ""}
      <span class="status ${m.has_minutes ? "ok" : m.has_transcript ? "half" : ""}">${m.has_minutes ? t("已出纪要", "Minutes") : m.has_transcript ? t("仅转写", "Transcript only") : t("未处理", "Not processed")}</span>
    </div>
  </a>
  <button class="meeting-more" aria-label="${t("会议操作", "Meeting actions")}" aria-expanded="false">•••</button>
  </div>`;

/** 首次直接进入详情时，返回底图也使用真实会议内容。 */
export function meetingsPreview(): string {
  return `<div class="page"><div class="page-head"><h1>${t("会议记录", "Meetings")}</h1></div><div class="grid">${S.meetings.map(card).join("")}</div></div>`;
}

function bindCards(box: HTMLElement): void {
  if (!isIOS()) return;
  // 按住一张卡片：进入多选并选中它
  bindSwipeRows(box, (row) => setSelecting(true, row.dataset.meeting));
  $$<HTMLButtonElement>("[data-meeting-action]", box).forEach(button => button.onclick = () => {
    const row = button.closest<HTMLElement>(".meeting-row")!;
    const meeting = S.meetings.find(m => m.id === row.dataset.meeting);
    if (!meeting) return;
    if (jobOf(meeting.id)) { toast(t("这场会议正在处理，完成后再操作", "Wait for this meeting to finish processing"), true); return; }
    const rename = button.dataset.meetingAction === "rename";
    openDialog({
      title: rename ? t("修改会议标题", "Rename meeting") : t("删除这场会议？", "Delete this meeting?"),
      description: rename ? t("只改显示标题，不改会议文件夹。留空恢复默认标题。", "The folder stays unchanged. Leave empty to restore the default title.") : t("录音、转写和纪要会移到「最近删除」，可随时在 App 内查看和恢复。", "The recording, transcript and minutes move to Recently deleted, where you can view and restore them."),
      body: rename ? `<label for="meetingTitle">${t("会议标题", "Meeting title")}</label><input class="input" id="meetingTitle" maxlength="80" value="${esc(meeting.title)}">` : `<p>${esc(meeting.title)}</p>`,
      actions: `<button class="btn" data-close>${t("取消", "Cancel")}</button><span class="grow"></span><button class="btn ${rename ? "primary" : "danger"}" id="meetingConfirm">${rename ? t("保存", "Save") : t("删除会议", "Delete meeting")}</button>`,
    });
    const confirm = $<HTMLButtonElement>("#meetingConfirm");
    confirm.onclick = async () => {
      if (jobOf(meeting.id)) { toast(t("这场会议正在处理，完成后再操作", "Wait for processing to finish"), true); return; }
      confirm.disabled = true;
      try {
        if (rename) await api(meetingPath(meeting.id, "/meta"), {method: "PUT", json: {title: $<HTMLInputElement>("#meetingTitle").value.trim() || null}});
        else await api(meetingPath(meeting.id), {method: "DELETE"});
        closeModal();
        await refreshMeetings();
        if (box.isConnected) drawMeetings(($maybe<HTMLInputElement>("#mSearch")?.value ?? "").trim());
        toast(rename ? t("已保存", "Saved") : t("已移到「最近删除」", "Moved to Recently deleted"));
      } catch (e) { toast(errorMessage(e), true); confirm.disabled = false; }
    };
  });
}
