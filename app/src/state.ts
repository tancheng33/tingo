import { isEn, t } from "./i18n";
import type { Env, Job, JobState, MeetingDetail, MeetingSummary } from "./api";

export const S: {
  env: Env | null;
  meetings: MeetingSummary[];
  /** 首页玻璃面板跟随的任务：正在录音的那场，或者首次下载模型；没有就是 idle */
  job: Job;
  /** 所有任务：录音之外，转写、生成纪要、下载模型都在后台跑，可以同时有好几个 */
  jobs: Job[];
  form: { title: string; system: boolean; mic: boolean; hotwords: string };
  detail: MeetingDetail | null; // 当前打开的会议
} = {
  env: null,
  meetings: [],
  job: { state: "idle" },
  jobs: [],
  form: { title: "", system: true, mic: true, hotwords: "" },
  detail: null,
};

const BUSY = new Set<JobState>(["recording", "saving", "finalizing", "loading", "transcribing", "summarizing", "downloading", "merging"]);
export const isBusy = (j: Job | undefined): boolean => !!j && BUSY.has(j.state);

const LABELS: Partial<Record<JobState, [string, string]>> = {
  saving: ["正在保存", "Saving"],
  finalizing: ["转写最后一段", "Transcribing the last part"],
  loading: ["加载识别模型", "Loading the speech model"],
  transcribing: ["正在转写", "Transcribing"],
  summarizing: ["正在生成纪要", "Writing minutes"],
  downloading: ["下载识别模型", "Downloading speech model"],
  merging: ["正在合并", "Merging"],
};
/** 桌面版还没下载识别模型（Python 版会在首次转写时自动下载，不用这个判断）。 */
export const needsModels = (): boolean => !!S.env?.models_downloadable && !S.env.model_cached;

export const stateLabel = (s: JobState): string => {
  const l = LABELS[s];
  return l ? t(...l) : t("处理中", "Working");
};

/** 这场会议正在进行的任务 */
export const jobOf = (meeting: string): Job | undefined => S.jobs.find((j) => j.meeting === meeting && j.kind !== "models" && isBusy(j));

/** 任务标题：会议名（去掉日期时间前缀），或者下载的模型 */
export function jobTitle(j: Job): string {
  if (j.kind === "update") return t("下载新版 Tingo", "Downloading Tingo update");
  if (j.kind === "models") return `${isBusy(j) ? t("下载识别模型", "Downloading speech model") : t("识别模型", "Speech model")}${j.meeting && j.meeting !== "paraformer" ? ` · ${j.meeting}` : ""}`;
  const known = S.meetings.find((m) => m.id === j.meeting)?.title;
  return known || (j.meeting ?? "").replace(/^\d{4}-\d{2}-\d{2}_\d{4}_/, "") || t("会议", "Meeting");
}

/* ---------- 纪要生成进度 ----------
   大模型按模板依次写这几节，写到哪一节就能估出大概进度；"原因分析"只有讨论了原因才有，不算在内。 */
const SECTIONS_ZH = ["一句话总结", "议题与讨论", "决议", "待办", "待定问题与风险"];
const SECTIONS_EN = ["Summary", "Discussion", "Decisions", "Action Items", "Open Questions & Risks"];
/** 进度里显示的章节名（按界面语言）；识别时中英文标题都认 */
export const sectionLabels = (): string[] => (isEn() ? SECTIONS_EN : SECTIONS_ZH);
export const SECTION_COUNT = SECTIONS_ZH.length;

export interface MinutesProgress {
  /** 0～1 */
  value: number;
  /** 当前在写第几节（-1 表示还没开始写，在读转写） */
  section: number;
  chars: number;
  /** 分段提炼的第几段 / 共几段（长会议） */
  chunk?: [number, number];
  merging?: boolean;
}

export function minutesProgress(j: Job): MinutesProgress {
  const text = j.minutes ?? "";
  let section = -1;
  for (const m of text.matchAll(/^##\s+(.+)$/gm)) {
    const h = m[1];
    const i = Math.max(
      SECTIONS_ZH.findIndex((x) => h.startsWith(x)),
      ["会议概述", "议题讨论", "明确决议", "待办", "未决问题"].findIndex(x => h.startsWith(x)),
      SECTIONS_EN.findIndex((x) => h.startsWith(x)),
    );
    if (i > section) section = i;
  }
  const chars = [...text.replace(/\s/g, "")].length;
  // 动笔前（读转写、思考）看不出进度，按用时缓慢增长到 22%；动笔后按写到第几节算
  const reading = 0.02 + 0.2 * (1 - Math.exp(-(j.elapsed ?? 0) / 60));
  const write = section < 0 ? reading : Math.min(0.97, 0.22 + (0.75 * (section + 0.5)) / SECTION_COUNT);
  const chunk = /(?:分段提炼|Summarizing part)\s*(\d+)\/(\d+)/.exec(j.stage ?? "");
  if (chunk) {
    const [i, n] = [Number(chunk[1]), Number(chunk[2])];
    return { value: (0.65 * (i - 1)) / n + 0.02, section: -1, chars: 0, chunk: [i, n] };
  }
  if (/合并|Merging/.test(j.stage ?? "")) return { value: 0.65 + 0.35 * write, section, chars, merging: true };
  return { value: write, section, chars };
}

/** 任务整体进度 0～1：转写按百分比，生成纪要按写到第几节，下载按字节 */
export function progressOf(j: Job): number {
  if (j.state === "done") return 1;
  const pct = (s?: string) => {
    const m = /([\d.]+)%/.exec(s ?? "");
    return m ? Number(m[1]) / 100 : 0;
  };
  if (j.kind === "models" || j.kind === "update") {
    const m = /([\d.]+)\s*\/\s*([\d.]+)/.exec(j.stage ?? "");
    return m ? Number(m[1]) / Number(m[2]) : 0;
  }
  if (j.kind === "summarize") return minutesProgress(j).value;
  // 合并会议：接录音占前一半；有的段没转写时后一半是整段重新转写
  if (j.kind === "merge") return j.state === "merging" ? 0.3 : j.state === "transcribing" ? 0.5 + 0.5 * pct(j.stage) : 0.5;
  // 录音结束后、导入的录音：转写占前一半，纪要占后一半（边录边转的话转写早就做完了，只剩最后一段）
  const live = j.kind === "record";
  switch (j.state) {
    case "saving":
      return 0.03;
    case "finalizing":
      return live ? 0.15 : 0.05;
    case "loading":
      return 0.06;
    case "transcribing":
      return 0.08 + 0.42 * pct(j.stage);
    case "summarizing":
      return (live ? 0.3 : 0.5) + (live ? 0.7 : 0.5) * minutesProgress(j).value;
    default:
      return 0;
  }
}

/** 一句话说清任务现在在干什么 */
export function jobDetail(j: Job): string {
  if (j.state === "error") return j.error || t("没有完成", "Didn't finish");
  if (j.state === "done") return j.kind === "models" || j.kind === "update" ? t("下载完成", "Downloaded") : j.kind === "merge" ? t("已合并", "Merged") : t("纪要已生成", "Minutes ready");
  if (j.state === "merging") return j.stage || stateLabel(j.state);
  if (j.kind === "models" || j.kind === "update") return j.stage || t("准备下载…", "Starting download…");
  if (j.state === "transcribing") return `${t("正在转写", "Transcribing")}${j.stage ? ` ${j.stage}` : ""}`;
  if (j.local && j.state === "summarizing") return `MiniCPM · ${j.stage || "正在生成纪要"} · ${Math.floor(j.elapsed ?? 0)}秒`;
  if (j.state === "summarizing") {
    // 和大模型的连接中途断了，引擎正在从头重新要（阶段文字是「原阶段 · 连接断了，正在重试（1/2）」）
    const retry = /(连接断了.*|Connection dropped.*)$/.exec(j.stage ?? "");
    if (retry) return retry[1];
    const p = minutesProgress(j);
    if (p.chunk) return t(`长会议分段提炼 ${p.chunk[0]}/${p.chunk[1]}`, `Long meeting: summarizing part ${p.chunk[0]}/${p.chunk[1]}`);
    if (p.section < 0)
      return p.merging
        ? t("合并成纪要…", "Merging into minutes…")
        : j.thinking
          ? t(`大模型在思考 · ${j.thinking} 字`, `Thinking · ${j.thinking} chars`)
          : t("大模型在读转写…", "Reading the transcript…");
    return t(`在写「${SECTIONS_ZH[p.section]}」· ${p.chars} 字`, `Writing “${SECTIONS_EN[p.section]}” · ${p.chars} chars`);
  }
  return stateLabel(j.state);
}
