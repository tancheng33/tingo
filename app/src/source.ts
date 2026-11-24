/* 识别模型从哪儿下：国内镜像还是 GitHub，让用户选，记在本机。
   首页、新手引导、设置 → 识别模型 三个下载入口共用。 */

import { lang, t } from "./i18n";
import { $$ } from "./dom";
import { S } from "./state";

export type Source = "cn" | "github";

const KEY = "tingo.modelSource";

export function modelSource(): Source {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "cn" || v === "github") return v;
  } catch {
    /* 没有本地存储就按界面语言 */
  }
  return lang() === "en" ? "github" : "cn";
}

function setModelSource(v: Source): void {
  try {
    localStorage.setItem(KEY, v);
  } catch {
    /* 记不住也能用，只是下次还按默认 */
  }
}

/** 首次下载的大小（MB），按选的线路 */
export const sourceSizeMb = (src: Source = modelSource()): number =>
  (src === "cn" ? S.env?.models_size_mb_cn : S.env?.models_size_mb) ?? 330;

/** 下载请求带上线路 */
export const downloadInit = (): RequestInit & { json: unknown } => ({ json: { source: modelSource() } });

const hint = (src: Source): string =>
  src === "cn"
    ? t(
        "ModelScope、hf-mirror，国内快。标点模型镜像上只有完整版，所以总共大一些。",
        "ModelScope and hf-mirror — fast in mainland China. Only the full punctuation model is mirrored, so it's a bit larger.",
      )
    : t("sherpa-onnx 官方发布，体积最小；国内可能非常慢。", "Official sherpa-onnx releases — smallest; can be very slow in mainland China.");

export function sourcePicker(): string {
  const cur = modelSource();
  return `
    <div class="src-pick">
      <span>${t("下载线路", "Download from")}</span>
      <div class="seg fit">
        <button data-src="cn" class="${cur === "cn" ? "on" : ""}">${t("国内镜像", "China mirror")}</button>
        <button data-src="github" class="${cur === "github" ? "on" : ""}">GitHub</button>
      </div>
      <small id="srcHint">${hint(cur)}</small>
    </div>`;
}

/** 切换线路后调 `onChange`，用来刷新显示的大小 */
export function bindSourcePicker(root: ParentNode, onChange?: (src: Source) => void): void {
  $$<HTMLButtonElement>(".src-pick [data-src]", root).forEach(
    (b) =>
      (b.onclick = () => {
        const src = b.dataset.src as Source;
        setModelSource(src);
        $$<HTMLButtonElement>(".src-pick [data-src]", root).forEach((x) => x.classList.toggle("on", x === b));
        $$<HTMLElement>(".src-pick #srcHint", root).forEach((h) => (h.textContent = hint(src)));
        onChange?.(src);
      }),
  );
}
