import { $$, main } from "./dom";
import { S } from "./state";
import { renderDetail, stopAudio } from "./views/detail";
import { renderHome } from "./views/home";
import { leaveRecording } from "./views/recording";
import { renderMeetings } from "./views/meetings";
import { renderSettings } from "./views/settings";
import { renderWelcome } from "./views/welcome";

export function route(): void {
  const h = decodeURIComponent(location.hash.slice(1) || "/");
  stopAudio();
  leaveRecording();
  S.detail = null;
  let nav = "home";
  if (h.startsWith("/m/")) {
    nav = "meetings";
    const [id, query] = h.slice(3).split("?");
    const at = Number(new URLSearchParams(query ?? "").get("t"));
    renderDetail(id, false, Number.isFinite(at) && query ? at : undefined);
  } else if (h === "/meetings") {
    nav = "meetings";
    renderMeetings();
  } else if (h.startsWith("/welcome")) {
    renderWelcome(h.split("/")[2]);
  } else if (h === "/settings" || h.startsWith("/settings/")) {
    nav = "settings";
    renderSettings(h.split("/")[2] || "llm");
  } else {
    renderHome();
  }
  $$<HTMLAnchorElement>("#sidebar [data-nav]").forEach((a) => a.classList.toggle("active", a.dataset.nav === nav));
  $$<HTMLAnchorElement>("#recentList a").forEach((a) => a.classList.toggle("active", h.split("?")[0] === `/m/${a.dataset.id}`));
  if (nav === "settings") main().scrollTo({ top: 0, behavior: "instant" });
  else main().scrollTop = 0;
}
