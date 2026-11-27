//! 检查更新（只桌面版）：读 GitHub 上最新的 Release，找本平台的安装包（macOS 是 .dmg，Windows 是 NSIS
//! 的 -setup.exe）和 checksums.txt 里的 sha256。桌面 App 走 Tauri 自带的签名更新，这里是浏览器入口的兜底。

use std::cmp::Ordering;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const LATEST_URL: &str = "https://api.github.com/repos/tancheng33/tingo/releases/latest";
const MANIFEST_URL: &str = "https://github.com/tancheng33/tingo/releases/latest/download/latest.json";
const RELEASE_BASE: &str = "https://github.com/tancheng33/tingo/releases";
const MAX_NOTES: usize = 3000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub latest: String,
    pub url: String,
    pub notes: String,
    /// 安装包下载地址和文件名
    pub dmg: Option<String>,
    pub dmg_name: String,
    pub size: u64,
    pub sha256: Option<String>,
}

impl Release {
    pub fn newer_than(&self, current: &str) -> bool {
        compare(&self.latest, current) == Some(Ordering::Greater)
    }

    pub fn to_json(&self, current: &str) -> Value {
        json!({
            "current": current,
            "latest": self.latest,
            "newer": self.newer_than(current),
            "url": self.url,
            "notes": self.notes,
            "dmg": self.dmg,
            "size_mb": (self.size as f64 / 1e6).round() as u64,
            "sha256": self.sha256,
        })
    }
}

/// "v1.2.3-beta.1" → ([1, 2, 3], Some("beta.1"))；认不出返回 None
fn parse(v: &str) -> Option<(Vec<u64>, Option<String>)> {
    let v = v.trim();
    let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let v = v.split('+').next()?; // 构建元数据不参与比较
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let mut nums: Vec<u64> = core.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if nums.is_empty() || nums.len() > 3 {
        return None;
    }
    nums.resize(3, 0);
    Some((nums, pre))
}

/// 语义化版本比较（去掉前缀 v）：先比主、次、修订号；相同时正式版大于预发布版，预发布按点分段比。
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (na, pa) = parse(a)?;
    let (nb, pb) = parse(b)?;
    Some(na.cmp(&nb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split('.').collect(), y.split('.').collect());
            for (p, q) in xs.iter().zip(&ys) {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    _ => p.cmp(q),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            xs.len().cmp(&ys.len())
        }
    }))
}

/// checksums.txt（`shasum -a 256` 的输出）里某个文件的 sha256
pub fn checksum_for(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let (sum, file) = (parts.next()?, parts.next()?);
        let file = file.trim_start_matches('*'); // 二进制模式的输出带 *
        (file == name && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit())).then(|| sum.to_ascii_lowercase())
    })
}

const INSTALLER_SUFFIX: &str = if cfg!(windows) { "-setup.exe" } else { ".dmg" };

/// 发布脚本给本平台安装包起的名字
fn installer_name(version: &str, arch: &str) -> String {
    format!("Tingo_{version}_{arch}{INSTALLER_SUFFIX}")
}

/// 本平台的安装包；有多个时优先选本机架构的
fn pick_installer(assets: &[Value]) -> Option<&Value> {
    let found: Vec<&Value> = assets
        .iter()
        .filter(|a| a["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().ends_with(INSTALLER_SUFFIX)))
        .collect();
    let arch: &[&str] = if std::env::consts::ARCH == "aarch64" { &["aarch64", "arm64"] } else { &["x86_64", "x64", "intel"] };
    found.iter()
        .find(|a| {
            let n = a["name"].as_str().unwrap_or("").to_ascii_lowercase();
            arch.iter().any(|k| n.contains(k))
        })
        .or(found.first())
        .copied()
}

/// 解析 GitHub releases/latest 的返回；checksums 是 checksums.txt 的内容（有的话）
pub fn parse_release(v: &Value, checksums: Option<&str>) -> Option<Release> {
    let tag = v["tag_name"].as_str()?;
    let latest = tag.trim().trim_start_matches(['v', 'V']).to_string();
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let dmg = pick_installer(&assets);
    let dmg_name = dmg.and_then(|a| a["name"].as_str()).unwrap_or("").to_string();
    Some(Release {
        latest,
        url: v["html_url"].as_str().unwrap_or("").to_string(),
        notes: v["body"].as_str().unwrap_or("").chars().take(MAX_NOTES).collect(),
        dmg: dmg.and_then(|a| a["browser_download_url"].as_str()).map(String::from),
        size: dmg.and_then(|a| a["size"].as_u64()).unwrap_or(0),
        sha256: checksums.and_then(|c| checksum_for(c, &dmg_name)),
        dmg_name,
    })
}

/// checksums.txt 的下载地址
fn checksums_url(v: &Value) -> Option<String> {
    v["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some("checksums.txt"))?["browser_download_url"]
        .as_str()
        .map(String::from)
}

pub fn client(version: &str, timeout: Option<Duration>) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .user_agent(format!("Tingo/{version}"))
        .build()
        .map_err(|e| e.to_string())
}

/// API 限流或不可达时，使用公开 Release 附件；不需要用户提供 GitHub token。
pub fn fetch_latest(version: &str) -> Result<Release, String> {
    let c = client(version, Some(Duration::from_secs(15)))?;
    fetch_latest_with(|url| {
        c.get(url).send().map_err(|e| e.to_string())?
            .error_for_status().map_err(|e| e.to_string())?
            .text().map_err(|e| e.to_string())
    })
}

fn fetch_latest_with(mut get: impl FnMut(&str) -> Result<String, String>) -> Result<Release, String> {
    let api = (|| {
        let v: Value = serde_json::from_str(&get(LATEST_URL)?).map_err(|e| e.to_string())?;
        let sums = checksums_url(&v).and_then(|u| get(&u).ok());
        parse_release(&v, sums.as_deref()).ok_or_else(|| "GitHub: no tag_name".to_string())
    })();
    match api {
        Ok(release) => Ok(release),
        Err(api_error) => (|| {
            let manifest: Value = serde_json::from_str(&get(MANIFEST_URL)?).map_err(|e| e.to_string())?;
            let version = manifest_version(&manifest)?;
            // Pin checksum and DMG URLs to the manifest version, avoiding a latest-release race.
            let sums = get(&format!("{RELEASE_BASE}/download/v{version}/checksums.txt"))?;
            release_from_manifest(&manifest, &sums)
        })().map_err(|fallback: String| format!(
            "无法检查更新，请稍后重试或访问 {RELEASE_BASE}/latest / Update check failed. API: {api_error}; release manifest: {fallback}"
        )),
    }
}

fn manifest_version(v: &Value) -> Result<&str, String> {
    let version = v["version"].as_str().ok_or("Missing update version")?;
    if parse(version).is_none() || !version.bytes().all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b)) {
        return Err("Invalid update version".into());
    }
    Ok(version)
}

fn release_from_manifest(v: &Value, sums: &str) -> Result<Release, String> {
    let version = manifest_version(v)?;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x64",
        _ => return Err("Unsupported update architecture".into()),
    };
    let name = installer_name(version, arch);
    let sha256 = checksum_for(sums, &name).ok_or("No verified installer for this architecture")?;
    Ok(Release {
        latest: version.into(),
        url: format!("{RELEASE_BASE}/tag/v{version}"),
        notes: v["notes"].as_str().unwrap_or("").chars().take(MAX_NOTES).collect(),
        dmg: Some(format!("{RELEASE_BASE}/download/v{version}/{name}")),
        dmg_name: name,
        sha256: Some(sha256),
        size: 0,
    })
}

/// 文件的 sha256（小写十六进制）
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_rate_limit_falls_back_to_pinned_release_assets() {
        let arch = if std::env::consts::ARCH == "aarch64" { "aarch64" } else { "x64" };
        let mut urls = Vec::new();
        let r = fetch_latest_with(|url| {
            urls.push(url.to_string());
            if url == LATEST_URL { return Err("403 Forbidden: rate limit".into()); }
            if url == MANIFEST_URL { return Ok(r#"{"version":"0.5.2","notes":"fixed"}"#.into()); }
            assert_eq!(url, format!("{RELEASE_BASE}/download/v0.5.2/checksums.txt"));
            Ok(format!("{}  {}", "a".repeat(64), installer_name("0.5.2", arch)))
        }).unwrap();
        assert_eq!(urls.len(), 3);
        assert_eq!(r.latest, "0.5.2");
        assert_eq!(r.sha256, Some("a".repeat(64)));
        assert!(r.dmg.unwrap().contains("/download/v0.5.2/"));
    }

    #[test]
    fn manifest_fallback_requires_valid_version_and_matching_checksum() {
        assert!(release_from_manifest(&json!({"version":"../../bad"}), "").is_err());
        assert!(release_from_manifest(&json!({"version":"0.5.2"}), "").is_err());
        let err = fetch_latest_with(|_| Err("403 Forbidden".into())).unwrap_err();
        assert!(err.contains("/releases/latest"));
        assert!(err.contains("release manifest"));
    }

    #[test]
    fn semver_compare() {
        use Ordering::*;
        assert_eq!(compare("0.3.0", "0.2.0"), Some(Greater));
        assert_eq!(compare("v0.2.0", "0.2.0"), Some(Equal));
        assert_eq!(compare("0.10.0", "0.9.9"), Some(Greater)); // 按数字比，不按字符串
        assert_eq!(compare("1.0", "1.0.0"), Some(Equal));
        assert_eq!(compare("0.3.0-beta.1", "0.3.0"), Some(Less));
        assert_eq!(compare("0.3.0-beta.2", "0.3.0-beta.10"), Some(Less));
        assert_eq!(compare("0.3.0-rc.1", "0.3.0-beta.1"), Some(Greater));
        assert_eq!(compare("0.3.0+build5", "0.3.0"), Some(Equal));
        assert_eq!(compare("latest", "0.2.0"), None);
        let r = Release { latest: "0.3.0".into(), ..Default::default() };
        assert!(r.newer_than("0.2.0") && !r.newer_than("0.3.0") && !r.newer_than("0.4.0"));
    }

    #[test]
    fn release_json_and_checksums() {
        let name = installer_name("0.3.0", "aarch64");
        let sums = format!("0123456789abcdef0123456789abcdef0123456789abcdef0123456789ABCDEF  {name}\nffff  other.dmg\n");
        let sums = sums.as_str();
        assert_eq!(
            checksum_for(sums, &name).as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(checksum_for(sums, "other.dmg"), None);
        let v = json!({
            "tag_name": "v0.3.0", "html_url": "https://github.com/tancheng33/tingo/releases/tag/v0.3.0",
            "body": "x".repeat(5000),
            "assets": [
                {"name": "checksums.txt", "browser_download_url": "https://e/checksums.txt", "size": 100},
                {"name": "Tingo_0.3.0_x64-setup.exe.sig", "browser_download_url": "https://e/a.sig", "size": 400},
                {"name": "Tingo.app.tar.gz", "browser_download_url": "https://e/a.tar.gz", "size": 12_000_000},
                {"name": name, "browser_download_url": "https://e/installer", "size": 12_400_000},
            ],
        });
        let r = parse_release(&v, Some(sums)).unwrap();
        let j = r.to_json("0.2.0");
        assert_eq!(j["latest"], "0.3.0");
        assert_eq!(j["newer"], true);
        assert_eq!(j["dmg"], "https://e/installer");
        assert_eq!(j["size_mb"], 12);
        assert_eq!(j["notes"].as_str().unwrap().chars().count(), 3000);
        assert!(j["sha256"].as_str().unwrap().starts_with("0123"));
        assert_eq!(checksums_url(&v).as_deref(), Some("https://e/checksums.txt"));
        let bare = parse_release(&json!({"tag_name": "0.1.0"}), None).unwrap();
        assert_eq!((bare.dmg, bare.sha256), (None, None));
        assert!(parse_release(&json!({}), None).is_none());
    }
}
