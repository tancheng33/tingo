/* 纪要渲染：只覆盖纪要用到的 Markdown 子集（标题、列表、表格、粗体、行内代码），时间戳变成可点的按钮。 */

import { t, tl } from "./i18n";
import { esc, tsToMs } from "./dom";
import { parseFishbone, parseTerms, renderFishbone, type Resolver } from "./fishbone";

const TS = String.raw`\d{1,2}:\d{2}(?::\d{2})?`;

function inline(s: string): string {
  return esc(s)
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(new RegExp(String.raw`\s*[（(]\s*\[(${TS})\]\s*[）)]`, "g"), "[$1]")
    .replace(new RegExp(String.raw`\[(${TS})\]`, "g"), (_, t: string) => `<button class="ts" data-ms="${tsToMs(t)}">${t}</button>`);
}

const LIST = /^\s*([-*]|\d+\.)\s+/;

export function renderMarkdown(
  md: string,
  { skipHead = false, resolve, en }: { skipHead?: boolean; resolve?: Resolver; en?: boolean } = {},
): string {
  const lines = md.replace(/\r/g, "").split("\n");
  const out: string[] = [];
  let i = 0;
  let afterH1 = false;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    if (line.trim().startsWith("```")) {
      // 代码块：fishbone 渲染成鱼骨图；生成中还没闭合时先给个占位
      const lang = line.trim().slice(3).trim();
      const body: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith("```")) body.push(lines[i++]);
      const closed = i < lines.length;
      i++;
      if (lang === "terms") {
        // 人名与术语：没写完时不显示
        const terms = closed ? parseTerms(body.join("\n")) : [];
        if (terms.length)
          out.push(`<div class="terms"><div class="terms-head"><b>${tl(en, "人名与术语", "Names & terms")}</b>${
            en === undefined ? `<button class="link-btn terms-add" type="button">${t("全部加入热词", "Add all to hotwords")}</button>` : ""
          }</div><div class="terms-list">${terms
            .map(
              (x) =>
                `<span class="term" data-term="${esc(x.term)}"><b>${esc(x.term)}</b>${
                  x.variants?.length ? `<small>${tl(en, "识别成", "heard as")} ${esc(x.variants.join(tl(en, "、", ", ")))}</small>` : ""
                }</span>`,
            )
            .join("")}</div></div>`);
      } else if (lang === "fishbone") {
        const spec = closed ? parseFishbone(body.join("\n")) : null;
        out.push(spec ? renderFishbone(spec, resolve, en) : closed ? "" : `<div class="fishbone pending">${t("正在画鱼骨图…", "Drawing the fishbone…")}</div>`);
      } else {
        out.push(`<pre><code>${esc(body.join("\n"))}</code></pre>`);
      }
      afterH1 = false;
      continue;
    }
    const h = /^(#{1,3})\s+(.*)$/.exec(line);
    if (h) {
      // 详情页的标题和"日期/时长/参会人"已经在页头显示，跳过
      if (!(skipHead && h[1].length === 1)) out.push(`<h${h[1].length}>${inline(h[2])}</h${h[1].length}>`);
      afterH1 = h[1].length === 1;
      i++;
      continue;
    }
    if (line.trim().startsWith("|")) {
      const rows: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith("|")) rows.push(lines[i++]);
      const cells = (r: string) =>
        r
          .trim()
          .replace(/^\||\|$/g, "")
          .split("|")
          .map((c) => c.trim());
      const [head, ...rest] = rows.filter((r, k) => !(k === 1 && /^[\s|:-]+$/.test(r)));
      // 每格带上列名：手机上表格改成一行一张卡片时用
      const labels = cells(head).map((c) => esc(c.replace(/[*_`]/g, "")));
      out.push(
        `<table><thead><tr>${cells(head)
          .map((c) => `<th>${inline(c)}</th>`)
          .join("")}</tr></thead><tbody>${rest
          .map((r) => `<tr>${cells(r).map((c, k) => `<td data-label="${labels[k] ?? ""}">${inline(c)}</td>`).join("")}</tr>`)
          .join("")}</tbody></table>`,
      );
      afterH1 = false;
      continue;
    }
    if (LIST.test(line)) {
      const ordered = /^\s*\d+\./.test(line);
      const items: string[] = [];
      while (i < lines.length && LIST.test(lines[i])) items.push(lines[i++].replace(LIST, ""));
      const meta = afterH1 && items.some((x) => /^(日期：|Date:)/.test(x));
      const tag = ordered ? "ol" : "ul";
      if (!(skipHead && meta)) out.push(`<${tag}>${items.map((t) => `<li>${inline(t)}</li>`).join("")}</${tag}>`);
      afterH1 = false;
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i].trim() && !/^(#{1,3}\s|\s*\||\s*([-*]|\d+\.)\s|\s*```)/.test(lines[i])) para.push(lines[i++]);
    out.push(`<p>${inline(para.join(" "))}</p>`);
    afterH1 = false;
  }
  return out.join("\n");
}
