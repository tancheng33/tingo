import { t } from "./i18n";
import { showUpdate, updateInfo } from "./update";
import { api, type Env, type MeetingSummary } from "./api";
import { $, $maybe, esc } from "./dom";
import { S } from "./state";

export async function refreshMeetings(): Promise<void> {
  try {
    S.meetings = await api<MeetingSummary[]>("/api/meetings");
  } catch {
    S.meetings = [];
  }
  const h = decodeURIComponent(location.hash.slice(1));
  $("#recentList").innerHTML = S.meetings.length
    ? S.meetings
        .filter((m) => !m.archived) // 归档的不放进"最近"
        .slice(0, 8)
        .map(
          (m) => `
        <a href="#/m/${encodeURIComponent(m.id)}" data-id="${esc(m.id)}" class="${h === "/m/" + m.id ? "active" : ""}">
          <div class="r-title">${esc(m.title)}</div>
          <div class="r-date">${esc(m.date.slice(5).replace("-", "/"))} ${esc(m.time)}${m.duration ? " · " + esc(m.duration) : ""}</div>
        </a>`,
        )
        .join("")
    : `<div class="r-empty">${t("还没有会议", "No meetings yet")}</div>`;
}

export async function refreshEnv(): Promise<void> {
  try {
    S.env = await api<Env>("/api/env");
  } catch {
    return;
  }
  const e = S.env;
  // [状态, 文字, 有问题时去哪个设置页]
  const rows: [string, string, string][] = [
    [
      e.model_loaded || e.model_cached ? "" : "warn",
      e.model_loaded
        ? t("识别模型已加载", "Speech model loaded")
        : e.model_cached
          ? t("识别模型已就绪", "Speech model ready")
          : e.models_downloadable
            ? t("识别模型未下载", "Speech model not downloaded")
            : t("首次转写会下载模型", "Model downloads on first use"),
      e.models_downloadable ? "asr" : "about",
    ],
    [e.llm_ready ? "" : "bad", e.llm_ready ? e.llm_model : t("未配置大模型", "No LLM configured"), "llm"],
    [e.system_audio ? "" : "warn", e.system_audio ? t("可录系统声音", "System audio available") : t("仅能录麦克风", "Microphone only"), "about"],
  ];
  const update = updateInfo()?.newer ? updateInfo() : null;
  $("#envBox").innerHTML =
    rows
      .map(([c, text, tab]) => {
        const body = `<span class="env-dot ${c}"></span><span>${esc(text)}</span>`;
        // 有问题的那行点了直接去对应的设置页
        return c ? `<a class="env-row" href="#/settings/${tab}">${body}</a>` : `<div class="env-row">${body}</div>`;
      })
      .join("") +
    `<div class="env-row env-ver">v${esc(e.version)}${
      update ? ` <a class="update-dot" id="upLink" href="#">${t(`新版本 ${esc(update.latest ?? "")}`, `Update ${esc(update.latest ?? "")}`)}</a>` : ""
    }</div>`;
  const up = document.getElementById("upLink");
  if (up)
    up.onclick = (ev) => {
      ev.preventDefault();
      showUpdate();
    };

  // 收成图标栏时：一切正常就不占位置；有问题才出一个提醒图标（颜色跟最严重的那项），点了去对应的设置页
  const worst = rows.find(([c]) => c === "bad") ?? rows.find(([c]) => c === "warn");
  const rail = $("#envRail") as HTMLAnchorElement;
  rail.hidden = !worst;
  if (worst) {
    rail.href = `#/settings/${worst[2]}`;
    rail.dataset.tip = rows.filter(([c]) => c).map(([, text]) => text).join(" · ");
    rail.innerHTML = `<span class="side-ico env-alert ${worst[0]}"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="M12 7.8v5M12 16v.2"/></svg></span><span class="side-label">${esc(worst[1])}</span>`;
  }
  $maybe('.side-item[data-nav="settings"]')?.classList.toggle("has-update", !!update);
}
