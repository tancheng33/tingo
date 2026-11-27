/* 导出 PDF：用纪要渲染器排出一份独立的 HTML（打印样式），交给后端用系统 WebKit 转成 A4 PDF。
   鱼骨图仅导出问题、分支和说明，不附带引用原话。 */

import { docIsEn } from "./i18n";
import type { MeetingDetail } from "./api";
import { esc } from "./dom";
import { fishboneToText, stripTerms } from "./fishbone";
import { renderMarkdown } from "./markdown";

const CSS = `
@page { size: A4; }
* { box-sizing: border-box; }
html { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body { margin: 0; font: 14px/1.8 -apple-system, "PingFang SC", "Helvetica Neue", "Segoe UI", "Microsoft YaHei", sans-serif; color: #1d1d1f; }
.head { display: flex; align-items: center; gap: 10px; padding-bottom: 14px; border-bottom: 1px solid #e9e9ed; margin-bottom: 22px; }
.head svg { width: 22px; height: 22px; flex: none; }
.head .brand { font-weight: 600; font-size: 13px; letter-spacing: .3px; }
.head .brand small { font-weight: 500; color: #a3a7ad; margin-left: 6px; letter-spacing: 1px; }
.head .grow { flex: 1; }
.head .date { font-size: 11.5px; color: #a3a7ad; }
h1.title { font-size: 25px; line-height: 1.35; margin: 0 0 10px; font-weight: 650; letter-spacing: -.2px; }
.meta { display: flex; flex-wrap: wrap; gap: 6px 18px; font-size: 12.5px; color: #6b7076; margin-bottom: 26px; }
.meta b { font-weight: 500; color: #1d1d1f; margin-left: 4px; }
.md h2 { font-size: 16.5px; font-weight: 600; margin: 26px 0 8px; display: flex; align-items: center; gap: 8px; break-after: avoid; }
.md h2::before { content: ""; width: 4px; height: 15px; border-radius: 2px; background: linear-gradient(180deg, #34c6f4, #1f4bff); }
.md h3 { font-size: 14.5px; font-weight: 600; margin: 16px 0 4px; break-after: avoid; }
.md p { margin: 5px 0; }
.md ul, .md ol { margin: 5px 0; padding-left: 20px; }
.md li { margin: 2px 0; }
.md li::marker { color: #a3a7ad; }
.md code { font: 12.5px ui-monospace, Menlo, Consolas, monospace; background: #f2f2f5; padding: 1px 5px; border-radius: 5px; }
.md table { width: calc(100% - 1px); border-collapse: collapse; margin: 10px 0; font-size: 13px; }
.md th { text-align: left; font-weight: 500; color: #6b7076; background: #f6f7f9; font-size: 12px; }
.md th, .md td { padding: 7px 10px; border: 1px solid #e9e9ed; vertical-align: top; }
.md tr { break-inside: avoid; }
.ts { display: inline; border: none; background: #eef2ff; color: #1f4bff; font: 500 11px ui-monospace, Menlo, Consolas, monospace; padding: 0 5px; border-radius: 5px; margin: 0 2px; vertical-align: 1px; }
.fishbone { margin: 12px 0 4px; border: 1px solid #e9e9ed; border-radius: 12px; padding: 14px 16px; break-inside: avoid-page; }
.fishbone.pending, .fb-draw { display: none; }
.keep { break-inside: avoid-page; page-break-inside: avoid; }
.fb-head { display: flex; align-items: center; gap: 8px; }
.fb-head svg { width: 16px; height: 16px; fill: none; stroke: #1f4bff; stroke-width: 1.8; flex: none; }
.fb-problem { font-weight: 600; font-size: 14.5px; flex: 1; }
.fb-kind { font-size: 10.5px; color: #6b7076; background: #f2f2f5; padding: 1px 8px; border-radius: 99px; }
.fb-summary { margin: 2px 0 12px 24px; font-size: 11.5px; color: #6b7076; }
.fb-summary b { color: #b42318; font-weight: 500; }
.fb-body { position: relative; display: grid; grid-template-columns: 1fr 1fr; column-gap: 24px; row-gap: 8px; align-items: start; }
.fb-spine { z-index: 0; position: absolute; left: 50%; top: 4px; bottom: 4px; width: 2px; margin-left: -1px; background: #dfe5f5; }
.fb-branch { position: relative; background: #f6f7f9; border-radius: 9px; padding: 8px 10px; break-inside: avoid; }
.fb-branch.right { margin-top: 14px; }
.fb-branch::after { z-index: 1; content: ""; position: absolute; top: 13px; width: 7px; height: 7px; border-radius: 50%; background: #1d1d1f; }
.fb-branch.left::after { right: -16px; }
.fb-branch.right::after { left: -16px; }
.fb-branch.lv-none { background: #fff6f5; }
.fb-branch.lv-none::after { background: #f04438; }
.fb-title { font-weight: 600; font-size: 13px; line-height: 1.45; }
.fb-detail { color: #6b7076; font-size: 12px; line-height: 1.5; margin-top: 1px; }
.fb-src { display: inline-block; margin-top: 5px; font-size: 10.5px; padding: 0 7px; border-radius: 99px; background: #fff; color: #6b7076; border: 1px solid #e9e9ed; }
.fb-src.weak { color: #b54708; background: #fffaeb; border-color: #fedf89; }
.fb-src.none { color: #b42318; background: #fee4e2; border-color: #fecdca; }
.foot { margin-top: 34px; padding-top: 12px; border-top: 1px solid #e9e9ed; font-size: 11px; color: #a3a7ad; display: flex; justify-content: space-between; }
`;

/* 打印前把鱼骨图和它上面的小标题包进一个不拆页的容器：标题不会单独留在上一页底部。
   （对标题设 break-after / page-break-after: avoid 在 WebKit 打印里不生效，"整块不拆"是生效的。） */
const KEEP_TOGETHER = `
  document.querySelectorAll(".md h2").forEach((h) => {
    const next = h.nextElementSibling;
    if (!next || !next.classList.contains("fishbone")) return;
    const box = document.createElement("div");
    box.className = "keep";
    h.before(box);
    box.append(h, next);
  });
`;

/** 导出 Word 用的 HTML：macOS 的 textutil 只认简单的排版，鱼骨图换成文字列表，样式只留字号、粗细和表格边框 */
export function wordDocument(d: MeetingDetail): string {
  const en = docIsEn(d.minutes);
  const L = (zh: string, e: string) => (en ? e : zh);
  const md = fishboneToText(d.minutes);
  const body = renderMarkdown(md, { skipHead: true, en }).replace(/<button class="ts"[^>]*>([^<]*)<\/button>/g, "[$1]");
  const speakers = d.speaker_list
    .map((s) => (en ? s.name.replace(/^说话人(\d+)$/, "Speaker $1") : s.name))
    .join(en ? ", " : "、");
  const css = `body{font-family:"PingFang SC",Helvetica,sans-serif;font-size:11pt;line-height:1.6}h1{font-size:20pt}h2{font-size:14pt;margin-top:16pt}h3{font-size:12pt}
    table{border-collapse:collapse}td,th{border:1px solid #bbb;padding:4pt 6pt;vertical-align:top}th{background:#f2f2f5}.meta{color:#555}`;
  return `<!doctype html><html><head><meta charset="utf-8"><title>${esc(d.title)}</title><style>${css}</style></head><body>
  <h1>${esc(d.title)}</h1>
  <p class="meta">${L("日期", "Date")}：${esc(d.date)} ${esc(d.time)}${d.duration ? `　${L("时长", "Duration")}：${esc(d.duration)}` : ""}${speakers ? `　${L("参会人", "Participants")}：${esc(speakers)}` : ""}</p>
  ${body}
  <p class="meta">${L("由 Tingo 听过 生成", "Generated by Tingo")}</p>
</body></html>`;
}

/** 生成导出用的完整 HTML 文档 */
export function minutesDocument(d: MeetingDetail): string {
  const mark = document.querySelector(".brand-mark svg")?.outerHTML ?? "";
  const en = docIsEn(d.minutes); // 导出的文件跟纪要语言走
  const L = (zh: string, e: string) => (en ? e : zh);
  const body = renderMarkdown(stripTerms(d.minutes), {
    skipHead: true,
    en,
  });
  const speakers = d.speaker_list
    .map((s) => (en ? s.name.replace(/^说话人(\d+)$/, "Speaker $1") : s.name))
    .join(en ? ", " : "、");
  const now = new Date();
  const stamp = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
  return `<!doctype html><html lang="${en ? "en" : "zh-CN"}"><head><meta charset="utf-8"><title>${esc(d.title)}</title><style>${CSS}</style></head><body>
  <div class="head">${mark}<span class="brand">Tingo<small>${L("听过", "")}</small></span><span class="grow"></span><span class="date">${L("会议纪要", "Meeting minutes")}</span></div>
  <h1 class="title">${esc(d.title)}</h1>
  <div class="meta">
    <span>${L("日期", "Date")}<b>${esc(d.date)} ${esc(d.time)}</b></span>
    ${d.duration ? `<span>${L("时长", "Duration")}<b>${esc(d.duration)}</b></span>` : ""}
    ${speakers ? `<span>${L("参会人", "Participants")}<b>${esc(speakers)}</b></span>` : ""}
  </div>
  <div class="md">${body}</div>
  <script>${KEEP_TOGETHER}</script>
  <div class="foot"><span>${L("由 Tingo 听过 生成 · 录音在本机转写，每条结论后的时间戳对应录音里的原话", "Generated by Tingo · transcribed on-device; each timestamp points to the original words in the recording")}</span><span>${stamp}</span></div>
</body></html>`;
}
