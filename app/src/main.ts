import "./style.css";
import { api, type Config, initApi, isIOS, type Job } from "./api";
import { applyLang, renderShell, t } from "./i18n";
import { checkUpdate } from "./update";
import { shouldOnboard } from "./views/welcome";
import { $, setupModal } from "./dom";
import { poll } from "./poll";
import { route, swipeBackTarget, beginBackPreview } from "./router";
import { refreshShell, setupShell } from "./shell";
import { refreshEnv, refreshMeetings } from "./sidebar";
import { S } from "./state";
import { setupPlayerKeys } from "./views/detail";
import { setupImport } from "./views/home";
import { setupMotion, setupBackGesture } from "./motion";

async function boot(): Promise<void> {
  setupShell();
  try {
    await initApi();
  } catch (e) {
    $("#main").innerHTML = `<div class="page"><div class="empty"><h3>${t("本地服务没有启动", "The local service didn't start")}</h3><div>${String(e)}</div></div></div>`;
    return;
  }
  // 界面语言存在配置里；启动时先用上次记住的语言渲染，拿到配置后不一致再改
  try {
    applyLang(((await api<Config>("/api/config")).ui_language ?? "auto") as "auto");
  } catch {
    /* 拿不到配置就按系统语言 */
  }
  renderShell();
  refreshShell();
  setupModal();
  setupImport();
  setupPlayerKeys();
  window.addEventListener("hashchange", route);
  await Promise.all([refreshEnv(), refreshMeetings()]);
  // iOS：只录麦克风，没有访达、Obsidian、自动更新，导出走系统分享（style.css 里 .ios 相关的规则）
  document.documentElement.classList.toggle("ios", S.env?.platform === "ios");
  setupMotion();
  setupBackGesture(swipeBackTarget, beginBackPreview);
  try {
    S.job = await api<Job>("/api/job");
  } catch {
    /* 服务刚起来 */
  }
  // 第一次打开：先走新手引导
  if (shouldOnboard() && (!location.hash || location.hash === "#/")) location.hash = "#/welcome";
  route();
  poll();
  // 有新版本时侧栏版本号旁边亮起提示（iOS 走 App Store 更新）
  if (!isIOS())
    checkUpdate().then((u) => {
      if (u?.newer) refreshEnv();
    });
}

boot();
