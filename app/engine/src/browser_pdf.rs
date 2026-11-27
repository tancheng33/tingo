//! HTML → PDF：用无界面的 Edge / Chrome 打印（Windows 上每台都有 Edge）。macOS 用录音组件里的 WebKit，不走这里。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::i18n::tr;

/// 找本机的 Edge，没有就找 Chrome
pub fn find_browser() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
                candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
            }
        }
    } else {
        candidates.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
        candidates.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 本地文件路径 → file:// 地址（路径里有中文、空格时要转义）
pub fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !s.starts_with('/') {
        url.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// 用 `browser` 把 HTML 文件打印成 PDF（纸张按 HTML 里的 @page，没写时是浏览器默认）
pub fn print(browser: &Path, html: &Path, out: &Path) -> Result<(), String> {
    let failed = |e: &str| tr(&format!("导出 PDF 失败：{e}"), &format!("PDF export failed: {e}")).to_string();
    // 独立的用户目录：用户自己正开着 Edge 时，不会把任务交给那个窗口然后马上退出
    let profile = std::env::temp_dir().join(format!("tingo-pdf-profile-{}-{}", std::process::id(), nanos()));
    let _ = std::fs::remove_file(out);
    let mut cmd = Command::new(browser);
    cmd.args(["--headless", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--disable-extensions"])
        // 不要页眉页脚（日期、文件路径）；新旧两种写法都给
        .args(["--no-pdf-header-footer", "--print-to-pdf-no-header"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", out.display()))
        .arg(file_url(html))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| failed(&e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _ = std::fs::remove_dir_all(&profile);
    let ok = out.metadata().map(|m| m.len() > 0).unwrap_or(false);
    match status {
        _ if ok => Ok(()),
        None => Err(failed(tr("排版超时", "timed out"))),
        Some(s) => Err(failed(&format!("{s}"))),
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_escape_spaces_and_chinese() {
        assert_eq!(file_url(Path::new("/tmp/a b/纪要.html")), "file:///tmp/a%20b/%E7%BA%AA%E8%A6%81.html");
        assert_eq!(file_url(Path::new(r"C:\Users\张三\x.html")), "file:///C:/Users/%E5%BC%A0%E4%B8%89/x.html");
    }

    /// 本机有 Edge / Chrome 时真打印一份，看纸张是不是 A4、中文路径行不行
    #[test]
    fn prints_a4_pdf_when_a_browser_is_installed() {
        let Some(browser) = find_browser() else { return };
        let dir = std::env::temp_dir().join(format!("tingo-pdf-测试 {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let html = dir.join("纪要.html");
        let body = "<p>会议纪要正文</p>".repeat(200);
        std::fs::write(&html, format!("<!doctype html><meta charset=utf-8><style>@page{{size:A4}}</style><h1>标题</h1>{body}")).unwrap();
        let out = dir.join("纪要.pdf");
        print(&browser, &html, &out).unwrap();
        let pdf = std::fs::read(&out).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        let text = String::from_utf8_lossy(&pdf);
        // A4 是 595.92 × 842.88 点（Chrome 写成小数）
        assert!(text.contains("/MediaBox [0 0 595") || text.contains("/MediaBox [0 0 594"), "not A4");
        let _ = std::fs::remove_dir_all(dir);
    }
}
