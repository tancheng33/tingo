import { api, type MeetingSummary } from "../api";
import { isEn, speakerName, t } from "../i18n";
import { $, $$, $maybe, color, errorMessage, esc, fmt, ICON, initial, main, toast } from "../dom";
import { allTags, batch, type BatchAction, confirmBox, editTags, fmtBytes } from "../manage";
import { searchShortcut } from "../shell";
import { refreshMeetings } from "../sidebar";
import { S } from "../state";

/* 列表状态：看全部还是已归档、按哪个标签筛、是否在多选 */
const view = { archived: false, tag: "", selecting: false, selected: new Set<string>() };

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
    const q = search.value.trim();
    if (!q) return drawMeetings("");
    timer = window.setTimeout(() => runSearch(q), 250);
  };
  refreshMeetings().then(() => drawMeetings(search.value.trim()));
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
  $("#mSelect").onclick = () => {
    view.selecting = !view.selecting;
    view.selected.clear();
    redraw();
  };
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
  bar.innerHTML = `
    <span class="batch-n">${t(`已选 ${n} 场`, `${n} selected`)}</span>
    <button class="btn" data-b="tag" ${dis}>${t("加标签", "Add tag")}</button>
    <button class="btn" data-b="${view.archived ? "unarchive" : "archive"}" ${dis}>${view.archived ? t("取消归档", "Unarchive") : t("归档", "Archive")}</button>
    <button class="btn" data-b="delete_audio" ${dis}>${t("只删录音", "Delete recordings")}</button>
    <button class="btn danger" data-b="delete" ${dis}>${t("删除", "Delete")}</button>`;
  $$<HTMLButtonElement>("button", bar).forEach((b) => (b.onclick = () => runBatch(b.dataset.b ?? "")));
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
  if (kind === "delete" && !(await confirmBox(t(`删除 ${ids.length} 场会议？`, `Delete ${ids.length} meetings?`), t("录音、转写和纪要都会移到废纸篓，需要的话可以从废纸篓恢复。", "Recordings, transcripts and minutes move to the Trash — you can restore them from there."), t("移到废纸篓", "Move to Trash"))))
    return;
  if (
    kind === "delete_audio" &&
    !(await confirmBox(
      t(`删除 ${ids.length} 场会议的录音？`, `Delete the recordings of ${ids.length} meetings?`),
      t("只把录音移到废纸篓，转写和纪要都保留，以后就不能回听了。", "Only the recordings move to the Trash; transcripts and minutes stay, but you can't play them back."),
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

function drawMeetings(q: string): void {
  const box = $maybe("#mList");
  if (!box) return;
  drawTools();
  drawBatch();
  const items = S.meetings.filter(
    (m) =>
      !!m.archived === view.archived &&
      (!view.tag || (m.tags ?? []).includes(view.tag)) &&
      (!q || (m.title + m.summary + m.speakers.join("") + (m.tags ?? []).join("")).includes(q)),
  );
  const root = S.env ? S.env.root.replace(/^\/Users\/[^/]+/, "~") : "~/Documents/tingo";
  $("#mCount").textContent = S.meetings.length ? t(`共 ${S.meetings.length} 场，存放在 ${root}`, `${S.meetings.length} meetings, stored in ${root}`) : "";
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
  if (view.selecting)
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
  <a class="card m-card ${view.selecting ? "selecting" : ""} ${view.selected.has(m.id) ? "picked" : ""}" data-id="${esc(m.id)}" href="#/m/${encodeURIComponent(m.id)}" style="animation-delay:${Math.min(i, 8) * 30}ms">
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
  </a>`;
