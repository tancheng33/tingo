/* 会议管理：改名、标签、归档、删除（都移到废纸篓）、批量操作。会议列表和详情页共用。 */

import { api, isIOS, type MeetingSummary, meetingPath } from "./api";
import { $, $$, closeModal, esc, openModal } from "./dom";
import { t } from "./i18n";
import { S } from "./state";

/** 整场删除的会议去哪：iOS 没有系统废纸篓，放进「最近删除」；Mac 上进系统废纸篓 */
export const deletesToRecent = (): boolean => isIOS();

export const fmtBytes = (n: number | null | undefined): string => {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${u[i]}`;
};

/** 所有会议用过的标签，按使用次数排 */
export function allTags(): { tag: string; count: number }[] {
  const m = new Map<string, number>();
  for (const x of S.meetings) for (const tg of x.tags ?? []) m.set(tg, (m.get(tg) ?? 0) + 1);
  return [...m.entries()].map(([tag, count]) => ({ tag, count })).sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
}

/** 确认弹窗（不用浏览器自带的 confirm：Tauri 窗口里不一定弹得出来） */
export function confirmBox(title: string, body: string, ok: string, danger = true): Promise<boolean> {
  return new Promise((resolve) => {
    openModal(`
      <h3>${title}</h3>
      <p class="hint">${body}</p>
      <div class="modal-foot"><button class="btn" id="cfNo">${t("取消", "Cancel")}</button><button class="btn ${danger ? "danger" : "primary"}" id="cfYes">${ok}</button></div>`);
    $("#cfNo").onclick = () => {
      closeModal();
      resolve(false);
    };
    $("#cfYes").onclick = () => {
      closeModal();
      resolve(true);
    };
  });
}

/** 编辑标签：现有标签点一下切换，也可以输入新标签 */
export function editTags(current: string[], title = t("编辑标签", "Edit tags")): Promise<string[] | null> {
  return new Promise((resolve) => {
    const sel = new Set(current);
    const known = [...new Set([...allTags().map((x) => x.tag), ...current])];
    openModal(`
      <h3>${title}</h3>
      <p class="hint">${t("用标签给会议分类，比如项目名、会议类型。会议列表可以按标签筛选。", "Use tags to group meetings — project names, meeting types. The meeting list can filter by tag.")}</p>
      <div class="tag-pick" id="tgPick"></div>
      <input class="input" id="tgNew" placeholder="${t("输入新标签，回车添加", "Type a new tag and press Enter")}" maxlength="20">
      <div class="modal-foot"><button class="btn" id="tgNo">${t("取消", "Cancel")}</button><button class="btn primary" id="tgOk">${t("保存", "Save")}</button></div>`);
    const draw = () => {
      $("#tgPick").innerHTML = known.length
        ? known.map((tg) => `<button class="tag-chip ${sel.has(tg) ? "on" : ""}" data-t="${esc(tg)}">${esc(tg)}</button>`).join("")
        : `<span class="hint">${t("还没有标签", "No tags yet")}</span>`;
      $$<HTMLButtonElement>("#tgPick .tag-chip").forEach(
        (b) =>
          (b.onclick = () => {
            const tg = b.dataset.t ?? "";
            if (sel.has(tg)) sel.delete(tg);
            else sel.add(tg);
            draw();
          }),
      );
    };
    draw();
    const input = $<HTMLInputElement>("#tgNew");
    input.onkeydown = (e) => {
      const v = input.value.trim();
      if (e.key === "Enter" && v) {
        e.preventDefault();
        if (!known.includes(v)) known.push(v);
        sel.add(v);
        input.value = "";
        draw();
      }
    };
    $("#tgNo").onclick = () => {
      closeModal();
      resolve(null);
    };
    $("#tgOk").onclick = () => {
      const v = input.value.trim();
      if (v) sel.add(v);
      closeModal();
      resolve(known.filter((x) => sel.has(x)).concat(v && !known.includes(v) ? [v] : []));
    };
  });
}

/** 改会议标题；留空恢复默认（纪要里的标题） */
export function renameBox(current: string): Promise<string | null | undefined> {
  return new Promise((resolve) => {
    openModal(`
      <h3>${t("修改会议标题", "Rename meeting")}</h3>
      <input class="input" id="rnTitle" value="${esc(current)}" maxlength="80">
      <p class="hint">${t("留空会恢复成纪要里的标题。不会改会议文件夹的名字。", "Leave empty to use the title from the minutes. The meeting folder isn't renamed.")}</p>
      <div class="modal-foot"><button class="btn" id="rnNo">${t("取消", "Cancel")}</button><button class="btn primary" id="rnOk">${t("保存", "Save")}</button></div>`);
    const input = $<HTMLInputElement>("#rnTitle");
    input.select();
    const ok = () => {
      closeModal();
      resolve(input.value.trim() || null);
    };
    input.onkeydown = (e) => {
      if (e.key === "Enter") ok();
    };
    $("#rnOk").onclick = ok;
    $("#rnNo").onclick = () => {
      closeModal();
      resolve(undefined);
    };
  });
}

export const setMeta = (id: string, patch: { title?: string | null; tags?: string[]; archived?: boolean }) =>
  api<MeetingSummary>(meetingPath(id, "/meta"), { method: "PUT", json: patch });

export const deleteMeeting = (id: string) => api(meetingPath(id), { method: "DELETE" });

export const deleteAudio = (id: string) => api<{ freed_bytes: number }>(meetingPath(id, "/audio"), { method: "DELETE" });

export type BatchAction = "archive" | "unarchive" | "tag_add" | "tag_remove" | "delete" | "delete_audio";

export const batch = (ids: string[], action: BatchAction, tag?: string) =>
  api<{ ok: number; failed: { id: string; error: string }[]; freed_bytes: number }>("/api/meetings/batch", { json: { ids, action, tag } });
