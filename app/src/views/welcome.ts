/* 新手引导：第一次打开时走一遍——认识 Tingo、下载识别模型、试录 3 秒确认权限、接上大模型、看示例会议。
   每一步都可以跳过；走完（或点"跳过引导"）后记在 localStorage，不再自动弹出，设置 → 运行环境里可以重新打开。 */

import { api, type Config, type Job } from "../api";
import { $, $maybe, errorMessage, esc, ICON, main, toast } from "../dom";
import { lang, t } from "../i18n";
import { poll } from "../poll";
import { refreshEnv, refreshMeetings } from "../sidebar";
import { isBusy, needsModels, S } from "../state";

const KEY = "tingo.onboarded";

export const onboarded = (): boolean => {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return true; // 拿不到 localStorage 就别每次都弹
  }
};

const finish = () => {
  try {
    localStorage.setItem(KEY, "1");
  } catch {
    /* 同上 */
  }
};

/** 第一次打开、还没有会议时自动进引导 */
export const shouldOnboard = (): boolean => !onboarded() && (needsModels() || S.meetings.length === 0);

type Step = "hello" | "model" | "perm" | "llm" | "demo";
const STEPS: Step[] = ["hello", "model", "perm", "llm", "demo"];
let step: Step = "hello";
let permResult: { system: string; mic: string; message?: string } | null = null;

export function renderWelcome(at?: string): void {
  step = STEPS.includes(at as Step) ? (at as Step) : "hello";
  permResult = null;
  draw();
}

function stepNames(): Record<Step, string> {
  return {
    hello: t("认识 Tingo", "Welcome"),
    model: t("识别模型", "Speech model"),
    perm: t("录音权限", "Permissions"),
    llm: t("大模型", "LLM"),
    demo: t("示例会议", "Sample meeting"),
  };
}

function draw(): void {
  const i = STEPS.indexOf(step);
  const names = stepNames();
  main().innerHTML = `
    <div class="page welcome">
      <div class="wl-steps">${STEPS.map((s, k) => `<span class="${k < i ? "done" : k === i ? "now" : ""}"><b>${k < i ? "✓" : k + 1}</b>${esc(names[s])}</span>`).join("")}</div>
      <section class="card wl-card" id="wlBody"></section>
      <div class="wl-foot">
        <button class="btn ghost" id="wlSkip">${t("跳过引导", "Skip the tour")}</button>
        <span class="grow"></span>
        ${i > 0 ? `<button class="btn" id="wlPrev">${t("上一步", "Back")}</button>` : ""}
        <button class="btn primary" id="wlNext">${i === STEPS.length - 1 ? t("开始第一场会议", "Start my first meeting") : t("下一步", "Next")}</button>
      </div>
    </div>`;
  $("#wlSkip").onclick = () => {
    finish();
    location.hash = "#/";
  };
  const prev = $maybe("#wlPrev");
  if (prev) prev.onclick = () => go(-1);
  $("#wlNext").onclick = () => {
    if (i === STEPS.length - 1) {
      finish();
      location.hash = "#/";
    } else go(1);
  };
  BODY[step]();
}

function go(d: number): void {
  step = STEPS[Math.min(STEPS.length - 1, Math.max(0, STEPS.indexOf(step) + d))];
  draw();
}

/** 下载多语种模型要多大：基础模型（标点、声纹、VAD）+ SenseVoice */
let sizeMb = 0;
async function multilingualSize(): Promise<number> {
  try {
    const r = await api<{ models: { id: string; size_mb: number }[] }>("/api/models");
    const size = (id: string) => r.models.find((m) => m.id === id)?.size_mb ?? 0;
    const base = (S.env?.models_size_mb ?? 0) - size("paraformer");
    return base > 0 && size("sensevoice") ? base + size("sensevoice") : 0;
  } catch {
    return 0;
  }
}

/** 非中文界面：会议语言设成自动识别，再下载 SenseVoice（连同基础模型，下完自动设为当前模型） */
async function downloadMultilingual(): Promise<Job> {
  const c = await api<Config>("/api/config");
  await api("/api/config", {
    method: "PUT",
    json: { base_url: c.base_url, model: c.model, api_key: null, hotwords: c.hotwords, webhook: c.webhook, secret: null, asr_language: "auto" },
  });
  return api<Job>("/api/models/sensevoice/download", { method: "POST" });
}

const BODY: Record<Step, () => void> = {
  hello() {
    $("#wlBody").innerHTML = `
      <img src="data:image/svg+xml,${encodeURIComponent(document.querySelector(".brand-mark svg")?.outerHTML.replace("<svg", "<svg xmlns='http://www.w3.org/2000/svg'") ?? "")}" class="wl-logo" alt="">
      <h1>${t("开过的会，都听过。", "Every meeting, heard.")}</h1>
      <p class="hint">${t("Tingo 录下会议，在这台 Mac 上转文字、分清谁说了什么，再由你选的大模型整理成纪要。花一分钟走完下面几步就能用。", "Tingo records your meetings, transcribes them on this Mac, tells speakers apart, and has the LLM you choose write the minutes. Take a minute to set it up.")}</p>
      <ul class="wl-points">
        <li>${ICON.mic}<span><b>${t("录音不出本机", "Audio stays on your Mac")}</b>${t("识别、区分说话人都在本地完成", "Recognition and speaker separation run locally")}</span></li>
        <li>${ICON.monitor}<span><b>${t("直接录线上会议", "Records any call")}</b>${t("腾讯会议、飞书、Zoom 里对方的声音都能录", "Captures the other side in Zoom, Teams, Feishu…")}</span></li>
        <li>${ICON.spark}<span><b>${t("结论有出处", "Every claim has a source")}</b>${t("纪要每条都能点回录音原话", "Each line plays back the original words")}</span></li>
      </ul>`;
  },

  model() {
    const box = $("#wlBody");
    const env = S.env;
    const dl = S.jobs.find((j) => j.kind === "models" && isBusy(j));
    if (!env?.models_downloadable) {
      box.innerHTML = `<h2>${t("识别模型", "Speech model")}</h2><p class="hint">${t("第一次转写时会自动下载识别模型（约 2 GB），之后离线可用。", "The speech model downloads automatically on first use (about 2 GB), then works offline.")}</p>`;
      return;
    }
    if (!needsModels()) {
      box.innerHTML = `<h2>${t("识别模型", "Speech model")}</h2><div class="wl-ok">✓ ${t(`已就绪：${env.asr_model ?? "Paraformer-large"}`, `Ready: ${env.asr_model ?? "Paraformer-large"}`)}</div>
        <p class="hint">${
          env.asr_model === "SenseVoice"
            ? t("在设置 → 识别模型里还可以换成 Paraformer-large（纯中文会议更准）或 Fun-ASR-Nano（最准）。", "For all-Chinese meetings you can switch to Paraformer-large or Fun-ASR-Nano (most accurate) in Settings → Speech model.")
            : t("在设置 → 识别模型里还可以换成 SenseVoice（多语种）或 Fun-ASR-Nano（更准）。", "You can switch to SenseVoice (multilingual) or Fun-ASR-Nano (most accurate) in Settings → Speech model.")
        }</p>`;
      return;
    }
    // 默认的 Paraformer 只认中文。界面不是中文的人多半开英文（或日、韩语）会，改下多语种的 SenseVoice、会议语言自动识别
    const multilingual = lang() !== "zh";
    const intro = multilingual
      ? `<p class="hint">One-time download${sizeMb ? ` (~${sizeMb} MB)` : ""}; recognition then works fully offline. Tingo will use <b>SenseVoice</b>, which recognizes English, Chinese, Japanese, Korean and Cantonese, even mixed in one meeting. For all-Chinese meetings you can switch to Paraformer-large in Settings → Speech model.</p>`
      : `<p class="hint">只需下载一次（约 ${env.models_size_mb ?? 330} MB），之后识别全程离线。</p>`;
    box.innerHTML = `
      <h2>${t("下载识别模型", "Download the speech model")}</h2>
      ${intro}
      ${
        dl
          ? `<div class="asr-prog" style="width:100%"><div class="asr-bar"><i id="wlBar"></i></div><small id="wlText">${esc(dl.stage || t("准备下载…", "Starting download…"))}</small></div>`
          : `<button class="btn primary" id="wlDl">${ICON.download}${t("下载模型", "Download")}</button>`
      }`;
    const b = $maybe("#wlDl");
    if (b)
      b.onclick = async () => {
        try {
          const job = multilingual ? await downloadMultilingual() : await api<Job>("/api/models/download", { method: "POST" });
          S.jobs = [...S.jobs, job];
          poll();
          BODY.model();
          watchDownload();
        } catch (e) {
          toast(errorMessage(e), true);
        }
      };
    if (dl) watchDownload();
    if (multilingual && !sizeMb && !dl) void multilingualSize().then((mb) => mb && ((sizeMb = mb), step === "model" && BODY.model()));
  },

  perm() {
    const box = $("#wlBody");
    const icon = (st: string) => (st === "ok" ? "✓" : st === "silent" ? "~" : st === "unsupported" ? "–" : "!");
    const label = (st: string) =>
      ({
        ok: t("正常", "Working"),
        silent: t("录到了，但几乎没声音", "Recorded, but almost silent"),
        denied: t("没有权限", "No permission"),
        unsupported: t("这台 Mac 不支持（需要 macOS 15+）", "Not supported on this Mac (needs macOS 15+)"),
        error: t("出错了", "Error"),
      })[st] ?? st;
    box.innerHTML = `
      <h2>${t("录音权限", "Recording permissions")}</h2>
      <p class="hint">${t(
        "Tingo 要两个权限：「麦克风」录你说的话，「屏幕与系统录音」录会议软件里对方的声音（只录声音，不录画面）。点下面的按钮试录 3 秒，系统会弹窗请求权限。试录时可以随便说句话、放点声音。",
        "Tingo needs two permissions: Microphone for your voice, and Screen & System Audio Recording for the other side of the call (audio only, never the screen). Click below to test for 3 seconds — macOS will ask for permission. Say something and play some audio while it tests.",
      )}</p>
      <button class="btn primary" id="wlTest">${ICON.mic}${t("试录 3 秒", "Test for 3 seconds")}</button>
      ${
        permResult
          ? `<div class="wl-perm">
          <div class="${permResult.system}"><b>${icon(permResult.system)}</b>${t("系统声音", "System audio")}<small>${esc(label(permResult.system))}</small></div>
          <div class="${permResult.mic}"><b>${icon(permResult.mic)}</b>${t("麦克风", "Microphone")}<small>${esc(label(permResult.mic))}</small></div>
        </div>
        ${
          permResult.system === "denied" || permResult.mic === "denied"
            ? `<p class="hint">${t(
                "去「系统设置 → 隐私与安全性」，在「麦克风」和「屏幕与系统录音」里打开 Tingo，然后退出再重新打开 Tingo。之前拒绝过的话，可以在终端运行 <code>tccutil reset Microphone io.github.tancheng33.tingo</code> 和 <code>tccutil reset ScreenCapture io.github.tancheng33.tingo</code> 后重试。",
                "Open System Settings → Privacy & Security, turn Tingo on under Microphone and Screen & System Audio Recording, then quit and reopen Tingo. If you denied it before, run <code>tccutil reset Microphone io.github.tancheng33.tingo</code> and <code>tccutil reset ScreenCapture io.github.tancheng33.tingo</code> in Terminal and try again.",
              )}</p>`
            : ""
        }
        ${permResult.message ? `<p class="hint">${esc(permResult.message)}</p>` : ""}`
          : ""
      }`;
    $("#wlTest").onclick = async () => {
      const b = $<HTMLButtonElement>("#wlTest");
      b.disabled = true;
      b.textContent = t("试录中…", "Testing…");
      try {
        permResult = await api("/api/record/test", { json: { seconds: 3 } });
      } catch (e) {
        permResult = { system: "error", mic: "error", message: errorMessage(e) };
      }
      BODY.perm();
    };
  },

  llm() {
    const env = S.env;
    $("#wlBody").innerHTML = `
      <h2>${t("接上大模型", "Connect an LLM")}</h2>
      <p class="hint">${t(
        "转写不需要大模型，生成纪要需要。DeepSeek、通义千问、OpenAI、Claude、Gemini 填个 API Key 就行；本机的 Magpie、CC Switch、Ollama 会自动识别，选 Ollama 全程不出本机。",
        "Transcription doesn't need an LLM; minutes do. DeepSeek, Qwen, OpenAI, Claude and Gemini just need an API key; local Magpie, CC Switch and Ollama are detected automatically — pick Ollama to stay fully offline.",
      )}</p>
      ${
        env?.llm_ready
          ? `<div class="wl-ok">✓ ${t(`已接上：${esc(env.llm_model)}`, `Connected: ${esc(env.llm_model)}`)}</div>`
          : `<div class="wl-warn">${t("还没有配置大模型", "No LLM configured yet")}</div>`
      }
      <a class="btn" href="#/settings/llm" id="wlLlm">${ICON.bot}${env?.llm_ready ? t("换一个", "Change") : t("去设置", "Set it up")}</a>
      <p class="hint" style="margin-top:12px">${t("设置好后，点设置页顶部的「回到新手引导」接着走。", "When you're done, click \"Back to the tour\" at the top of Settings.")}</p>`;
  },

  demo() {
    $("#wlBody").innerHTML = `
      <h2>${t("先看一场示例会议", "See a sample meeting")}</h2>
      <p class="hint">${t(
        "一场虚构的「注册转化率下降复盘会」：两分钟、四个人，有转写、纪要、待办和一张鱼骨图。点开看看纪要长什么样、点时间戳回听原话。",
        "A fictional two-minute \"signup conversion\" retrospective with four people — transcript, minutes, action items and a fishbone diagram. Open it to see what the minutes look like and click a timestamp to hear the original words.",
      )}</p>
      <button class="btn primary" id="wlDemo">${ICON.file}${t("打开示例会议", "Open the sample meeting")}</button>`;
    $("#wlDemo").onclick = async () => {
      try {
        const r = await api<{ id: string }>("/api/demo", { method: "POST" });
        finish();
        await refreshMeetings();
        location.hash = `#/m/${encodeURIComponent(r.id)}`;
      } catch (e) {
        toast(errorMessage(e), true);
      }
    };
  },
};

/** 引导页里的模型下载进度 */
function watchDownload(): void {
  const tick = () => {
    if (step !== "model" || !$maybe("#wlBody")) return;
    const j = S.jobs.find((x) => x.kind === "models");
    if (!j || !isBusy(j)) {
      refreshEnv().then(() => step === "model" && BODY.model());
      return;
    }
    const m = /([\d.]+)\s*\/\s*([\d.]+)/.exec(j.stage ?? "");
    const bar = $maybe("#wlBar");
    if (bar && m) bar.style.width = `${Math.min(100, (Number(m[1]) / Number(m[2])) * 100).toFixed(1)}%`;
    const txt = $maybe("#wlText");
    if (txt) txt.textContent = j.stage || t("准备下载…", "Starting download…");
    window.setTimeout(tick, 500);
  };
  tick();
}
