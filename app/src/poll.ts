import { localJobs } from "./local-jobs";
/* 轮询本地服务的任务列表，驱动首页（录音 / 首次下载模型）、详情页的生成过程、设置页的模型下载和侧边栏的任务列表。 */

import { t } from "./i18n";
import { api, isIOS, meetingPath, type Job } from "./api";
import { errorMessage, toast } from "./dom";
import { usesLocalMinutes } from "./local-minutes";
import { refreshEnv, refreshMeetings } from "./sidebar";
import { isBusy, jobTitle, needsModels, S } from "./state";
import { renderTasks } from "./tasks";
import { updateDetailJob } from "./views/detail";
import { updateHome } from "./views/home";
import { gapText } from "./views/recording";
import { updateSettingsJob } from "./views/settings";

let timer: number | undefined;
const IDLE: Job = { state: "idle" };

/* 这场录音已经提示过的"没录到"秒数。记在 localStorage 而不是内存里：App 在后台待久了，网页可能被系统回收、
   回来时重新加载，内存里上一次的任务状态就没了，而这正是录音最容易断的时候。 */
const LOST_KEY = "tingo.lost";

function lostSeen(meeting: string): number {
  try {
    const v = JSON.parse(localStorage.getItem(LOST_KEY) ?? "null");
    return v && v.meeting === meeting ? Number(v.s) || 0 : 0;
  } catch {
    return 0;
  }
}

function markLost(meeting: string, s: number): void {
  try {
    localStorage.setItem(LOST_KEY, JSON.stringify({ meeting, s }));
  } catch {
    /* 存不了就算了，最多重复提示一次 */
  }
}

async function fetchJobs(): Promise<Job[]> {
  try {
    return (await api<{ jobs: Job[] }>("/api/jobs")).jobs;
  } catch {
    // 旧版后端只有一个任务
    const j = await api<Job>("/api/job");
    return j.state === "idle" ? [] : [{ ...j, id: j.id ?? "single" }];
  }
}

/** 合并完成：说一声；合成的是一场新会议，还没有纪要——用在线大模型的话接着生成一份。
    手机离线模式要停在前台算很久，不替用户自动开始。 */
async function afterMerge(j: Job): Promise<void> {
  await refreshMeetings();
  const m = S.meetings.find((x) => x.id === j.meeting);
  const title = m?.title ?? jobTitle(j);
  const auto = !!m?.has_transcript && !!S.env?.llm_ready && !(isIOS() && usesLocalMinutes());
  toast(
    auto
      ? t(`已合并为「${title}」，正在生成纪要`, `Merged into “${title}” — writing the minutes`)
      : t(`已合并为「${title}」，打开它可以生成纪要`, `Merged into “${title}” — open it to write the minutes`),
  );
  if (!auto || !j.meeting) return;
  try {
    await api(meetingPath(j.meeting, "/summarize"), { method: "POST" });
    void poll();
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

export async function poll(): Promise<void> {
  window.clearTimeout(timer);
  let jobs: Job[];
  try {
    jobs = [...(await fetchJobs()), ...localJobs()];
  } catch {
    jobs = [...S.jobs.filter(j => !j.local), ...localJobs()];
  }
  const before = new Map(S.jobs.map((j) => [j.id, j]));
  S.jobs = jobs;

  // 首页玻璃面板只跟随录音，以及第一次使用时的模型下载
  const prevFocus = S.job;
  S.job =
    jobs.find((j) => j.state === "recording") ??
    jobs.find((j) => j.kind === "models" && (isBusy(j) || j.state === "error") && needsModels()) ??
    IDLE;
  const onHome = !location.hash || location.hash === "#/";
  if (onHome) updateHome(S.job, prevFocus);
  renderTasks();

  let changed = false;
  for (const j of jobs) {
    const prev = before.get(j.id) ?? IDLE;
    if (S.detail && j.meeting === S.detail.id && j.kind !== "models") updateDetailJob(j, prev);
    if (j.kind === "models") updateSettingsJob(j, prev);
    if (j.kind === "update" && j.state === "error" && isBusy(prev)) toast(j.error ?? "", true);
    // 录音断过又接上了：恢复是自动的，不说的话没人知道中间有一段没录到
    if (j.state === "recording" && j.meeting && (j.lost_s ?? 0) > lostSeen(j.meeting)) {
      const gap = gapText((j.lost_s ?? 0) - lostSeen(j.meeting));
      markLost(j.meeting, j.lost_s ?? 0);
      toast(t(`录音已接上，中间有 ${gap}没录到（详见会议文件夹里的录音日志）`, `Recording resumed — ${gap} wasn't captured (see the recording log in the meeting folder)`), true);
    }
    if (prev.state === j.state || !(j.state === "done" || j.state === "error") || !isBusy(prev)) continue;
    changed = true;
    const here = S.detail?.id === j.meeting;
    if (j.kind === "merge") {
      if (j.state === "done") void afterMerge(j);
      else toast(t(`合并没有完成：${j.error ?? ""}`, `The merge didn't finish: ${j.error ?? ""}`), true);
    } else if (j.kind !== "models" && j.kind !== "update" && !here) {
      if (j.state === "done") toast(t(`「${jobTitle(j)}」的纪要已生成`, `Minutes ready: ${jobTitle(j)}`));
      else toast(t(`「${jobTitle(j)}」没有处理完：${j.error ?? ""}`, `${jobTitle(j)} didn't finish: ${j.error ?? ""}`), true);
    }
    if (j.state === "done" && j.notice) toast(j.notice, /^(没能|Couldn't)/.test(j.notice));
  }
  if (changed) {
    refreshMeetings();
    // 先拿到最新状态再重画首页：模型刚下完时首页已经按旧状态（"还没下载"）画过一次，不重画就会一直显示"下载模型"
    refreshEnv().then(() => {
      if ((!location.hash || location.hash === "#/") && S.job.state === "idle") updateHome(S.job, IDLE, true);
    });
  }
  const rec = jobs.some((j) => j.state === "recording");
  timer = window.setTimeout(poll, rec ? 160 : jobs.some(isBusy) ? 500 : 2500);
}
