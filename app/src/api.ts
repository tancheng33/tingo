/* 和本地服务通信。浏览器 / `tingo ui` 里是同源；Tauri 窗口里服务地址由外壳告诉我们。 */

import { lang, t } from "./i18n";

export interface Env {
  version: string;
  /** macos | ios | linux…（桌面版 Rust 后端） */
  platform?: string;
  root: string;
  ffmpeg: boolean;
  system_audio: boolean;
  model_cached: boolean;
  model_loaded: boolean;
  llm_ready: boolean;
  llm_model: string;
  /** 桌面版（Rust 后端）：模型在界面里下载 */
  models_downloadable?: boolean;
  models_size_mb?: number;
  /** 走国内镜像时首次下载的大小 */
  models_size_mb_cn?: number;
  /** 当前识别模型的名字（桌面版） */
  asr_model?: string;
  /** 后端能合并会议、接着录（Rust 后端；Python 版没有，界面不显示入口） */
  merge?: boolean;
}

export interface MeetingSummary {
  id: string;
  title: string;
  date: string;
  time: string;
  summary: string;
  has_minutes: boolean;
  has_transcript: boolean;
  duration: string;
  speakers: string[];
  /** 会议管理（新版后端才有） */
  tags?: string[];
  archived?: boolean;
  size_bytes?: number;
  has_audio?: boolean;
  /** 录音时 App 被系统关掉、恢复出来的会议：可以「接着录」 */
  interrupted?: boolean;
}

export interface Turn {
  speaker: string;
  color: number;
  start_ms: number;
  ts: string;
  text: string;
}

export interface SpeakerInfo {
  index: number;
  name: string;
  custom: boolean;
}

export interface MeetingDetail extends MeetingSummary {
  minutes: string;
  turns: Turn[];
  speaker_list: SpeakerInfo[];
  audio: boolean;
  path: string;
  /** 录音中标记的重点时刻（毫秒） */
  marks?: number[];
}

export type JobState =
  | "idle"
  | "recording"
  | "saving"
  | "finalizing"
  | "loading"
  | "transcribing"
  | "summarizing"
  | "downloading"
  | "merging"
  | "done"
  | "error";

export interface Job {
  local?: boolean;
  /** 任务编号（桌面版和新版 Python 后端都有；旧接口只有一个任务时为空） */
  id?: string;
  state: JobState;
  kind?: "record" | "process" | "summarize" | "models" | "update" | "merge";
  meeting?: string;
  error?: string;
  minutes?: string;
  stage?: string;
  sources?: string[];
  /** 开始时间（Unix 秒） */
  started_at?: number;
  elapsed?: number;
  model_loaded?: boolean;
  levels?: Record<string, number>;
  /** 边录边转最近几句；speaker 是录音中的临时编号（结束后整场重新区分说话人） */
  live?: { start_ms: number; speaker?: number | null; text: string }[];
  notice?: string;
  /** 推理模型已经思考了多少字 */
  thinking?: number;
  /** 录音中：距最后有人说话过了多少秒；还有多少秒自动结束 */
  silent_s?: number;
  auto_stop_s?: number;
  /** 录音中：录音暂时停着（麦克风被来电或别的 App 占用、App 被挂起过）已经多少秒；没停时没有这个字段 */
  paused_s?: number;
  /** 录音中：之前暂停过、已经接上的时间一共多少秒（这些时间没录到） */
  lost_s?: number;
  /** 这个任务占着的别的会议：合并任务是正在被合并的那几场；「接着录」的录音是要接上去的那一场 */
  merging?: string[];
}

export interface Config {
  base_url: string;
  model: string;
  api_key_set: boolean;
  api_key_hint: string;
  hotwords: string[];
  webhook: string;
  secret_set: boolean;
  path: string;
  obsidian: ObsidianConfig;
  /** 接口协议：openai（OpenAI 兼容）| anthropic */
  api?: string;
  /** 采样温度，null = 默认 0.2 */
  temperature?: number | null;
  /** 写纪要时让模型深度思考，默认关 */
  think?: boolean;
  /** 开会时实时整理纪要，默认开 */
  live_minutes?: boolean;
  /** auto | zh | en */
  ui_language?: string;
  minutes_language?: string;
  /** zh | en | auto（会议语言，桌面版识别用） */
  asr_language?: string;
  /** 连续多少分钟没人说话自动结束录音，0 = 不自动结束 */
  auto_stop_silence_min?: number;
  /** 会议软件退出时自动结束录音 */
  auto_stop_app_quit?: boolean;
}

export interface ObsidianConfig {
  vault: string;
  folder: string;
  auto: boolean;
  transcript: boolean;
}

export interface LocalProvider {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  recommended: string;
  hint: string;
  running: boolean;
  models: string[];
}

let base = "";

/** Tauri 窗口里向外壳要本地服务地址；外壳会等服务起来再返回。 */
export async function initApi(): Promise<void> {
  if ("__TAURI_INTERNALS__" in window) {
    const { invoke } = await import("@tauri-apps/api/core");
    base = await invoke<string>("backend_url");
  }
}

export const url = (path: string): string => base + path;

export const isIOS = (): boolean => document.documentElement.classList.contains("ios");

/** iOS 把 App 挂起后，系统可能收回本地服务监听的 socket：请求连不上时让外壳重新监听一次，录音和任务都还在。 */
let reconnecting: Promise<boolean> | null = null;
function reconnect(): Promise<boolean> {
  if (!("__TAURI_INTERNALS__" in window)) return Promise.resolve(false);
  reconnecting ??= import("@tauri-apps/api/core")
    .then(({ invoke }) => invoke<string>("backend_restart"))
    .then((u) => {
      base = u;
      return true;
    })
    .catch(() => false)
    .finally(() => setTimeout(() => (reconnecting = null), 1000));
  return reconnecting;
}

async function send(path: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url(path), init);
  } catch (e) {
    // 只有连不上（TypeError）才重连；FormData 之类的请求体可以再发一次
    if (e instanceof TypeError && (await reconnect())) return fetch(url(path), init);
    throw e;
  }
}

/** iOS：用系统分享面板发出导出的文件（存到「文件」、微信、AirDrop） */
export async function share(path: string): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("plugin:recorder|share", { path });
}

interface ApiOptions extends RequestInit {
  json?: unknown;
}

export async function api<T = Record<string, unknown>>(path: string, opts: ApiOptions = {}): Promise<T> {
  const { json, ...init } = opts;
  const headers: Record<string, string> = { "X-Tingo-Lang": lang() }; // 后端按这个语言回提示
  if (json !== undefined) {
    init.method = init.method || "POST";
    headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(json);
  }
  init.headers = headers;
  const res = await send(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(typeof data.detail === "string" ? data.detail : t(`请求失败（${res.status}）`, `Request failed (${res.status})`));
  return data as T;
}

export const meetingPath = (id: string, sub = ""): string => `/api/meetings/${encodeURIComponent(id)}${sub}`;
