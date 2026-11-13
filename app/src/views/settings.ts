/* 设置：左边二级菜单，右边一次只显示一项——大模型（本机中转一键选择 + 远程服务 + 自定义）、识别模型（桌面版可下载切换）、
   热词、推送到群、Obsidian、运行环境。每一项单独保存。 */

import { api, type Config, type Job, type LocalProvider } from "../api";
import { $, $$, $maybe, errorMessage, esc, ICON, main, toast } from "../dom";
import { refreshEnv } from "../sidebar";
import { isBusy, S } from "../state";
import { applyLang, type LangPref, renderShell, t } from "../i18n";
import { checkUpdate, showUpdate } from "../update";
import { onboarded } from "./welcome";
import { confirmBox, deleteAudio, fmtBytes } from "../manage";
import { refreshMeetings } from "../sidebar";
import { refreshShell } from "../shell";

interface Preset {
  id: string;
  name: string;
  base_url?: string;
  model?: string;
  /** 接口协议，默认 openai */
  api?: "openai" | "anthropic";
  local?: LocalProvider;
}

// 本机中转（Magpie、CC Switch、Ollama）由服务端探测后补在前面；这里是远程服务
const remotePresets = (): Preset[] => [
  { id: "deepseek", name: "DeepSeek", base_url: "https://api.deepseek.com", model: "deepseek-chat" },
  { id: "bailian", name: t("通义千问（百炼）", "Qwen (Bailian)"), base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4.1-mini" },
  { id: "claude", name: "Claude", base_url: "https://api.anthropic.com/v1", model: "claude-sonnet-5", api: "anthropic" },
  { id: "gemini", name: "Gemini", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-2.5-flash" },
  { id: "custom", name: t("自定义", "Custom") },
];

/* 模型选择：点开显示全部模型（按服务商分组），输入时过滤。不用 <datalist>：它按输入框现有内容过滤，
   框里已经有模型名时就只剩这一个选项，等于没法选。返回一个刷新函数，模型列表异步到达后调用。 */
function bindModelPicker(getModels: () => string[], getRecommended: () => string): () => void {
  const input = $<HTMLInputElement>("#sModel");
  const pop = $("#sModelPop");
  let filter = "";
  let active = -1;
  let items: string[] = [];
  const close = () => {
    pop.hidden = true;
    active = -1;
  };
  const draw = () => {
    const q = filter.trim().toLowerCase();
    const all = getModels();
    items = all.filter((m) => !q || m.toLowerCase().includes(q));
    if (!all.length) {
      pop.innerHTML = `<div class="combo-empty">${t("这个接口没有返回模型列表，直接输入模型名即可", "This endpoint didn't return a model list — just type the model name")}</div>`;
      return;
    }
    if (!items.length) {
      pop.innerHTML = `<div class="combo-empty">${t(`没有匹配“${esc(filter)}”的模型`, `No model matches “${esc(filter)}”`)}</div>`;
      return;
    }
    const groups: Record<string, string[]> = {};
    for (const m of items) (groups[m.includes("/") ? m.split("/")[0] : t("其他", "Other")] ||= []).push(m);
    let i = 0;
    pop.innerHTML = Object.entries(groups)
      .map(
        ([g, ms]) => `
      <div class="combo-group">${esc(g)}</div>
      ${ms
        .map(
          (m) => `<div class="combo-item ${m === input.value ? "sel" : ""} ${i === active ? "act" : ""}" data-i="${i++}">
        <span>${esc(m.includes("/") ? m.slice(g.length + 1) : m)}</span>${m === getRecommended() ? `<em>${t("推荐", "Recommended")}</em>` : ""}</div>`,
        )
        .join("")}`,
      )
      .join("");
    ($maybe(".combo-item.act", pop) ?? $maybe(".combo-item.sel", pop))?.scrollIntoView({ block: "nearest" });
  };
  const open = () => {
    filter = "";
    active = -1;
    pop.hidden = false;
    draw();
    input.select(); // 选中现有内容：直接打字就是搜索，不会接在原来的模型名后面
  };
  const pick = (m: string) => {
    input.value = m;
    close();
    input.dispatchEvent(new Event("change"));
  };
  input.addEventListener("focus", open);
  input.addEventListener("click", () => {
    if (pop.hidden) open();
  });
  input.addEventListener("input", () => {
    filter = input.value;
    active = -1;
    pop.hidden = false;
    draw();
  });
  input.addEventListener("keydown", (e) => {
    if (pop.hidden) return;
    if (e.key === "ArrowDown") {
      active = Math.min(items.length - 1, active + 1);
      draw();
      e.preventDefault();
    } else if (e.key === "ArrowUp") {
      active = Math.max(0, active - 1);
      draw();
      e.preventDefault();
    } else if (e.key === "Enter" && active >= 0) {
      pick(items[active]);
      e.preventDefault();
    } else if (e.key === "Escape") {
      close();
      e.stopPropagation();
    }
  });
  pop.addEventListener("mousedown", (e) => {
    e.preventDefault(); // 不让输入框先失焦把列表关掉
    const it = (e.target as HTMLElement).closest<HTMLElement>(".combo-item");
    if (it) pick(items[Number(it.dataset.i)]);
  });
  input.addEventListener("blur", () => setTimeout(close, 100));
  return () => {
    if (!pop.hidden) draw();
  };
}

interface AsrModel {
  id: string;
  name: string;
  desc: string;
  size_mb: number;
  ready: boolean;
  active: boolean;
  /** 比如会议语言选了英文、当前是只认中文的 Paraformer */
  warning?: string;
  system?: boolean;
  available?: boolean;
}

const delArmed = new Set<string>(); // 删除要点两次

/* 识别模型卡片：只有桌面版（Rust 后端）有 /api/models，Python 版拿不到就不显示。 */
async function drawAsr(): Promise<void> {
  const box = $maybe("#sAsr");
  if (!box) return;
  let list: AsrModel[];
  try {
    list = (await api<{ models: AsrModel[] }>("/api/models")).models;
  } catch {
    box.hidden = true;
    return;
  }
  const job = S.job;
  const dlId = job?.kind === "models" && isBusy(job) ? job.meeting || "paraformer" : "";
  const action = (m: AsrModel) => {
    if (m.system && m.available === false) return `<span class="asr-badge">${t("不可用", "Unavailable")}</span>`;
    if (m.active && m.ready) return `<span class="asr-badge">${t("使用中", "In use")}</span>`;
    if (m.ready)
      return `${m.id !== "paraformer" && !m.system ? `<button class="btn ghost asr-del" data-del="${m.id}">${delArmed.has(m.id) ? t("确认删除", "Confirm") : t("删除", "Delete")}</button>` : ""}<button class="btn" data-use="${m.id}">${t("使用", "Use")}</button>`;
    if (m.id === dlId) return `<div class="asr-prog"><div class="asr-bar"><i id="asrBar"></i></div><small id="asrText">${t("准备下载…", "Starting download…")}</small></div>`;
    if (m.system) return `<button class="btn" data-dl="${m.id}" ${dlId ? "disabled" : ""}>${t("检查／安装", "Check / install")}</button>`;
    return `<button class="btn" data-dl="${m.id}" ${dlId ? "disabled" : ""}>${ICON.download}${t("下载", "Download")} · ${m.size_mb} MB</button>`;
  };
  box.hidden = false;
  $("#sAsrList", box).innerHTML = list
    .map(
      (m) => `
      <div class="asr-row ${m.active ? "on" : ""}">
        <div class="asr-main"><b>${esc(m.name)}</b>${m.id === "paraformer" ? `<em>${t("默认", "Default")}</em>` : ""}<small>${esc(m.desc)}</small>${m.warning ? `<small class="asr-warn">${esc(m.warning)}</small>` : ""}</div>
        <div class="asr-act">${action(m)}</div>
      </div>`,
    )
    .join("");
  if (job && dlId) updateAsrProgress(job);
  const act = async (path: string, init: RequestInit, done?: string) => {
    try {
      const r = await api<Job | { ok: boolean }>(path, init);
      if ("state" in r) S.job = r as Job;
      if (done) toast(done);
      refreshEnv();
    } catch (e) {
      toast(errorMessage(e), true);
    }
    drawAsr();
  };
  $$<HTMLButtonElement>("[data-dl]", box).forEach((b) => (b.onclick = () => act(`/api/models/${b.dataset.dl}/download`, { method: "POST" })));
  $$<HTMLButtonElement>("[data-use]", box).forEach(
    (b) => (b.onclick = () => act(`/api/models/${b.dataset.use}/use`, { method: "POST" }, t("已切换，下一次转写起生效", "Switched — takes effect from the next transcription"))),
  );
  $$<HTMLButtonElement>("[data-del]", box).forEach(
    (b) =>
      (b.onclick = () => {
        const id = b.dataset.del ?? "";
        if (!delArmed.has(id)) {
          delArmed.add(id);
          b.textContent = t("确认删除", "Confirm");
          setTimeout(() => delArmed.delete(id), 4000);
          return;
        }
        delArmed.delete(id);
        act(`/api/models/${id}`, { method: "DELETE" }, t("已删除", "Deleted"));
      }),
  );
}

function updateAsrProgress(job: Job): void {
  const bar = $maybe("#asrBar");
  if (!bar) return;
  const m = /([\d.]+)\s*\/\s*([\d.]+)/.exec(job.stage ?? "");
  $("#asrText").textContent = job.stage || t("准备下载…", "Starting download…");
  if (m) bar.style.width = `${Math.min(100, (Number(m[1]) / Number(m[2])) * 100).toFixed(1)}%`;
}

/** 轮询到任务变化时调用：下载进度、下载完成/失败后刷新卡片 */
export function updateSettingsJob(job: Job, prev: Job): void {
  if (!$maybe("#sAsr") || job.kind !== "models") return;
  if (prev.state !== job.state) {
    if (job.state === "error")
      toast(
        t(`模型下载没有完成：${job.error ?? ""}（已下载的部分会保留，重试时接着下）`, `Model download didn't finish: ${job.error ?? ""} (progress is kept; retrying resumes)`),
        true,
      );
    if (job.state === "done") toast(t("模型下载好了，已切换过去", "Model downloaded and switched on"));
    drawAsr();
  } else updateAsrProgress(job);
}

type Tab = "llm" | "asr" | "record" | "lang" | "hotwords" | "push" | "obsidian" | "storage" | "about";

interface Ctx {
  c: Config;
  vaults: { name: string; path: string }[];
}

let localProviders: Promise<LocalProvider[]> | undefined;

const tabs = (): { id: Tab; name: string; icon: keyof typeof ICON }[] => [
  { id: "llm", name: t("大模型", "LLM"), icon: "bot" },
  { id: "asr", name: t("识别模型", "Speech model"), icon: "mic" },
  { id: "record", name: t("录音", "Recording"), icon: "mic" },
  { id: "lang", name: t("语言", "Language"), icon: "globe" },
  { id: "hotwords", name: t("热词", "Hotwords"), icon: "spark" },
  { id: "push", name: t("推送到群", "Share to chat"), icon: "send" },
  { id: "obsidian", name: "Obsidian", icon: "vault" },
  { id: "storage", name: t("存储", "Storage"), icon: "folder" },
  { id: "about", name: t("运行环境", "About"), icon: "monitor" },
];

const LANG_NAMES: Record<string, [string, string]> = {
  auto: ["跟随系统", "System"],
  zh: ["中文", "中文"],
  en: ["English", "English"],
};

const platformOf = (url: string) =>
  /feishu|larksuite/.test(url)
    ? t("飞书", "Feishu")
    : /dingtalk/.test(url)
      ? t("钉钉", "DingTalk")
      : /weixin\.qq\.com/.test(url)
        ? t("企业微信", "WeCom")
        : t("已设置", "Set");

function subtitle(tab: Tab, x: Ctx): string {
  const c = x.c;
  switch (tab) {
    case "llm":
      return c.model || t("未设置", "Not set");
    case "asr":
      return S.env?.asr_model ?? "Paraformer-large";
    case "hotwords":
      return c.hotwords.length ? t(`${c.hotwords.length} 个`, `${c.hotwords.length} words`) : t("未添加", "None");
    case "push":
      return c.webhook ? platformOf(c.webhook) : t("未设置", "Not set");
    case "obsidian":
      return c.obsidian.vault
        ? c.obsidian.vault.split("/").filter(Boolean).pop() + (c.obsidian.auto ? t(" · 自动存", " · auto") : "")
        : t("未设置", "Not set");
    case "storage":
      return S.meetings.length
        ? t(`${S.meetings.length} 场 · ${fmtBytes(S.meetings.reduce((n, m) => n + (m.size_bytes ?? 0), 0))}`, `${S.meetings.length} meetings · ${fmtBytes(S.meetings.reduce((n, m) => n + (m.size_bytes ?? 0), 0))}`)
        : t("还没有会议", "No meetings yet");
    case "record": {
      const n = c.auto_stop_silence_min ?? 10;
      return n ? t(`${n} 分钟没声音自动结束`, `Auto-stop after ${n} min of silence`) : t("手动结束", "Stop manually");
    }
    case "lang": {
      const ui = LANG_NAMES[c.ui_language ?? "auto"] ?? LANG_NAMES.auto;
      return t(ui[0], ui[1]);
    }
    case "about":
      return S.env ? `v${S.env.version}` : "";
  }
}

/** 保存：接口要完整的配置，没改的项照原样带上（API Key、加签密钥传 null 表示不改） */
async function save(x: Ctx, patch: Record<string, unknown>, tab: Tab): Promise<void> {
  const c = x.c;
  const panel = $maybe("#sBody .set-panel");
  try {
    const updated = await api<Config>("/api/config", {
      method: "PUT",
      json: {
        base_url: c.base_url,
        model: c.model,
        api_key: null,
        hotwords: c.hotwords,
        webhook: c.webhook,
        secret: null,
        obsidian: c.obsidian,
        ...patch,
      },
    });
    // 换了界面语言：侧边栏、页面里写死的文字一起刷新
    const languageChanged = "ui_language" in patch && applyLang(patch.ui_language as LangPref);
    if (languageChanged) {
      renderShell();
      refreshShell();
      refreshMeetings();
      localProviders = undefined;
    }
    toast(t("已保存", "Saved"));
    refreshEnv();
    if (panel?.isConnected) {
      renderSettings(tab, updated);
    } else {
      const current = $maybe(".set-nav a.on")?.dataset.tab as Tab | undefined;
      if (current && languageChanged) renderSettings(current, updated);
      else if (current) updateSettingsNav({ ...x, c: updated }, current);
    }
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

const panelFoot = (extra = "") => `<div class="panel-foot">${extra}<span class="grow"></span><button class="btn primary" id="sSave">${t("保存", "Save")}</button></div>`;

function settingsHeader(): string {
  return `<div><h1>${t("设置", "Settings")}</h1><div class="sub">${t("保存在本机配置文件里", "Stored in a local config file")}</div></div><span class="grow"></span>${
    onboarded() ? "" : `<a class="btn" href="#/welcome/llm">${t("回到新手引导", "Back to the tour")} ${ICON.chevron}</a>`
  }`;
}

function updateSettingsNav(x: Ctx, cur: Tab): void {
  for (const { id, name } of tabs()) {
    const link = $maybe(`.set-nav [data-tab="${id}"]`);
    if (!link) continue;
    link.classList.toggle("on", id === cur);
    $("b", link).textContent = name;
    $("small", link).textContent = subtitle(id, x);
  }
}

/** 窄窗口里二级菜单是横向滚动的一排：把当前这项滚到中间 */
function revealTab(): void {
  const nav = $maybe(".set-nav");
  const on = $maybe(".set-nav a.on");
  if (!nav || !on || nav.scrollWidth <= nav.clientWidth) return;
  nav.scrollTo({ left: on.offsetLeft - (nav.clientWidth - on.offsetWidth) / 2, behavior: "smooth" });
}

export async function renderSettings(tab: string = "llm", updated?: Config): Promise<void> {
  const list = tabs().filter((tb) => tb.id !== "asr" || S.env?.models_downloadable);
  const cur = (list.find((tb) => tb.id === tab) ?? list[0]).id;
  if (!$maybe("#sBody")) {
    localProviders = undefined;
    main().innerHTML = `<div class="page"><div class="page-head" id="sHead">${settingsHeader()}</div>
      <div class="set-layout" id="sBody"><nav class="set-nav">${list.map(
        ({ id, name, icon }) => `<a href="#/settings/${id}" data-tab="${id}"><span class="set-ico">${ICON[icon]}</span><span class="set-txt"><b>${name}</b><small>&nbsp;</small></span></a>`,
      ).join("")}</nav><section class="set-panel"></section></div></div>`;
  } else if (updated) {
    $("#sHead").innerHTML = settingsHeader();
  }

  $$(".set-nav a").forEach((a) => a.classList.toggle("on", a.dataset.tab === cur));
  revealTab();
  const panel = document.createElement("section");
  panel.id = "sPanel";
  panel.className = "card s-card set-panel";
  panel.innerHTML = `<h2>${list.find((tb) => tb.id === cur)?.name}</h2><p class="hint" role="status">${t("正在加载…", "Loading…")}</p>`;
  $("#sBody .set-panel").replaceWith(panel);
  try {
    const c = updated ?? await api<Config>("/api/config");
    if (!panel.isConnected) return;
    const x: Ctx = { c, vaults: [] };
    updateSettingsNav(x, cur);
    if (cur === "obsidian") {
      x.vaults = await api<Ctx["vaults"]>("/api/obsidian/vaults").catch(() => []);
    }
    if (panel.isConnected) await PANELS[cur](x);
  } catch (e) {
    if (panel.isConnected) $("p", panel).textContent = errorMessage(e);
  }
}

const PANELS: Record<Tab, (x: Ctx) => void | Promise<void>> = {
  llm: panelLlm,
  asr: panelAsr,
  lang: panelLang,
  record: panelRecord,
  storage: panelStorage,
  hotwords: panelHotwords,
  push: panelPush,
  obsidian: panelObsidian,
  about: panelAbout,
};

/** 三选一的分段按钮 */
function segChoice(id: string, value: string, options: [string, string, string][]): string {
  return `<div class="seg fit" id="${id}">${options
    .map(([v, zh, en]) => `<button data-v="${v}" class="${v === value ? "on" : ""}">${t(zh, en)}</button>`)
    .join("")}</div>`;
}

function bindSeg(id: string): () => string {
  const box = $(`#${id}`);
  $$<HTMLButtonElement>("button", box).forEach(
    (b) => (b.onclick = () => $$<HTMLButtonElement>("button", box).forEach((x) => x.classList.toggle("on", x === b))),
  );
  return () => $<HTMLButtonElement>("button.on", box).dataset.v ?? "";
}

function panelAsr(x: Ctx): void {
  $("#sPanel").id = "sAsr";
  $("#sAsr").innerHTML = `
    <h2>${t("识别模型", "Speech model")}</h2>
    <p class="hint">${t(
      "把录音变成文字的模型，都在本机运行，录音不会上传。默认的 Paraformer 适合大多数中文会议；会上中英文混着说可以换 SenseVoice，要更准、想让热词在识别时就生效可以换 Fun-ASR-Nano。切换后从下一次转写起生效。",
      "Turns audio into text, entirely on this Mac — audio is never uploaded. Paraformer (default) is best for Chinese; SenseVoice handles English and mixed-language meetings; Fun-ASR-Nano is the most accurate and applies your hotwords during recognition. Changes apply from the next transcription.",
    )}</p>
    <div class="field" style="margin-bottom:14px"><label>${t("会议里说的语言", "Meeting language")}</label>
      ${segChoice("sAsrLang", x.c.asr_language ?? "zh", [
        ["zh", "中文", "Chinese"],
        ["en", "英文", "English"],
        ["auto", "自动识别（多语种）", "Auto-detect (multilingual)"],
      ])}</div>
    <div class="asr-list" id="sAsrList"></div>`;
  const get = bindSeg("sAsrLang");
  // 会议语言改了马上保存（模型列表里的提示也跟着变）
  $$<HTMLButtonElement>("#sAsrLang button").forEach((b) => {
    const prev = b.onclick;
    b.onclick = (e) => {
      prev?.call(b, e);
      save(x, { asr_language: get() }, "asr");
    };
  });
  drawAsr();
}

interface Storage {
  can_change_directory?: boolean;
  root: string;
  count: number;
  meetings_bytes: number;
  audio_bytes: number;
  leftover_bytes: number;
  models_bytes: number | null;
  free_bytes: number;
  largest: { id: string; title: string; date: string; bytes: number; audio_bytes: number; archived: boolean }[];
}

async function panelStorage(): Promise<void> {
  const panel = $("#sPanel");
  panel.innerHTML = `<h2>${t("存储", "Storage")}</h2><p class="hint">${t("正在统计…", "Calculating…")}</p>`;
  let st: Storage;
  try {
    st = await api<Storage>("/api/storage");
  } catch (e) {
    panel.innerHTML = `<h2>${t("存储", "Storage")}</h2><p class="hint">${esc(errorMessage(e))}</p>`;
    return;
  }
  if (!panel.isConnected) return;
  const other = Math.max(0, st.meetings_bytes - st.audio_bytes);
  const total = st.meetings_bytes + (st.models_bytes ?? 0);
  const pct = (n: number) => (total ? Math.max(0.5, (n / total) * 100) : 0).toFixed(1);
  panel.innerHTML = `
    <h2>${t("存储", "Storage")}</h2>
    <p class="hint">${t(`会议保存在 ${esc(st.root)}，磁盘还剩 ${fmtBytes(st.free_bytes)}。删除的东西都会进废纸篓，清空废纸篓后才真正腾出空间。`, `Meetings live in ${esc(st.root)}; ${fmtBytes(st.free_bytes)} free on this disk. Everything deleted goes to the Trash — empty it to actually free the space.`)}</p>
    ${st.can_change_directory ? `<div class="storage-location">
      <label for="stPath">${t("会议存储目录", "Meeting folder")}</label>
      <div class="storage-location-picker"><input class="input" id="stPath" value="${esc(st.root)}" aria-label="${t("会议存储目录", "Meeting folder")}"><button class="btn" id="stChoose">${ICON.folder} ${t("选择文件夹", "Choose folder")}</button></div>
      <label class="storage-copy"><input type="checkbox" id="stCopy" checked> ${t("将已有会议复制到新目录", "Copy existing meetings to the new folder")}</label>
      <p class="hint">${t("录音、转写、纪要及历史版本一起保存。复制需选择空文件夹，原文件会保留。识别模型的位置不变。", "Recordings, transcripts, minutes and history stay together. Copying requires an empty folder; originals are kept. Speech models stay in their current location.")}</p>
      <div class="storage-location-actions"><button class="btn primary" id="stApply" disabled>${t("保存并切换", "Save and switch")}</button><span class="hint" id="stStatus" role="status" aria-live="polite"></span></div>
      <progress id="stProgress" hidden max="100"></progress>
    </div>` : ""}
    <div class="st-bar">
      <i class="a" style="width:${pct(st.audio_bytes)}%"></i><i class="b" style="width:${pct(other)}%"></i>${st.models_bytes ? `<i class="c" style="width:${pct(st.models_bytes)}%"></i>` : ""}
    </div>
    <div class="st-legend">
      <span><i class="a"></i>${t("录音", "Recordings")} <b>${fmtBytes(st.audio_bytes)}</b></span>
      <span><i class="b"></i>${t("转写、纪要和导出的文件", "Transcripts, minutes & exports")} <b>${fmtBytes(other)}</b></span>
      ${st.models_bytes != null ? `<span><i class="c"></i>${t("识别模型", "Speech models")} <b>${fmtBytes(st.models_bytes)}</b></span>` : ""}
    </div>

    <h3 class="st-h">${t("清理", "Clean up")}</h3>
    <div class="st-row">
      <div><b>${t("残留文件", "Leftover files")}</b><small>${t("合成录音失败前留下的原始音轨、没下完的临时文件", "Raw tracks left behind when merging failed, unfinished temp files")}</small></div>
      <button class="btn" id="stLeft" ${st.leftover_bytes ? "" : "disabled"}>${st.leftover_bytes ? t(`清理 ${fmtBytes(st.leftover_bytes)}`, `Clean ${fmtBytes(st.leftover_bytes)}`) : t("没有残留", "Nothing to clean")}</button>
    </div>
    <div class="st-row">
      <div><b>${t("删除旧会议的录音", "Delete old recordings")}</b><small>${t("只删录音，转写和纪要都留着，照样能看、能搜", "Only the audio — transcripts and minutes stay viewable and searchable")}</small>
        <div class="st-opts">${segChoice("stDays", "90", [
          ["30", "30 天前", "> 30 days"],
          ["90", "90 天前", "> 90 days"],
          ["180", "半年前", "> 6 months"],
          ["365", "一年前", "> 1 year"],
        ])}<button class="toggle" id="stArch">${t("只处理已归档的", "Archived only")}</button></div></div>
      <button class="btn" id="stAudio">${t("删除录音", "Delete recordings")}</button>
    </div>

    <h3 class="st-h">${t(`最占空间的会议（共 ${st.count} 场）`, `Largest meetings (${st.count} total)`)}</h3>
    <div class="st-list">${
      st.largest.length
        ? st.largest
            .map(
              (m) => `<div class="st-item"><a href="#/m/${encodeURIComponent(m.id)}"><b>${esc(m.title)}</b>${m.archived ? `<span class="arch-badge">${t("已归档", "Archived")}</span>` : ""}<small>${esc(m.date)} · ${fmtBytes(m.bytes)}${m.audio_bytes ? t(`，录音 ${fmtBytes(m.audio_bytes)}`, `, audio ${fmtBytes(m.audio_bytes)}`) : ""}</small></a>
              ${m.audio_bytes ? `<button class="btn ghost" data-audio="${esc(m.id)}">${t("只删录音", "Delete audio")}</button>` : ""}</div>`,
            )
            .join("")
        : `<p class="hint">${t("还没有会议", "No meetings yet")}</p>`
    }</div>`;
  if (st.can_change_directory) {
    const path = $<HTMLInputElement>("#stPath");
    const copy = $<HTMLInputElement>("#stCopy");
    const choose = $<HTMLButtonElement>("#stChoose");
    const apply = $<HTMLButtonElement>("#stApply");
    const status = $("#stStatus");
    const progress = $<HTMLProgressElement>("#stProgress");
    let working = false;
    const update = () => { apply.disabled = working || !path.value.trim() || path.value.trim() === st.root; };
    path.oninput = update;
    choose.onclick = async () => {
      choose.disabled = true;
      try {
        const selected = await api<{ path: string | null }>("/api/storage/choose", { json: {} });
        if (selected.path) path.value = selected.path;
        update();
      } catch (e) { toast(errorMessage(e), true); }
      finally { choose.disabled = false; }
    };
    apply.onclick = async () => {
      if (S.jobs.some(isBusy) || isBusy(S.job)) {
        toast(t("请等正在进行的任务完成后再切换", "Wait for current tasks to finish before switching"), true);
        return;
      }
      const destination = path.value.trim();
      const copyExisting = copy.checked;
      if (!(await confirmBox(t("切换会议存储目录？", "Switch meeting folder?"),
        copyExisting ? t(`将会议复制到 ${destination}，完成后切换。原目录保留，请勿在复制期间退出 Tingo。`, `Copy meetings to ${destination}, then switch. Originals are kept. Keep Tingo open until finished.`)
        : t(`切换到 ${destination}。只显示新目录中的会议；旧会议保留原处，可切回原目录查看。`, `Switch to ${destination}. Only meetings in that folder will be shown; you can switch back to see the originals.`), t("确认切换", "Switch folder")))) return;
      working = true;
      path.disabled = copy.disabled = choose.disabled = true;
      update();
      progress.hidden = false;
      progress.removeAttribute("value");
      status.textContent = t("正在检查目录…", "Checking folder…");
      const poll = async () => {
        try {
          const p = await api<{ active: boolean; copied_bytes: number; total_bytes: number }>("/api/storage/progress");
          if (!working || !p.active) return;
          if (p.total_bytes) {
            progress.value = Math.min(100, p.copied_bytes / p.total_bytes * 100);
            status.textContent = t(`复制并校验 ${fmtBytes(p.copied_bytes)} / ${fmtBytes(p.total_bytes)}`, `Copying and verifying ${fmtBytes(p.copied_bytes)} / ${fmtBytes(p.total_bytes)}`);
          }
        } catch { /* the final request reports errors */ }
      };
      const timer = window.setInterval(() => void poll(), 700);
      try {
        await api("/api/storage/directory", { json: { path: destination, copy_existing: copyExisting } });
        S.detail = null;
        S.jobs = [];
        S.job = { state: "idle" };
        await Promise.all([refreshEnv(), refreshMeetings()]);
        toast(t("存储目录已切换，原目录文件保留", "Folder changed. Original files are kept."));
        if (path.isConnected) await panelStorage();
      } catch (e) {
        status.textContent = errorMessage(e);
        toast(errorMessage(e), true);
      } finally {
        working = false;
        clearInterval(timer);
        path.disabled = copy.disabled = choose.disabled = false;
        progress.hidden = true;
        update();
      }
    };
  }
  const days = bindSeg("stDays");
  $("#stArch").onclick = () => $("#stArch").classList.toggle("on");
  const run = async (body: unknown, what: string) => {
    try {
      const r = await api<{ count: number; freed_bytes: number }>("/api/storage/cleanup", { json: body });
      if (!r.count) {
        toast(t("没有符合条件的会议，什么都没删", "Nothing matched — nothing was deleted"));
        return;
      }
      toast(t(`已把 ${r.count} ${what}移到废纸篓，腾出 ${fmtBytes(r.freed_bytes)}`, `Moved ${r.count} to the Trash — freed ${fmtBytes(r.freed_bytes)}`));
      await refreshMeetings();
      if (panel.isConnected) panelStorage();
    } catch (e) {
      toast(errorMessage(e), true);
    }
  };
  $("#stLeft").onclick = () => run({ what: "leftovers" }, t("个文件", ""));
  $("#stAudio").onclick = async () => {
    const n = Number(days());
    const archivedOnly = $("#stArch").classList.contains("on");
    const ok = await confirmBox(
      t(`删除 ${n} 天前${archivedOnly ? "已归档" : ""}会议的录音？`, `Delete recordings of ${archivedOnly ? "archived " : ""}meetings older than ${n} days?`),
      t("录音会移到废纸篓，转写和纪要保留，以后就不能回听这些会议了。", "Recordings move to the Trash; transcripts and minutes stay, but you can't play those meetings back."),
      t("删除录音", "Delete recordings"),
    );
    if (ok) run({ what: "audio", older_than_days: n, archived_only: archivedOnly }, t("场会议的录音", ""));
  };
  $$<HTMLButtonElement>("[data-audio]", panel).forEach(
    (b) =>
      (b.onclick = async () => {
        if (!(await confirmBox(t("删除这场会议的录音？", "Delete this meeting's recording?"), t("录音会移到废纸篓，转写和纪要保留。", "The recording moves to the Trash; transcript and minutes stay."), t("删除录音", "Delete recording"))))
          return;
        try {
          const r = await deleteAudio(b.dataset.audio ?? "");
          toast(t(`腾出 ${fmtBytes(r.freed_bytes)}`, `Freed ${fmtBytes(r.freed_bytes)}`));
          await refreshMeetings();
          if (panel.isConnected) panelStorage();
        } catch (e) {
          toast(errorMessage(e), true);
        }
      }),
  );
}

function panelRecord(x: Ctx): void {
  const c = x.c;
  const cur = String(c.auto_stop_silence_min ?? 10);
  $("#sPanel").innerHTML = `
    <h2>${t("录音", "Recording")}</h2>
    <p class="hint">${t("忘了点结束也没关系：会开完、没人说话了就自动结束，并把末尾的空白截掉。", "Forgot to stop? Tingo ends the recording when the meeting is over and trims the silent tail.")}</p>
    <div class="s-grid">
      <div class="field"><label>${t("连续多久没人说话就自动结束", "Stop automatically after this much silence")}</label>
        ${segChoice(
          "sSilence",
          ["0", "5", "10", "20", "30"].includes(cur) ? cur : "10",
          [
            ["0", "不自动结束", "Never"],
            ["5", "5 分钟", "5 min"],
            ["10", "10 分钟", "10 min"],
            ["20", "20 分钟", "20 min"],
            ["30", "30 分钟", "30 min"],
          ],
        )}</div>
      <div class="s-toggles">
        <button class="toggle ${c.auto_stop_app_quit !== false ? "on" : ""}" id="sAppQuit">${ICON.monitor}${t(
          "会议软件退出时自动结束",
          "Stop when the meeting app quits",
        )}</button>
      </div>
      <small class="hint">${t(
        "会议软件包括腾讯会议、飞书、钉钉、Zoom、Teams、Webex、Slack：开始录音时开着的，全部退出后就结束。",
        "Meeting apps: Zoom, Teams, Webex, Slack, Tencent Meeting, Feishu, DingTalk — recording ends once all that were open at the start have quit.",
      )}</small>
    </div>
    ${panelFoot()}`;
  const silence = bindSeg("sSilence");
  $("#sAppQuit").onclick = () => $("#sAppQuit").classList.toggle("on");
  $("#sSave").onclick = () =>
    save(x, { auto_stop_silence_min: Number(silence()), auto_stop_app_quit: $("#sAppQuit").classList.contains("on") }, "record");
}

function panelLang(x: Ctx): void {
  $("#sPanel").innerHTML = `
    <h2>${t("语言", "Language")}</h2>
    <div class="s-grid">
      <div class="field"><label>${t("界面语言", "App language")}</label>
        ${segChoice("sUiLang", x.c.ui_language ?? "auto", [
          ["auto", "跟随系统", "System"],
          ["zh", "中文", "中文"],
          ["en", "English", "English"],
        ])}</div>
      <div class="field"><label>${t("纪要语言", "Minutes language")}</label>
        ${segChoice("sMinLang", x.c.minutes_language ?? "auto", [
          ["auto", "跟会议语言一致", "Same as the meeting"],
          ["zh", "中文", "Chinese"],
          ["en", "英文", "English"],
        ])}
        <small class="hint" style="margin-top:6px">${t(
          "选了和会议不同的语言时，大模型会边整理边翻译，比如英文会议也能出中文纪要。",
          "If it differs from the meeting language, the LLM translates while writing — e.g. English minutes for a Chinese meeting.",
        )}</small></div>
      <div class="field"><label>${t("会议里说的语言", "Meeting language")}</label>
        <small class="hint">${t("在「识别模型」里设置：识别模型要知道会上说的是哪种语言。", "Set under Speech model — the recognizer needs to know what language is spoken.")}
          <a href="#/settings/asr">${t("去设置", "Open")}</a></small></div>
    </div>
    ${panelFoot()}`;
  const ui = bindSeg("sUiLang");
  const min = bindSeg("sMinLang");
  $("#sSave").onclick = () => save(x, { ui_language: ui(), minutes_language: min() }, "lang");
}

function panelLlm(x: Ctx): void {
  const c = x.c;
  const panel = $("#sPanel");
  const presets = remotePresets();
  const norm = (u: string) => u.trim().replace(/\/+$/, "").replace("localhost", "127.0.0.1");
  const baseInput = () => $<HTMLInputElement>("#sBase");
  const presetOf = () =>
    (presets.find((p) => p.base_url && norm(baseInput().value) === norm(p.base_url)) ?? presets[presets.length - 1]).id;
  const dot = (p: Preset) =>
    p.local
      ? `<span class="dot" style="background:${p.local.running ? "var(--green)" : "#c9ccd1"};display:inline-block;margin-right:6px;vertical-align:1px" title="${p.local.running ? t("运行中", "Running") : t("未启动", "Not running")}"></span>`
      : "";
  const keyHint = c.api_key_set
    ? t(
        `已设置${c.api_key_hint !== "已设置" ? `（${esc(c.api_key_hint)}）` : ""}，留空表示不修改`,
        `Saved${c.api_key_hint !== "已设置" && c.api_key_hint !== "Set" ? ` (${esc(c.api_key_hint)})` : ""} — leave empty to keep it`,
      )
    : t("本地 Ollama 不需要", "Not needed for local Ollama");
  panel.innerHTML = `
    <h2>${t("大模型", "LLM")}</h2>
    <p class="hint">${t(
      "用来把转写整理成纪要。支持 OpenAI 兼容接口和 Anthropic（Claude）接口。本机的 Magpie、CC Switch 中转打开后可以直接选；选本地 Ollama 则全程不出本机。",
      "Turns the transcript into minutes. Works with OpenAI-compatible and Anthropic (Claude) APIs. Local relays (Magpie, CC Switch) are detected automatically; pick local Ollama to keep everything on this Mac.",
    )}</p>
    <div class="s-grid">
      <div class="prov-grid" id="sPreset"></div>
      <div class="field"><label>${t("接口协议", "API protocol")}</label>
        ${segChoice("sApi", c.api === "anthropic" ? "anthropic" : "openai", [
          ["openai", "OpenAI 兼容", "OpenAI-compatible"],
          ["anthropic", "Anthropic（Claude）", "Anthropic (Claude)"],
        ])}</div>
      <div class="s-row2">
        <div class="field"><label>${t("接口地址 Base URL", "Base URL")}</label><input class="input" id="sBase" value="${esc(c.base_url)}"></div>
        <div class="field"><label>${t("模型", "Model")} <span id="sModelCount" style="color:var(--muted-2)"></span>
          <button class="link-btn" id="sFetch" type="button">${ICON.redo}${t("获取模型列表", "Fetch models")}</button></label>
          <div class="combo"><input class="input" id="sModel" value="${esc(c.model)}" placeholder="${t("点这里从列表里选，也可以直接输入", "Pick from the list or type a model name")}" autocomplete="off" spellcheck="false"><span class="combo-arrow">${ICON.chevron}</span><div class="combo-pop" id="sModelPop" hidden></div></div></div>
      </div>
      <div class="field"><label>API Key</label><input class="input" id="sKey" type="password" autocomplete="off" placeholder="${keyHint}"></div>
    </div>
    ${panelFoot(`<button class="btn" id="sTest">${t("测试连接", "Test connection")}</button><span class="test-msg" id="sTestMsg"></span>`)}`;

  const model = $<HTMLInputElement>("#sModel");
  const key = $<HTMLInputElement>("#sKey");
  const presetGrid = $("#sPreset", panel);
  const modelCount = $("#sModelCount", panel);
  const testMsg = $("#sTestMsg", panel);
  const getApi = bindSeg("sApi");
  const setApi = (a: string) => $$<HTMLButtonElement>("#sApi button").forEach((b) => b.classList.toggle("on", b.dataset.v === a));
  const markPreset = () => $$<HTMLButtonElement>("button", presetGrid).forEach((b) => b.classList.toggle("on", b.dataset.id === presetOf()));
  const renderPresets = () => {
    presetGrid.innerHTML = presets.map((p) => `<button data-id="${p.id}" title="${esc(p.name)}">${dot(p)}<span>${esc(p.name)}</span></button>`).join("");
    markPreset();
  };
  renderPresets();

  let modelList: string[] = [];
  let recommended = "";
  let refreshPicker = () => {};
  const setModels = (models: string[], rec = "") => {
    modelList = models;
    recommended = rec;
    modelCount.textContent = models.length ? t(`（可选 ${models.length} 个）`, `(${models.length} available)`) : "";
    refreshPicker();
  };
  refreshPicker = bindModelPicker(
    () => modelList,
    () => recommended,
  );
  const msg = (text: string, cls = "") => {
    testMsg.className = "test-msg " + cls;
    testMsg.textContent = text;
  };
  /** 拉模型列表；manual 为 true 时（点了按钮）把结果告诉用户 */
  const fetchModels = async (manual = false) => {
    if (!baseInput().value.trim()) {
      if (manual) msg(t("先填接口地址", "Enter the base URL first"), "bad");
      return;
    }
    if (manual) msg(t("正在获取模型列表…", "Fetching models…"));
    try {
      const r = await api<{ ok: boolean; models: string[] }>("/api/llm/models", {
        json: { base_url: baseInput().value.trim(), model: "", api_key: key.value.trim() || null, api: getApi() },
      });
      if (!panel.isConnected) return;
      setModels(r.models);
      if (manual)
        msg(
          r.ok
            ? r.models.length
              ? t(`找到 ${r.models.length} 个模型，点模型框选择`, `Found ${r.models.length} models — pick one in the model box`)
              : t("这个接口没有返回模型列表，直接输入模型名即可", "This endpoint didn't return a model list — just type the model name")
            : t("拿不到模型列表：检查接口地址、API Key 和接口协议", "Couldn't get the model list: check the URL, API key and protocol"),
          r.ok ? "ok" : "bad",
        );
    } catch (e) {
      if (!panel.isConnected) return;
      setModels([]);
      if (manual) msg(errorMessage(e), "bad");
    }
  };
  $("#sFetch").onclick = (e) => {
    e.preventDefault();
    fetchModels(true);
  };
  let presetEdited = false;
  localProviders ??= api<LocalProvider[]>("/api/llm/providers").catch(() => []);
  void localProviders.then((providers) => {
    if (!panel.isConnected) return;
    if (providers.length) {
      presets.unshift(...providers.map((p) => ({ id: p.id, name: p.name, base_url: p.base_url, local: p })));
      renderPresets();
    }
    if (presetEdited || baseInput().value !== c.base_url || key.value) return;
    const current = presets.find((p) => p.id === presetOf());
    if (current?.local) setModels(current.local.models, current.local.recommended);
    else if (c.base_url) void fetchModels();
  });
  presetGrid.onclick = (e) => {
    const id = (e.target as Element).closest<HTMLElement>("[data-id]")?.dataset.id;
    const p = presets.find((x) => x.id === id);
    if (!p) return;
    presetEdited = true;
    msg("");
    if (!p.base_url) {
      // 自定义：清空，让用户填自己的地址；哪个预设高亮是按地址判断的，不清空就会跳回原来那个
      baseInput().value = "";
      model.value = "";
      key.value = "";
      setModels([]);
      markPreset();
      baseInput().focus();
      msg(t("填接口地址（OpenAI 兼容的一般以 /v1 结尾）、选接口协议，再点「获取模型列表」", "Enter the base URL (OpenAI-compatible ones usually end in /v1), choose the protocol, then click Fetch models"));
      return;
    }
    baseInput().value = p.base_url;
    setApi(p.api ?? "openai");
    if (p.local) {
      const lp = p.local;
      setModels(lp.models, lp.recommended);
      key.value = lp.api_key; // 本机中转的 key 是固定的，直接填上
      if (!lp.models.includes(model.value)) {
        model.value = (lp.recommended && lp.models.includes(lp.recommended) && lp.recommended) || lp.models[0] || "";
      }
      if (!lp.running) msg(lp.hint, "bad");
      else if (lp.recommended && model.value === lp.recommended) msg(t(`已选推荐模型 ${lp.recommended}：实测 76 分钟的会 51 秒出纪要`, `Recommended model selected: ${lp.recommended} (a 76-minute meeting took 51 s)`), "ok");
    } else {
      setModels([]);
      key.value = "";
      model.value = p.model ?? "";
      if (!c.api_key_set) msg(t("需要填写这家服务的 API Key", "This provider needs an API key"));
    }
    markPreset();
  };
  baseInput().oninput = markPreset;
  baseInput().onchange = () => fetchModels();
  key.onchange = () => fetchModels(); // 填了 Key 才拿得到模型列表的服务（OpenAI、Claude 等），填完自动拉一次
  $$<HTMLButtonElement>("#sApi button").forEach((b) => b.addEventListener("click", () => fetchModels()));

  $("#sTest").onclick = async () => {
    msg(t("连接中…", "Connecting…"));
    try {
      const r = await api<{ ok: boolean; message: string }>("/api/config/test", {
        json: { base_url: baseInput().value.trim(), model: model.value.trim(), api_key: key.value.trim() || null, api: getApi() },
      });
      msg(r.message, r.ok ? "ok" : "bad");
    } catch (e) {
      msg(errorMessage(e), "bad");
    }
  };
  $("#sSave").onclick = () =>
    save(x, { base_url: baseInput().value.trim(), model: model.value.trim(), api_key: key.value.trim() || null, api: getApi() }, "llm");
}

function panelHotwords(x: Ctx): void {
  const hot = [...x.c.hotwords];
  const nano = S.env?.asr_model === "Fun-ASR-Nano";
  $("#sPanel").innerHTML = `
    <h2>${t("热词", "Hotwords")}</h2>
    <p class="hint">${t("人名、项目代号、内部系统名、行业术语。每场会议都生效，录音时也可以给单场会议临时加几个。", "Names, project codes, internal systems, jargon. Applied to every meeting; you can add a few more per meeting when recording.")}</p>
    <div class="tags" id="sTags"><input id="sTagInput" placeholder="${t("输入后回车添加，可以一次粘贴多个（用逗号或空格隔开）", "Type and press Enter — paste several at once, separated by commas or spaces")}"></div>
    <div class="note-box">${
      nano
        ? t("当前识别模型 Fun-ASR-Nano 会在识别时直接用上这些词；生成纪要时也会交给大模型核对同音字。", "Fun-ASR-Nano uses these words during recognition; they're also given to the LLM as a glossary when writing minutes.")
        : t(
            "当前识别模型不支持热词，这些词会作为术语表交给大模型，在写纪要时纠正同音字（比如“历史中单”→“历史工单”）。想在识别时就用上，可以在「识别模型」里换成 Fun-ASR-Nano。",
            "The current speech model doesn't take hotwords, so they're given to the LLM as a glossary to fix misheard words in the minutes. To use them during recognition, switch to Fun-ASR-Nano under Speech model.",
          )
    }</div>
    ${panelFoot()}`;
  const tagInput = $<HTMLInputElement>("#sTagInput");
  const drawTags = () => {
    $$("#sTags .chip").forEach((el) => el.remove());
    hot.forEach((w, i) => {
      const chip = document.createElement("span");
      chip.className = "chip";
      chip.innerHTML = `${esc(w)}<button class="x" title="${t("删除", "Remove")}">×</button>`;
      $("button", chip).onclick = () => {
        hot.splice(i, 1);
        drawTags();
      };
      tagInput.before(chip);
    });
  };
  drawTags();
  const addPending = () => {
    for (const w of tagInput.value.split(/[,，、\s]+/)) if (w && !hot.includes(w)) hot.push(w);
    tagInput.value = "";
  };
  tagInput.onkeydown = (e) => {
    const v = tagInput.value.trim();
    if ((e.key === "Enter" || e.key === "," || e.key === "，") && v) {
      e.preventDefault();
      addPending();
      drawTags();
    } else if (e.key === "Backspace" && !v && hot.length) {
      hot.pop();
      drawTags();
    }
  };
  $("#sSave").onclick = () => {
    addPending();
    save(x, { hotwords: hot }, "hotwords");
  };
}

function panelPush(x: Ctx): void {
  const c = x.c;
  $("#sPanel").innerHTML = `
    <h2>${t("推送到群", "Share to chat")}</h2>
    <p class="hint">${t(
      "在群里添加\"自定义机器人\"，把 webhook 地址贴过来，按域名自动识别飞书、钉钉或企业微信。会议详情页点「推送到群」就会发过去。",
      "Add a custom bot to your group chat and paste its webhook URL — Feishu, DingTalk and WeCom are detected from the domain. Then click “Share to chat” on a meeting.",
    )}</p>
    <div class="s-grid">
      <div class="field"><label>${t("Webhook 地址", "Webhook URL")}</label><input class="input" id="sHook" value="${esc(c.webhook)}" placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/…"></div>
      <div class="field"><label>${t("加签密钥（机器人开启了\"加签\"才需要）", "Signing secret (only if the bot has signing enabled)")}</label><input class="input" id="sSecret" type="password" autocomplete="off" placeholder="${c.secret_set ? t("已设置，留空表示不修改", "Saved — leave empty to keep it") : t("可选", "Optional")}"></div>
    </div>
    ${panelFoot()}`;
  $("#sSave").onclick = () =>
    save(x, { webhook: $<HTMLInputElement>("#sHook").value.trim(), secret: $<HTMLInputElement>("#sSecret").value.trim() || null }, "push");
}

function panelObsidian(x: Ctx): void {
  const c = x.c;
  const vaults = x.vaults;
  $("#sPanel").innerHTML = `
    <h2>Obsidian</h2>
    <p class="hint">${t(
      "把纪要存进 Obsidian 库：一场会一篇笔记，日期、时长、参会人写在属性里；鱼骨图存成 Excalidraw 画板嵌进笔记（装了 Excalidraw 插件就能直接改），每个分支下面列着引用的原话。",
      "Save minutes to your Obsidian vault: one note per meeting with date, duration and participants as properties; fishbone diagrams are embedded as Excalidraw drawings (editable with the Excalidraw plugin), each branch listing its quotes.",
    )}</p>
    <div class="s-grid">
      ${vaults.length ? `<div class="field"><label>${t("本机 Obsidian 里的库", "Vaults on this Mac")}</label><div class="seg fit" id="sVaults">${vaults.map((v) => `<button data-path="${esc(v.path)}" title="${esc(v.path)}">${esc(v.name)}</button>`).join("")}</div></div>` : ""}
      <div class="s-row2">
        <div class="field"><label>${t("库的位置", "Vault location")}</label><input class="input" id="sVault" value="${esc(c.obsidian.vault)}" placeholder="${vaults.length ? t("点上面选一个，或者填库的文件夹路径", "Pick one above or enter the vault folder") : t("Obsidian 库的文件夹路径，比如 ~/Documents/笔记", "Path to your vault folder, e.g. ~/Documents/Notes")}"></div>
        <div class="field"><label>${t("存到库里的哪个文件夹", "Folder inside the vault")}</label><input class="input" id="sObFolder" value="${esc(c.obsidian.folder)}" placeholder="${t("留空表示库的根目录", "Leave empty for the vault root")}"></div>
      </div>
      <div class="s-toggles">
        <button class="toggle ${c.obsidian.auto ? "on" : ""}" id="sObAuto">${ICON.redo}${t("生成纪要后自动存", "Save automatically after minutes")}</button>
        <button class="toggle ${c.obsidian.transcript ? "on" : ""}" id="sObTr">${ICON.file}${t("附带转写全文", "Include full transcript")}</button>
      </div>
    </div>
    ${panelFoot()}`;
  const vault = $<HTMLInputElement>("#sVault");
  const markVault = () => $$<HTMLButtonElement>("#sVaults button").forEach((b) => b.classList.toggle("on", b.dataset.path === vault.value.trim()));
  markVault();
  $$<HTMLButtonElement>("#sVaults button").forEach((b) => (b.onclick = () => ((vault.value = b.dataset.path ?? ""), markVault())));
  vault.oninput = markVault;
  $$<HTMLButtonElement>("#sObAuto, #sObTr").forEach((b) => (b.onclick = () => b.classList.toggle("on")));
  $("#sSave").onclick = () =>
    save(
      x,
      {
        obsidian: {
          vault: vault.value.trim(),
          folder: $<HTMLInputElement>("#sObFolder").value.trim(),
          auto: $("#sObAuto").classList.contains("on"),
          transcript: $("#sObTr").classList.contains("on"),
        },
      },
      "obsidian",
    );
}

function bindUpdateButton(): void {
  const b = $maybe<HTMLButtonElement>("#sUpd");
  if (!b) return;
  b.onclick = async () => {
    const msg = $("#sUpdMsg");
    msg.className = "test-msg";
    msg.textContent = t("检查中…", "Checking…");
    const u = await checkUpdate(true);
    if (!u) {
      msg.textContent = t("这个版本不支持检查更新", "Update checks aren't available in this version");
    } else if (u.error) {
      msg.className = "test-msg bad";
      msg.textContent = t(`检查失败：${u.error}`, `Check failed: ${u.error}`);
    } else if (u.newer) {
      msg.className = "test-msg ok";
      msg.textContent = t(`有新版本 ${u.latest}`, `Version ${u.latest} is available`);
      refreshEnv();
      showUpdate();
    } else {
      msg.className = "test-msg ok";
      msg.textContent = t(`已经是最新版本（${u.current}）`, `You're up to date (${u.current})`);
    }
  };
}

function panelAbout(x: Ctx): void {
  const env = S.env;
  const checks: [boolean, string, string][] = [
    ...(env?.models_downloadable ? [] : ([[!!env?.ffmpeg, "ffmpeg", env?.ffmpeg ? t("已安装", "Installed") : "brew install ffmpeg"]] as [boolean, string, string][])),
    [
      !!env?.system_audio,
      t("录系统声音", "System audio"),
      env?.system_audio
        ? t("可以录腾讯会议、飞书里对方的声音", "Can record the other side in Zoom, Teams, Tencent Meeting, Feishu…")
        : t("需要 macOS 15+ 和 Xcode 命令行工具；不影响导入录音", "Needs macOS 15+ and Xcode Command Line Tools; importing recordings still works"),
    ],
    [
      !!env?.model_cached,
      t("识别模型", "Speech model"),
      `${env?.asr_model ?? ""}${env?.model_loaded ? t(" 已加载", " loaded") : env?.model_cached ? t(" 已下载", " downloaded") : t(" 未下载", " not downloaded")}`.trim(),
    ],
    [!!env?.llm_ready, t("大模型", "LLM"), env?.llm_ready ? env.llm_model : t("还没有配置", "Not set up")],
  ];
  $("#sPanel").innerHTML = `
    <h2>${t("运行环境", "About")}</h2>
    <div class="kv">
      <div><span>${t("会议保存在", "Meetings")}</span><code>${esc(env?.root ?? "")}</code></div>
      <div><span>${t("配置文件", "Config")}</span><code>${esc(x.c.path)}</code></div>
      <div><span>${t("版本", "Version")}</span><code>${esc(env?.version ?? "")}</code></div>
    </div>
    <div class="about-actions">
      <button class="btn" id="sUpd">${ICON.redo}${t("检查更新", "Check for updates")}</button><span class="test-msg" id="sUpdMsg"></span>
      <span class="grow"></span>
      <a class="btn ghost" href="#/welcome">${t("重新打开新手引导", "Reopen the tour")}</a>
    </div>
    <div class="checks">
      ${checks
        .map(
          ([ok, name, note]) =>
            `<div class="check"><span class="dot" style="background:${ok ? "var(--green)" : "var(--amber)"}"></span>${name}<small>${esc(note)}</small></div>`,
        )
        .join("")}
    </div>`;  bindUpdateButton();
}
