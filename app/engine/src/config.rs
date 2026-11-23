//! 配置：~/.config/tingo/config.toml，和 Python 版共用同一个文件、同一种格式。

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 接口协议："openai"（OpenAI 兼容的 chat/completions，默认）或 "anthropic"（Claude 的 messages）
    pub api: String,
    /// [llm] temperature：采样温度（0～2），None = 默认 0.2。Kimi 等只接受 1 的模型可以改成 1
    pub temperature: Option<f64>,
}

/// 设置里能填的温度范围
pub fn valid_temperature(t: f64) -> bool {
    t.is_finite() && (0.0..=2.0).contains(&t)
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            api_key: String::new(),
            model: "deepseek-chat".into(),
            api: "openai".into(),
            temperature: None,
        }
    }
}

/// 存进 Obsidian：库的路径、库里的文件夹、生成纪要后是否自动存、是否附带转写全文
#[derive(Clone, Debug, PartialEq)]
pub struct ObsidianConfig {
    pub vault: String,
    pub folder: String,
    pub auto: bool,
    pub transcript: bool,
}

impl Default for ObsidianConfig {
    fn default() -> Self {
        Self { vault: String::new(), folder: "会议纪要".into(), auto: true, transcript: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub storage_root: String,
    pub llm: LlmConfig,
    pub hotwords: Vec<String>,
    pub webhook: String,
    pub webhook_secret: String,
    pub obsidian: ObsidianConfig,
    /// [ui] language：auto | zh | en，界面语言（auto 由前端按系统语言决定）
    pub ui_language: String,
    /// [minutes] language：auto | zh | en，纪要语言；auto = 跟会议转写的语言
    pub minutes_language: String,
    /// [asr] language：zh | en | auto，会议语言（SenseVoice / Fun-ASR-Nano 用；Paraformer 只认中文）
    pub asr_language: String,
    /// [record] auto_stop_silence_min：连续这么多分钟没人说话就自动结束录音，0 表示不自动结束
    pub auto_stop_silence_min: u32,
    /// [record] auto_stop_app_quit：开始录音时在运行的会议软件全部退出后自动结束录音
    pub auto_stop_app_quit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage_root: String::new(),
            llm: LlmConfig::default(),
            hotwords: Vec::new(),
            webhook: String::new(),
            webhook_secret: String::new(),
            obsidian: ObsidianConfig::default(),
            ui_language: "auto".into(),
            minutes_language: "auto".into(),
            asr_language: "zh".into(),
            auto_stop_silence_min: DEFAULT_SILENCE_MIN,
            auto_stop_app_quit: true,
        }
    }
}

pub const DEFAULT_SILENCE_MIN: u32 = 10;
/// 没人说话自动结束录音的分钟数上限
pub const MAX_SILENCE_MIN: u32 = 240;

/// 三个语言选项都只认 auto / zh / en
pub const LANGUAGES: [&str; 3] = ["auto", "zh", "en"];

fn language(v: Option<String>, default: &str) -> String {
    v.map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| LANGUAGES.contains(&s.as_str()))
        .unwrap_or_else(|| default.into())
}

#[derive(Deserialize, Default)]
struct RawLlm {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    api: Option<String>,
    // 写错类型（比如写成字符串）不能让整个配置读不出来，所以先按任意值读
    temperature: Option<toml::Value>,
    hotwords: Option<Vec<String>>, // 早期模板误把 hotwords 写在 [llm] 下
}

#[derive(Deserialize, Default)]
struct RawPush {
    webhook: Option<String>,
    secret: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawObsidian {
    vault: Option<String>,
    folder: Option<String>,
    auto: Option<bool>,
    transcript: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawLang {
    language: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawRecord {
    auto_stop_silence_min: Option<i64>,
    auto_stop_app_quit: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawStorage {
    root: Option<String>,
}

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    storage: RawStorage,
    hotwords: Option<Vec<String>>,
    #[serde(default)]
    llm: RawLlm,
    #[serde(default)]
    push: RawPush,
    #[serde(default)]
    obsidian: RawObsidian,
    #[serde(default)]
    ui: RawLang,
    #[serde(default)]
    minutes: RawLang,
    #[serde(default)]
    asr: RawLang,
    #[serde(default)]
    record: RawRecord,
}

/// 读环境变量；改名前的 JIYAO_ 前缀照样认
pub fn env_var(name: &str) -> Option<String> {
    [format!("TINGO_{name}"), format!("JIYAO_{name}")]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// 新位置不存在而改名前（jiyao）的位置存在时，沿用旧位置，老用户的配置和会议不用搬
pub fn with_legacy(new: PathBuf, old: PathBuf) -> PathBuf {
    if !new.exists() && old.exists() {
        old
    } else {
        new
    }
}

/// 接口协议只认 openai / anthropic，其它值（包括没写）都当 openai
pub fn normalize_api(v: Option<&str>) -> String {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("anthropic") => "anthropic".into(),
        _ => "openai".into(),
    }
}

pub fn default_path() -> PathBuf {
    if let Some(p) = env_var("CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    with_legacy(home.join(".config/tingo/config.toml"), home.join(".config/jiyao/config.toml"))
}

/// 读配置；环境变量优先（与 Python 版相同的几个）。文件不存在或格式不对时用默认值。
pub fn load(path: &Path) -> Config {
    let raw: Raw = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    let d = LlmConfig::default();
    let mut cfg = Config {
        storage_root: raw.storage.root.unwrap_or_default(),
        llm: LlmConfig {
            base_url: raw.llm.base_url.unwrap_or(d.base_url),
            api_key: raw.llm.api_key.unwrap_or_default(),
            model: raw.llm.model.unwrap_or(d.model),
            api: normalize_api(raw.llm.api.as_deref()),
            temperature: raw
                .llm
                .temperature
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .filter(|t| valid_temperature(*t)),
        },
        hotwords: raw.hotwords.or(raw.llm.hotwords).unwrap_or_default(),
        webhook: raw.push.webhook.unwrap_or_default(),
        webhook_secret: raw.push.secret.unwrap_or_default(),
        obsidian: {
            let d = ObsidianConfig::default();
            ObsidianConfig {
                vault: raw.obsidian.vault.unwrap_or(d.vault),
                folder: raw.obsidian.folder.unwrap_or(d.folder),
                auto: raw.obsidian.auto.unwrap_or(d.auto),
                transcript: raw.obsidian.transcript.unwrap_or(d.transcript),
            }
        },
        ui_language: language(raw.ui.language, "auto"),
        minutes_language: language(raw.minutes.language, "auto"),
        asr_language: language(raw.asr.language, "zh"),
        auto_stop_silence_min: raw
            .record
            .auto_stop_silence_min
            .filter(|m| (0..=MAX_SILENCE_MIN as i64).contains(m))
            .map(|m| m as u32)
            .unwrap_or(DEFAULT_SILENCE_MIN),
        auto_stop_app_quit: raw.record.auto_stop_app_quit.unwrap_or(true),
    };
    let env = |k: &str| match k.strip_prefix("TINGO_") {
        Some(name) => env_var(name),
        None => std::env::var(k).ok().filter(|v| !v.is_empty()),
    };
    if let Some(v) = env("TINGO_LLM_BASE_URL") {
        cfg.llm.base_url = v;
    }
    if let Some(v) = env("TINGO_LLM_MODEL") {
        cfg.llm.model = v;
    }
    if let Some(v) = env("TINGO_LLM_API") {
        cfg.llm.api = normalize_api(Some(&v));
    }
    if let Some(v) = env("TINGO_LLM_API_KEY") {
        cfg.llm.api_key = v;
    } else if cfg.llm.api_key.is_empty() {
        cfg.llm.api_key = env("DEEPSEEK_API_KEY").unwrap_or_default();
    }
    if let Some(v) = env("TINGO_WEBHOOK") {
        cfg.webhook = v;
    }
    if let Some(v) = env("TINGO_WEBHOOK_SECRET") {
        cfg.webhook_secret = v;
    }
    cfg
}

fn quote(s: &str) -> String {
    // TOML 基本字符串和 JSON 字符串的转义规则兼容
    serde_json::to_string(s).unwrap()
}

/// 写回配置（设置页用）。hotwords 必须写在所有表之前，否则会被当成 [llm] 的配置。
pub fn save(path: &Path, cfg: &Config) -> std::io::Result<()> {
    let words: Vec<String> = cfg.hotwords.iter().map(|w| quote(w)).collect();
    let text = format!(
        "# tingo 配置，由图形界面写入；也可以直接编辑。\n\n# 专有名词、人名、项目代号，提高识别准确率\nhotwords = [{}]\n\n[llm]\nbase_url = {}\napi_key = {}\nmodel = {}\napi = {}\n{}\n[push]\nwebhook = {}\nsecret = {}\n\n[obsidian]\nvault = {}\nfolder = {}\nauto = {}\ntranscript = {}\n\n[ui]\nlanguage = {}\n\n[minutes]\nlanguage = {}\n\n[asr]\nlanguage = {}\n\n[record]\nauto_stop_silence_min = {}\nauto_stop_app_quit = {}\n\n[storage]\nroot = {}\n",
        words.join(", "),
        quote(&cfg.llm.base_url),
        quote(&cfg.llm.api_key),
        quote(&cfg.llm.model),
        quote(&cfg.llm.api),
        cfg.llm.temperature.map(|t| format!("temperature = {t:?}\n")).unwrap_or_default(),
        quote(&cfg.webhook),
        quote(&cfg.webhook_secret),
        quote(&cfg.obsidian.vault),
        quote(&cfg.obsidian.folder),
        cfg.obsidian.auto,
        cfg.obsidian.transcript,
        quote(&cfg.ui_language),
        quote(&cfg.minutes_language),
        quote(&cfg.asr_language),
        cfg.auto_stop_silence_min,
        cfg.auto_stop_app_quit,
        quote(&cfg.storage_root),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    static NEXT_SAVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!("toml.{}.{}.tmp", std::process::id(), NEXT_SAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let result = (|| {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600); // API keys must never be world-readable, including the temporary file.
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tingo-cfg-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn roundtrip_and_python_compat() {
        let p = tmp("rt");
        let cfg = Config {
            llm: LlmConfig {
                base_url: "http://127.0.0.1:3425/v1".into(),
                api_key: "k\"q".into(),
                model: "deepseek/deepseek-v4-flash".into(),
                api: "anthropic".into(),
                temperature: Some(1.0),
            },
            storage_root: "/Volumes/会议资料/Tingo".into(),
            hotwords: vec!["星河".into(), "Kafka".into()],
            webhook: "".into(),
            webhook_secret: "".into(),
            obsidian: ObsidianConfig { vault: "/Users/a/笔记".into(), folder: "会议".into(), auto: false, transcript: true },
            ..Default::default()
        };
        save(&p, &cfg).unwrap();
        assert_eq!(load(&p).storage_root, cfg.storage_root);
        assert_eq!(load(&p).llm, cfg.llm);
        assert_eq!(load(&p).obsidian, cfg.obsidian);
        assert_eq!(load(&p).hotwords, cfg.hotwords);
        // Python 版写出的文件
        std::fs::write(&p, "hotwords = [\"青鸾\"]\n\n[llm]\nbase_url = \"https://ai.example/v1\"\napi_key = \"sk\"\nmodel = \"m\"\n\n[push]\nwebhook = \"\"\nsecret = \"\"\n").unwrap();
        let c = load(&p);
        assert_eq!(
            (c.llm.model.as_str(), c.hotwords[0].as_str()),
            ("m", "青鸾")
        );
        // 早期模板把 hotwords 写在 [llm] 下
        std::fs::write(&p, "[llm]\nmodel = \"m\"\nhotwords = [\"旧版\"]\n").unwrap();
        assert_eq!(load(&p).hotwords, vec!["旧版".to_string()]);
    }

    #[test]
    fn temperature_is_optional_and_bad_values_are_ignored() {
        let p = tmp("temp");
        for (line, want) in [("", None), ("temperature = 1\n", Some(1.0)), ("temperature = 0.7\n", Some(0.7)), ("temperature = 9\n", None), ("temperature = \"1\"\n", None)] {
            std::fs::write(&p, format!("[llm]\nmodel = \"kimi\"\n{line}")).unwrap();
            let c = load(&p);
            // 温度写错了也不能连带别的配置一起读不出来
            assert_eq!((c.llm.model.as_str(), c.llm.temperature), ("kimi", want), "{line}");
        }
        let mut c = load(&p);
        c.llm.temperature = None;
        save(&p, &c).unwrap();
        assert!(!std::fs::read_to_string(&p).unwrap().contains("temperature"));
    }

    #[test]
    fn missing_file_gives_defaults() {
        let c = load(Path::new("/nonexistent/tingo.toml"));
        assert_eq!(c.llm.base_url, "https://api.deepseek.com");
        assert_eq!((c.ui_language.as_str(), c.minutes_language.as_str(), c.asr_language.as_str()), ("auto", "auto", "zh"));
    }

    #[test]
    fn language_sections_roundtrip() {
        let p = tmp("lang");
        let cfg = Config { ui_language: "en".into(), minutes_language: "zh".into(), asr_language: "auto".into(), ..Default::default() };
        save(&p, &cfg).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        // 写在 [obsidian] 之后，顺序 [ui]、[minutes]、[asr]
        let pos = |k: &str| text.find(k).unwrap();
        assert!(pos("[obsidian]") < pos("[ui]") && pos("[ui]") < pos("[minutes]") && pos("[minutes]") < pos("[asr]"));
        assert!(text.contains("transcript = true\n\n[ui]\nlanguage = \"en\"\n\n[minutes]\nlanguage = \"zh\"\n\n[asr]\nlanguage = \"auto\"\n\n[record]\nauto_stop_silence_min = 10\nauto_stop_app_quit = true\n"));
        let c = load(&p);
        assert_eq!((c.ui_language.as_str(), c.minutes_language.as_str(), c.asr_language.as_str()), ("en", "zh", "auto"));
        // 老配置没有这三节、或者填了不认识的值：用默认
        std::fs::write(&p, "[llm]\nmodel = \"m\"\n\n[asr]\nlanguage = \"fr\"\n\n[minutes]\nlanguage = \"EN\"\n").unwrap();
        let c = load(&p);
        assert_eq!((c.ui_language.as_str(), c.minutes_language.as_str(), c.asr_language.as_str()), ("auto", "en", "zh"));
    }

    #[test]
    fn record_section_roundtrip() {
        let p = tmp("record");
        let cfg = Config { auto_stop_silence_min: 0, auto_stop_app_quit: false, ..Default::default() };
        save(&p, &cfg).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        // 接在 [asr] 之后，格式固定
        assert!(text.contains("[asr]\nlanguage = \"zh\"\n\n[record]\nauto_stop_silence_min = 0\nauto_stop_app_quit = false\n"));
        let c = load(&p);
        assert_eq!((c.auto_stop_silence_min, c.auto_stop_app_quit), (0, false));
        save(&p, &Config { auto_stop_silence_min: 240, ..Default::default() }).unwrap();
        let c = load(&p);
        assert_eq!((c.auto_stop_silence_min, c.auto_stop_app_quit), (240, true));
        // 没有这一节、或者值不合法：用默认
        assert_eq!(load(Path::new("/nonexistent/tingo.toml")).auto_stop_silence_min, 10);
        std::fs::write(&p, "[record]\nauto_stop_silence_min = 999\n").unwrap();
        let c = load(&p);
        assert_eq!((c.auto_stop_silence_min, c.auto_stop_app_quit), (10, true));
    }
}
