//! 各平台不一样的系统能力：用户目录、磁盘剩余空间、用系统程序打开文件、列出正在运行的进程。
//! macOS 沿用原来的做法；Windows 只用系统自带的 API 和程序。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 用户主目录。Windows 通常没有 HOME，用 USERPROFILE。
pub fn home_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        var("USERPROFILE").or_else(|| var("HOME"))
    } else {
        var("HOME")
    }
}

/// 把开头的 `~/`（Windows 上也认 `~\`）换成主目录；不以它开头时原样返回。
pub fn expand_home(input: &str) -> Option<PathBuf> {
    let rest = input.strip_prefix("~/").or_else(|| if cfg!(windows) { input.strip_prefix("~\\") } else { None });
    match rest {
        Some(rest) => home_dir().map(|h| h.join(rest)),
        None => Some(PathBuf::from(input)),
    }
}

/// 路径所在磁盘的剩余空间（普通用户可用的）。路径还不存在时看最近一层存在的上级目录。
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let existing = path.ancestors().find(|p| p.exists())?;
    let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // 各平台字段类型不一样
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

/// 路径所在磁盘的剩余空间（普通用户可用的）。路径还不存在时看最近一层存在的上级目录。
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    // GetDiskFreeSpaceExW 只收目录
    let existing = path.ancestors().find(|p| p.is_dir())?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(available)
}

#[cfg(not(any(unix, windows)))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// 用系统默认程序打开文件、文件夹或链接（如 obsidian://）。打不开时静默忽略。
pub fn open(target: impl AsRef<OsStr>) {
    let program = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program).arg(target).spawn();
}

/// 在文件管理器里显示并选中这个文件（访达 / 资源管理器）。
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer 只认 `/select,"路径"` 这种写法；整段被加上引号时会忽略选中，打开「文档」
        let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        open(path.parent().unwrap_or(path));
    }
}

/// 正在运行的进程名，一行一个（可能带路径）。列不出来时返回空字符串。
pub fn process_names() -> String {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000; // 从窗口程序里调用控制台程序时不闪黑框
        Command::new("tasklist")
            .args(["/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| tasklist_names(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        Command::new("/bin/ps")
            .args(["-axo", "comm="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }
}

/// `tasklist /fo csv /nh` 的输出 → 一行一个进程名（第一列，去掉引号）
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn tasklist_names(csv: &str) -> String {
    csv.lines()
        .filter_map(|l| l.trim().strip_prefix('"')?.split('"').next())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_process_names_from_tasklist_csv() {
        let csv = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\"Zoom.exe\",\"1234\",\"Console\",\"1\",\"123,456 K\"\r\n\r\n\"WeMeetApp.exe\",\"88\",\"Console\",\"1\",\"9 K\"\r\n";
        assert_eq!(tasklist_names(csv), "System Idle Process\nZoom.exe\nWeMeetApp.exe");
        assert_eq!(tasklist_names(""), "");
    }

    #[test]
    fn expands_home_prefix_only() {
        let home = home_dir().unwrap();
        assert_eq!(expand_home("~/Notes").unwrap(), home.join("Notes"));
        assert_eq!(expand_home("/tmp/x").unwrap(), PathBuf::from("/tmp/x"));
        assert_eq!(expand_home("a~/b").unwrap(), PathBuf::from("a~/b"));
    }

    #[test]
    fn free_bytes_walks_up_to_an_existing_folder() {
        let tmp = std::env::temp_dir();
        assert!(free_bytes(&tmp).is_some_and(|n| n > 0));
        assert!(free_bytes(&tmp.join("tingo-not-created/yet")).is_some());
    }
}
