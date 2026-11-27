//! 自动结束录音用的检测：有没有人说话的电平门限、正在运行的会议软件。

use std::collections::BTreeSet;

/// 每隔这么多秒检查一次
pub const CHECK_S: u64 = 5;
/// 任一音轨最近 5 秒的电平（track_level 的 0～1 刻度）到这么多就算有人说话
/// 0.4 ≈ -40 dB：电平刻度是 -60 dB→0、-10 dB→1。设得太低（比如 0.04 ≈ -58 dB）时麦克风的底噪就超过了，永远判不出"没人说话"
pub const VOICE_LEVEL: f32 = 0.4;
/// 因为没人说话自动结束时，音轨保留到最后有声之后这么多秒
pub const KEEP_AFTER_S: f64 = 30.0;

/// 会议软件的进程名（比较时忽略大小写；Windows 上不带 .exe）
pub const MEETING_APPS: [&str; 17] = [
    "zoom.us",
    "Zoom",
    "腾讯会议",
    "TencentMeeting",
    "WeMeetApp",
    "Lark",
    "Feishu",
    "飞书",
    "Microsoft Teams",
    "MSTeams",
    "ms-teams",
    "Teams",
    "DingTalk",
    "钉钉",
    "Webex",
    "CiscoCollabHost",
    "Slack",
];

/// 进程列表（`ps -axo comm=` 或整理过的 tasklist，一行一个）里出现的会议软件（小写的进程名）。
/// 每行取最后一段路径来比，Windows 的 .exe 后缀去掉再比。
pub fn meeting_apps_in(ps: &str) -> BTreeSet<String> {
    let wanted: Vec<String> = MEETING_APPS.iter().map(|a| a.to_lowercase()).collect();
    ps.lines()
        .filter_map(|l| {
            let name = l.trim().rsplit(['/', '\\']).next()?.trim().to_lowercase();
            let name = name.strip_suffix(".exe").map(str::to_string).unwrap_or(name);
            wanted.contains(&name).then_some(name)
        })
        .collect()
}

/// 本机正在运行的会议软件；列不出进程时返回空
pub fn running_meeting_apps() -> BTreeSet<String> {
    meeting_apps_in(&crate::platform::process_names())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_meeting_apps_by_last_path_segment() {
        let ps = "/sbin/launchd\n/Applications/zoom.us.app/Contents/MacOS/zoom.us\n/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams\n/Applications/腾讯会议.app/Contents/MacOS/腾讯会议\n/Applications/Slack.app/Contents/Frameworks/Slack Helper (Renderer).app/Contents/MacOS/Slack Helper (Renderer)\n/usr/bin/zoom.usx\n  /Applications/Feishu.app/Contents/MacOS/FEISHU  \n";
        let got: Vec<String> = meeting_apps_in(ps).into_iter().collect();
        assert_eq!(got, vec!["feishu", "msteams", "zoom.us", "腾讯会议"]);
        assert!(meeting_apps_in("").is_empty());
    }

    #[test]
    fn finds_windows_meeting_apps_without_exe_suffix() {
        let names = "System Idle Process\nexplorer.exe\nZoom.exe\nWeMeetApp.exe\nC:\\Program Files\\Feishu\\Feishu.exe\nms-teams.exe\nzoomit.exe";
        let got: Vec<String> = meeting_apps_in(names).into_iter().collect();
        assert_eq!(got, vec!["feishu", "ms-teams", "wemeetapp", "zoom"]);
    }
}
