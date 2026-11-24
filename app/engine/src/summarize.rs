//! 用任一 OpenAI 兼容接口把转写整理成会议纪要（中文或英文）。Python 版 summarize.py 的移植，
//! 另加一条：热词作为术语表交给模型纠正同音字（这个识别模型不支持热词）。
//! 英文模板与中文一一对应，章节标题一字不差（界面、推送、Obsidian 都靠它们识别）。

use chrono::{Datelike, Duration as Days, NaiveDate};
use regex::{Captures, Regex};
use std::io::{BufRead, BufReader};
use std::sync::LazyLock;
use std::time::Duration;

use crate::config::LlmConfig;
use crate::i18n::{self, tr, Lang};
use crate::transcript::{fmt_ts, Transcript};

const SYSTEM: &str =
    "你是一位严谨的会议秘书，擅长把口语化、带识别错误的会议转写整理成中国职场常用的会议纪要。";

const MINUTES_PROMPT: &str = "下面是一场会议的语音转写，格式为「[时间戳] 说话人：内容」。会议日期：{meeting_date}。

请整理成会议纪要，严格使用以下 Markdown 结构，不要输出其他内容：

# <根据内容拟一个会议标题>

## 一句话总结
<一句话>

## 议题与讨论
### 1. <议题>
- <要点>（[mm:ss]）

## 决议
- <已经拍板的事项>（[mm:ss]）

## 待办
| 事项 | 负责人 | 截止时间 | 出处 |
|---|---|---|---|
| <具体动作> | <人名或说话人> | <时间> | [mm:ss] |

## 待定问题与风险
- <没有结论的问题、提到的风险>

## 原因分析
```fishbone
{\"problem\": \"<被分析的问题，用问句>\", \"branches\": [{\"title\": \"<一个原因，10 字以内>\", \"detail\": \"<一句话补充>\", \"refs\": [\"mm:ss\", \"mm:ss\"]}]}
```

```terms
{\"terms\": [{\"term\": \"历史工单\", \"variants\": [\"历史中单\"]}]}
```

要求：
1. 只依据转写内容，不编造。负责人或截止时间没有明确说出的，写\"未明确\"。
2. 转写有同音错字，结合上下文改正明显的错误；拿不准的保留原文。
3. 转写里的\"下周五\"\"明天\"等已由程序标好具体日期，如\"下周五（11月13日）\"，照抄括号里的日期，不要自己推算。其他相对时间（如\"十五号\"）保留原话，并查上面的日期对照表在括号里写出具体日期。
4. 每条要点、决议、待办都附上出处时间戳，便于回听核对。
5. 某一节没有内容时写\"无\"，不要删掉这一节（「原因分析」除外，见第 7 条）。
6. 去掉寒暄和口头禅，语言简洁、书面化。
7. 只有会议里明确分析了某个问题的原因时，才写「原因分析」这一节：每个问题一个 fishbone 代码块（最多两个），每块 2～6 个分支，内容是合法 JSON。refs 填支撑这个原因的原话的时间戳，必须是上面转写里出现过的 [mm:ss]（不带方括号）；找不到原话支撑就留空数组，不要编。没有分析原因的会议，整节省略，不要写\"无\"。
8. 全文最后（鱼骨图之后）必须原样输出 terms 代码块：列出会上出现的人名、产品、项目、专有名词的正确写法（优先用术语表、热词里的写法），variants 写转写里出现过的听错的写法（没有就写空数组）；最多 30 个；一个都没有时写 {\"terms\": []}。纪要正文里一律用正确写法。{glossary}{translate}

转写：
{transcript}
";

const CHUNK_PROMPT: &str = "下面是一场长会议中的一段转写（第 {index}/{total} 段），格式为「[时间戳] 说话人：内容」。会议日期：{meeting_date}。

请提炼这一段的：讨论要点、已拍板的决议、待办（事项/负责人/截止时间）、待定问题与风险。
每条都附出处时间戳 [mm:ss]，只依据原文，不编造，用简洁的条目输出。{glossary}{translate}

转写：
{transcript}
";

const REDUCE_PROMPT: &str = "下面是一场长会议按时间顺序分段提炼的笔记。请把它们合并成一份完整的会议纪要，去重、按议题归并，保留出处时间戳。

{instructions}

分段笔记：
{notes}
";

const SYSTEM_EN: &str = "You are a meticulous meeting secretary who turns colloquial meeting transcripts, \
speech-recognition errors and all, into clear, professional meeting minutes.";

const MINUTES_PROMPT_EN: &str = "Below is the speech-recognition transcript of a meeting, one line per turn in the form \"[timestamp] Speaker: content\". Meeting date: {meeting_date}

Write the meeting minutes using exactly the following Markdown structure, and output nothing else:

# <a short title>

## Summary
<one sentence>

## Discussion
### 1. <topic>
- <key point> ([mm:ss])

## Decisions
- <what was decided> ([mm:ss])

## Action Items
| Item | Owner | Due | Source |
|---|---|---|---|
| <concrete action> | <name or speaker> | <date> | [mm:ss] |

## Open Questions & Risks
- <unresolved questions, risks raised>

## Root Cause Analysis
```fishbone
{\"problem\": \"<the problem being analyzed, as a question>\", \"branches\": [{\"title\": \"<one cause, 5 words max>\", \"detail\": \"<one-sentence elaboration>\", \"refs\": [\"mm:ss\", \"mm:ss\"]}]}
```

```terms
{\"terms\": [{\"term\": \"Kafka\", \"variants\": [\"Kafca\"]}]}
```

Rules:
1. Use only what is in the transcript; do not make anything up. If an owner or a due date was not stated explicitly, write \"Not specified\".
2. The transcript has recognition errors (homophones, misheard words). Fix obvious ones from context; keep the original wording when unsure.
3. Chinese relative dates such as \"下周五\" or \"明天\" have already been annotated by the program with the actual date, e.g. \"下周五（11月13日）\"; use the date in the brackets instead of working it out yourself. For any other relative date (e.g. \"next Wednesday\", \"the 15th\"), keep the original wording and add the actual date in parentheses, looked up in the calendar above, e.g. \"next Wednesday (Nov 4)\".
4. End every key point, decision and action item with its source timestamp, so it can be checked against the recording.
5. If a section has nothing, write \"None\" rather than dropping the section (except \"Root Cause Analysis\", see rule 7).
6. Leave out small talk and filler words; keep the language concise and formal.
7. Only include the \"Root Cause Analysis\" section when the meeting explicitly analyzed the causes of a problem: one fishbone code block per problem (at most two), 2-6 branches each, valid JSON. In refs, put the timestamps of the lines that support the cause; they must be [mm:ss] timestamps that appear in the transcript below (without the brackets). If no line supports it, leave refs as an empty array; do not invent one. If the meeting did not analyze causes, omit the whole section; do not write \"None\".
8. At the very end (after the fishbone), always output the terms code block exactly as shown above: list the correct spelling of the names of people, products, projects and other proper nouns that came up in the meeting (prefer the spelling in the glossary / hotwords), and in variants put the misheard spellings that appear in the transcript (an empty array if none); at most 30 entries; if there are none, write {\"terms\": []}. Always use the correct spelling in the body of the minutes.{glossary}{translate}

Transcript:
{transcript}
";

const CHUNK_PROMPT_EN: &str = "Below is one part (part {index} of {total}) of the transcript of a long meeting, one line per turn in the form \"[timestamp] Speaker: content\". Meeting date: {meeting_date}

Extract from this part: key discussion points, decisions made, action items (item / owner / due date), and open questions & risks.
End every entry with its source timestamp [mm:ss]. Use only what is in the transcript, do not make anything up, and output concise bullet points.{glossary}{translate}

Transcript:
{transcript}
";

const REDUCE_PROMPT_EN: &str = "Below are notes taken part by part, in chronological order, from a long meeting. Merge them into one complete set of meeting minutes: remove duplicates, group points by topic, and keep the source timestamps.

{instructions}

Notes by part:
{notes}
";

/// 一种语言的整套提示词
struct Prompts {
    system: &'static str,
    minutes: &'static str,
    chunk: &'static str,
    reduce: &'static str,
    /// 纪要提示词里转写正文从这里开始；合并时只取它前面的格式要求
    transcript_marker: &'static str,
}

fn prompts(lang: Lang) -> Prompts {
    match lang {
        Lang::Zh => Prompts {
            system: SYSTEM,
            minutes: MINUTES_PROMPT,
            chunk: CHUNK_PROMPT,
            reduce: REDUCE_PROMPT,
            transcript_marker: "转写：",
        },
        Lang::En => Prompts {
            system: SYSTEM_EN,
            minutes: MINUTES_PROMPT_EN,
            chunk: CHUNK_PROMPT_EN,
            reduce: REDUCE_PROMPT_EN,
            transcript_marker: "\nTranscript:\n",
        },
    }
}

/// 转写和纪要不是同一种语言时（手动选了纪要语言），明确要求翻译
fn translate_note(minutes: Lang, transcript: Lang) -> &'static str {
    match (minutes, transcript) {
        (Lang::En, Lang::Zh) => "\n\nThe transcript is in Chinese: write the minutes in English, translating as needed (keep names of people, products and systems as they are).",
        (Lang::Zh, Lang::En) => "\n\n注意：转写是英文的，纪要一律用中文撰写，需要时翻译（人名、产品名、系统名等专有名词保留原文）。",
        _ => "",
    }
}

const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
const LOCAL_CHUNK_CHARS: usize = 6000;
const CHARS_PER_TOKEN: f64 = 1.3;
const RESERVED_TOKENS: usize = 3500;

fn md(d: NaiveDate) -> String {
    format!("{}月{}日", d.month(), d.day())
}

/// 英文的月日：Nov 4
fn md_en(d: NaiveDate) -> String {
    d.format("%b %-d").to_string()
}

/// 纪要里的会议日期：2026-10-28（星期三） / 2026-10-28 (Wednesday)
fn day_line(d: NaiveDate, lang: Lang) -> String {
    match lang {
        Lang::Zh => format!("{}（星期{}）", d.format("%Y-%m-%d"), weekday(d)),
        Lang::En => d.format("%Y-%m-%d (%A)").to_string(),
    }
}

fn weekday(d: NaiveDate) -> &'static str {
    WEEKDAYS[d.weekday().num_days_from_monday() as usize]
}

/// 日期对照表。小模型算"下周五是几号"经常算错，直接给表让它查。
pub fn calendar_hint(d: NaiveDate) -> String {
    calendar_hint_in(d, Lang::Zh)
}

pub fn calendar_hint_in(d: NaiveDate, lang: Lang) -> String {
    if lang.is_en() {
        return calendar_hint_en(d);
    }
    let monday = d - Days::days(d.weekday().num_days_from_monday() as i64);
    let mut lines = vec![
        "日期对照表（每周从周一开始）：".to_string(),
        format!(
            "- 今天 {}（星期{}），明天 {}，后天 {}",
            md(d),
            weekday(d),
            md(d + Days::days(1)),
            md(d + Days::days(2))
        ),
    ];
    for (name, offset) in [("本周", 0), ("下周", 7), ("下下周", 14)] {
        let days: Vec<String> = (0..7)
            .map(|i| {
                format!(
                    "周{} {}",
                    WEEKDAYS[i as usize],
                    md(monday + Days::days(offset + i))
                )
            })
            .collect();
        lines.push(format!("- {name}：{}", days.join("、")));
    }
    lines.join("\n")
}

fn calendar_hint_en(d: NaiveDate) -> String {
    let monday = d - Days::days(d.weekday().num_days_from_monday() as i64);
    let mut lines = vec![
        "Calendar (weeks start on Monday):".to_string(),
        format!(
            "- Today is {} ({}); tomorrow is {}; the day after tomorrow is {}",
            md_en(d),
            d.format("%A"),
            md_en(d + Days::days(1)),
            md_en(d + Days::days(2))
        ),
    ];
    for (name, offset) in [("This week", 0), ("Next week", 7), ("The week after next", 14)] {
        let days: Vec<String> = (0..7)
            .map(|i| {
                let x = monday + Days::days(offset + i);
                format!("{} {}", x.format("%a"), md_en(x))
            })
            .collect();
        lines.push(format!("- {name}: {}", days.join(", ")));
    }
    lines.join("\n")
}

// Rust 的 regex 不支持后行断言：把紧跟的括号一起匹配进来，有括号说明已经标过，原样返回
static WEEK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(下下|下|本|这)个?(?:周|星期|礼拜)的?(?:周|星期|礼拜)?([一二三四五六日天])([（(])?",
    )
    .unwrap()
});
static DAY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(大后天|后天|明天)([（(])?").unwrap());

/// 在"下周五""下下周的周一""明天"后面标上具体日期，换算由程序完成，不靠模型。
pub fn annotate_dates(text: &str, d: NaiveDate) -> String {
    let monday = d - Days::days(d.weekday().num_days_from_monday() as i64);
    let text = WEEK_RE.replace_all(text, |c: &Captures| {
        if c.get(3).is_some() {
            return c[0].to_string();
        }
        let week = match &c[1] {
            "下下" => 14,
            "下" => 7,
            _ => 0,
        };
        let day = "一二三四五六日天"
            .chars()
            .position(|x| c[2].starts_with(x))
            .unwrap()
            .min(6) as i64;
        format!("{}（{}）", &c[0], md(monday + Days::days(week + day)))
    });
    DAY_RE
        .replace_all(&text, |c: &Captures| {
            if c.get(2).is_some() {
                return c[0].to_string();
            }
            let n = match &c[1] {
                "明天" => 1,
                "后天" => 2,
                _ => 3,
            };
            format!("{}（{}）", &c[0], md(d + Days::days(n)))
        })
        .into_owned()
}

#[cfg(test)]
fn glossary(hotwords: &[String]) -> String {
    glossary_in(hotwords, Lang::Zh)
}

fn glossary_in(hotwords: &[String], lang: Lang) -> String {
    let words: Vec<&str> = hotwords
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return String::new();
    }
    if lang.is_en() {
        return format!(
            "\n9. Proper nouns in this meeting (people, projects, systems): {}. Wherever the transcript has a homophone or near-homophone of one of them, replace it with the exact term.",
            words.join(", ")
        );
    }
    format!(
        "\n9. 本场会议涉及的专有名词（人名、项目、系统名）：{}。转写里与它们同音或近音的写法，一律改成这些词。",
        words.join("、")
    )
}

/// 是不是本机 Ollama 上的这个模型：以 /api/show 里真实的模型文件格式为准
/// （本机的 OpenAI 兼容中转可能连 /api/show 都仿了，但给不出 gguf）。
pub fn is_ollama(cfg: &LlmConfig) -> bool {
    cfg.api != "anthropic" && ollama_show(cfg).is_some()
}

fn ollama_show(cfg: &LlmConfig) -> Option<serde_json::Value> {
    let host = reqwest::Url::parse(&cfg.base_url)
        .ok()?
        .host_str()?
        .to_string();
    if !["localhost", "127.0.0.1", "::1", "[::1]"].contains(&host.as_str()) {
        return None;
    }
    let root = cfg
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let r = client
        .post(format!("{root}/api/show"))
        .json(&serde_json::json!({ "model": cfg.model }))
        .send()
        .ok()?;
    let data: serde_json::Value = r.json().ok()?;
    let fmt = data["details"]["format"].as_str()?;
    ["gguf", "safetensors"].contains(&fmt).then_some(data)
}

/// 一次交给模型的转写最多多少字，超过就分段提炼再合并。
pub fn default_max_chars(cfg: &LlmConfig) -> usize {
    let Some(show) = ollama_show(cfg) else {
        return 40000;
    };
    static NUM_CTX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"num_ctx\s+(\d+)").unwrap());
    let ctx = show["parameters"]
        .as_str()
        .and_then(|p| NUM_CTX.captures(p))
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(4096);
    // 上下文够大也不超过 LOCAL_CHUNK_CHARS：实测 8B 模型一次读 1.3 万字，纪要里的待办从 14 条掉到 6 条
    let fits = (ctx.saturating_sub(RESERVED_TOKENS) as f64 * CHARS_PER_TOKEN) as usize;
    fits.clamp(4000, LOCAL_CHUNK_CHARS)
}

/// 去掉推理模型输出里的 <think>…</think>。partial 用于流式：未闭合的 <think> 和末尾半截标签先不显示。
pub fn strip_think(text: &str, partial: bool) -> String {
    static THINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>\s*").unwrap());
    let mut t = THINK.replace_all(text, "").into_owned();
    if partial {
        if let Some(i) = t.find("<think>") {
            t.truncate(i);
        }
        for n in (1.."<think>".len()).rev() {
            if t.ends_with(&"<think>"[..n]) {
                t.truncate(t.len() - n);
                break;
            }
        }
    }
    t
}

// 不管服务商默认的输出上限是多少，纪要都不能无限长下去。
// 正文（含夹在正文里的 <think>）和单独字段里的思考分开算：推理模型读长会议时光思考就能写五六万字
// （实测 MiniMax-M3.1-Flash 读 1.1 万字转写想了 5.9 万字，两分钟写完），这是正常的，不能当失控掐掉。
// 真失控（原地打转）靠 repeated_tail 发现，这两个上限只兜底。
const MAX_STREAM_CHARS: usize = 64_000;
const MAX_THINK_CHARS: usize = 240_000;
const MAX_MINUTES_CHARS: usize = 32_000;

fn check_stream_size(raw_chars: usize, thought_chars: usize) -> Result<(), String> {
    if thought_chars > MAX_THINK_CHARS {
        return Err(tr("大模型思考了二十多万字还没开始写，已停止生成。可以重试，或换用不深度思考的模型。", "The model thought for over 200,000 characters without starting the minutes; generation was stopped. Retry, or choose a model without deep thinking.").into());
    }
    if raw_chars > MAX_STREAM_CHARS {
        return Err(tr("模型输出异常过长，已停止生成。请换用非深度思考模型后重试。", "Model output exceeded the safety limit. Retry with a non-reasoning model.").into());
    }
    Ok(())
}

/// 思考内容只留最后几千字，够看出是不是在原地打转就行
fn push_think_tail(tail: &mut String, text: &str) {
    tail.push_str(text);
    let n = tail.chars().count();
    if n > 4096 {
        let cut = tail.char_indices().nth(n - 4096).map(|(i, _)| i).unwrap_or(0);
        tail.drain(..cut);
    }
}

fn repeated_tail(text: &str) -> bool {
    let tail: Vec<char> = text.chars().rev().take(2048).collect();
    (128..=512).any(|n| {
        tail.len() >= n * 4 && tail[..n].iter().filter(|c| !c.is_whitespace()).count() >= 64
            && (1..4).all(|i| tail[..n] == tail[i * n..(i + 1) * n])
    })
}

fn finish_reason(reason: &str) -> Result<(), String> {
    match reason {
        "stop" | "end_turn" | "stop_sequence" => Ok(()),
        "length" | "max_tokens" => Err(tr("模型达到输出上限，纪要尚未完成。", "The model reached its output limit before finishing the minutes.").into()),
        _ => Err(tr(&format!("模型提前结束输出（{reason}），纪要尚未完成。"), &format!("The model stopped early ({reason}); the minutes are incomplete.")).into()),
    }
}

/// 一次请求失败的原因。`dropped`：连接中途断了（App 在后台被挂起、网络切换、服务端掐断长连接），
/// 重新请求多半能成；别的错误（没配 Key、模型报错、输出超长）重试也没用。
#[derive(Debug)]
struct Failure {
    message: String,
    dropped: bool,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure { message, dropped: false }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

fn dropped(message: String) -> Failure {
    Failure { message, dropped: true }
}

fn stream_read_error(error: std::io::Error) -> Failure {
    use std::error::Error;
    let mut details = vec![error.to_string()];
    let mut cause = error.source();
    for _ in 0..4 {
        let Some(current) = cause else { break };
        let message = current.to_string();
        if details.last() != Some(&message) { details.push(message); }
        cause = current.source();
    }
    let detail: String = details.join(" → ").chars().take(400).collect();
    dropped(tr(&format!("读取大模型输出中断：{detail}"), &format!("Lost the connection while reading the model's output: {detail}")).into())
}

fn completed_text(raw: &str, complete: bool) -> Result<String, Failure> {
    if !complete {
        return Err(dropped(tr("模型连接提前结束，未收到完成标记。", "The model connection ended without a completion marker.").into()));
    }
    let text = strip_think(raw, false).trim().to_string();
    if text.is_empty() {
        return Err(tr("模型没有返回纪要正文，请重试或更换模型。", "The model returned no minutes. Retry or choose another model.").into());
    }
    Ok(text)
}

/// 纪要默认用低温度，少发挥。有的模型只接受默认值（Kimi、OpenAI 的推理模型只认 1），设置里可以改。
pub const DEFAULT_TEMPERATURE: f64 = 0.2;

fn temperature(cfg: &LlmConfig) -> f64 {
    cfg.temperature.unwrap_or(DEFAULT_TEMPERATURE)
}

fn http_error(status: reqwest::StatusCode, text: &str) -> String {
    let text: String = text.chars().take(300).collect();
    tr(&format!("大模型返回 {status}：{text}"), &format!("The language model returned {status}: {text}")).into()
}

/// 请模型别思考的参数：DeepSeek、Kimi、GLM、豆包认 thinking.type，通义千问认 enable_thinking。
/// 不认识的服务大多直接忽略；回 400 的由 send_llm 换成「少想」再换成不带。
fn thinking_off(body: &mut serde_json::Value) {
    body["thinking"] = serde_json::json!({"type": "disabled"});
    body["enable_thinking"] = false.into();
}

/// 发请求，返回成功的响应。模型因为某个参数回 400 时去掉它再发，不用用户先去设置里改：
/// - temperature：只接受默认温度的模型（Kimi 只认 1）
/// - 关思考的参数：关不掉思考的模型（MiniMax-M3.1 要求 adaptive thinking）改请它少想（reasoning_effort=low），
///   这个也不认就什么都不带，用模型自己的默认
fn send_llm(
    cfg: &LlmConfig,
    mut body: serde_json::Value,
    send: impl Fn(&serde_json::Value) -> reqwest::Result<reqwest::blocking::Response>,
) -> Result<reqwest::blocking::Response, String> {
    let unreachable = |e: reqwest::Error| -> String {
        let url = &cfg.base_url;
        tr(&format!("连不上大模型（{url}）：{e}"), &format!("Couldn't reach the language model ({url}): {e}")).into()
    };
    let mut resp = send(&body).map_err(unreachable)?;
    // 每次去掉一个参数，最多试 temperature、关思考、少想三次
    for _ in 0..3 {
        if resp.status() != reqwest::StatusCode::BAD_REQUEST {
            break;
        }
        let text = resp.text().unwrap_or_default();
        let lower = text.to_ascii_lowercase();
        let b = body.as_object_mut().expect("请求体是对象");
        let mentions = |words: &[&str]| words.iter().any(|w| lower.contains(w));
        if mentions(&["temperature"]) && b.remove("temperature").is_some() {
        } else if mentions(&["thinking", "reasoning"]) && (b.remove("thinking").is_some() | b.remove("enable_thinking").is_some()) {
            b.insert("reasoning_effort".into(), "low".into());
        } else if mentions(&["reasoning", "effort"]) && b.remove("reasoning_effort").is_some() {
        } else {
            return Err(http_error(reqwest::StatusCode::BAD_REQUEST, &text));
        }
        resp = send(&body).map_err(unreachable)?;
    }
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(http_error(status, &resp.text().unwrap_or_default()));
    }
    Ok(resp)
}

/// 流式调用 chat/completions，返回去掉思考过程的完整回答。
/// `on_think(已思考字数)`：推理模型先长时间思考，不报出来界面就像卡住了。
/// 思考内容有的放在 reasoning_content / reasoning 字段，有的夹在正文的 <think> 里，两种都算。
pub fn chat(
    cfg: &LlmConfig,
    system: &str,
    prompt: &str,
    think: bool,
    on_token: impl FnMut(&str),
    on_think: impl FnMut(usize),
) -> Result<String, String> {
    chat_once(cfg, system, prompt, think, on_token, on_think).map_err(|f| f.message)
}

/// 连接中途断了时最多再请求几次
const RETRIES: u32 = 2;
const RETRY_PAUSE: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(3) };

/// 生成纪要用的 chat：连接中途断了就从头再要一遍。
/// 手机上最常见的是生成时锁屏或切走，iOS 挂起 App 后收回网络连接，回到前台读到 Connection reset；
/// 网络切换、中转服务掐断长连接也一样。重试前调 `on_retry`，让界面清掉已经显示的半截正文。
#[allow(clippy::too_many_arguments)]
fn chat_retry(
    cfg: &LlmConfig,
    system: &str,
    prompt: &str,
    think: bool,
    stage: &str,
    on_token: &mut dyn FnMut(&str),
    on_think: &mut dyn FnMut(usize),
    on_retry: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let mut attempt = 0;
    loop {
        match chat_once(cfg, system, prompt, think, &mut *on_token, &mut *on_think) {
            Ok(text) => return Ok(text),
            Err(f) if f.dropped && attempt < RETRIES => {
                attempt += 1;
                // 接在原阶段文字后面：界面靠「分段提炼 2/5」「合并成纪要」算进度
                let note = tr(&format!("连接断了，正在重试（{attempt}/{RETRIES}）"), &format!("Connection dropped, retrying ({attempt}/{RETRIES})")).to_string();
                on_retry(&if stage.is_empty() { note } else { format!("{stage} · {note}") });
                std::thread::sleep(RETRY_PAUSE * attempt);
            }
            Err(f) if f.dropped => {
                let hint = tr(
                    "（重试后仍然断开。手机上生成纪要时请留在 App 里、别锁屏；网络切换也会断开。可以稍后重新生成。）",
                    " (Still dropped after retrying. On iPhone, stay in the app and keep the screen on while minutes are generated; switching networks also drops the connection. You can generate again later.)",
                );
                return Err(format!("{}{hint}", f.message));
            }
            Err(f) => return Err(f.message),
        }
    }
}

fn chat_once(
    cfg: &LlmConfig,
    system: &str,
    prompt: &str,
    think: bool,
    mut on_token: impl FnMut(&str),
    mut on_think: impl FnMut(usize),
) -> Result<String, Failure> {
    if cfg.api == "anthropic" {
        return chat_anthropic(cfg, system, prompt, on_token, on_think);
    }
    let local = is_ollama(cfg);
    let key = if cfg.api_key.is_empty() && local {
        "ollama".to_string()
    } else {
        cfg.api_key.clone()
    };
    if key.is_empty() {
        return Err(tr(
            "没有配置大模型的 API Key：在设置里填写",
            "No API key configured for the language model: add one in Settings",
        )
        .into());
    }
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
        "temperature": temperature(cfg),
        "stream": true,
    });
    if !(think && cfg.think) {
        if local {
            // Ollama 的兼容接口用 reasoning_effort=none 关掉 Qwen3 等模型的思考
            body["reasoning_effort"] = "none".into();
        } else {
            thinking_off(&mut body);
        }
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let resp = send_llm(cfg, body, |body| client.post(&url).bearer_auth(&key).json(body).send())?;
    let mut raw = String::new();
    let mut shown = 0;
    let mut thought = 0usize;
    let mut think_tail = String::new();
    let mut next_think_check = 512usize;
    let mut raw_chars = 0usize;
    let mut next_repeat_check = 512usize;
    let mut complete = false;
    for line in BufReader::new(resp).lines() {
        let line = line.map_err(stream_read_error)?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            complete = true;
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(error) = v.get("error") {
            let message: String = error["message"].as_str().unwrap_or("stream error").chars().take(300).collect();
            return Err(tr(&format!("大模型返回错误：{message}"), &format!("The language model returned an error: {message}")).into());
        }
        let reason = v["choices"][0]["finish_reason"].as_str();
        let d = &v["choices"][0]["delta"];
        if let Some(r) = d["reasoning_content"].as_str().or_else(|| d["reasoning"].as_str()) {
            if !r.is_empty() {
                thought += r.chars().count();
                check_stream_size(raw_chars, thought)?;
                push_think_tail(&mut think_tail, r);
                if thought >= next_think_check {
                    next_think_check = thought + 512;
                    if repeated_tail(&think_tail) {
                        return Err(tr("大模型思考时一直在重复同一段，已停止生成。可以重试，或换用不深度思考的模型。", "The model kept repeating itself while thinking; generation was stopped. Retry, or choose a model without deep thinking.").into());
                    }
                }
                on_think(thought);
            }
        }
        if let Some(delta) = d["content"].as_str().filter(|t| !t.is_empty()) {
            raw_chars += delta.chars().count();
            check_stream_size(raw_chars, thought)?;
            raw.push_str(delta);
            let visible = strip_think(&raw, true);
            if visible.chars().count() > MAX_MINUTES_CHARS {
                return Err(tr("纪要正文异常过长，已停止生成。", "The minutes grew beyond the output limit; generation was stopped.").into());
            }
            if visible.len() > shown {
                on_token(&visible[shown..]);
                shown = visible.len();
            } else if shown == 0 && raw.trim_start().starts_with("<think>") {
                on_think(thought + raw_chars);
            }
            if raw_chars >= next_repeat_check {
                next_repeat_check = raw_chars + 512;
                if repeated_tail(&raw) {
                    return Err(tr("模型持续重复同一段内容，已停止生成。", "The model kept repeating the same passage; generation was stopped.").into());
                }
            }
        }
        if let Some(reason) = reason {
            finish_reason(reason)?;
            complete = true;
            break;
        }
    }
    completed_text(&raw, complete)
}

/// Anthropic 接口的根地址：用户填 https://api.anthropic.com 或 …/v1 都行，统一成以 /v1 结尾
pub fn anthropic_v1(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        b.to_string()
    } else {
        format!("{b}/v1")
    }
}

/// 一行 Anthropic 流式事件（SSE 的 data 部分）
#[derive(Debug, PartialEq)]
pub enum AnthropicEvent {
    Text(String),
    Thinking(usize),
    Error(String),
    Stop,
    Finish(String),
    Other,
}

pub fn parse_anthropic_event(data: &str) -> AnthropicEvent {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return AnthropicEvent::Other };
    match v["type"].as_str() {
        Some("content_block_delta") => match v["delta"]["type"].as_str() {
            Some("text_delta") => AnthropicEvent::Text(v["delta"]["text"].as_str().unwrap_or("").to_string()),
            Some("thinking_delta") => AnthropicEvent::Thinking(v["delta"]["thinking"].as_str().unwrap_or("").chars().count()),
            _ => AnthropicEvent::Other,
        },
        Some("error") => AnthropicEvent::Error(v["error"]["message"].as_str().unwrap_or("unknown error").to_string()),
        Some("message_delta") => v["delta"]["stop_reason"].as_str().map(|r| AnthropicEvent::Finish(r.into())).unwrap_or(AnthropicEvent::Other),
        Some("message_stop") => AnthropicEvent::Stop,
        _ => AnthropicEvent::Other,
    }
}

/// Claude 等 Anthropic Messages 接口：POST /v1/messages，x-api-key + anthropic-version 头，流式事件见 parse_anthropic_event
fn chat_anthropic(
    cfg: &LlmConfig,
    system: &str,
    prompt: &str,
    mut on_token: impl FnMut(&str),
    mut on_think: impl FnMut(usize),
) -> Result<String, Failure> {
    if cfg.api_key.is_empty() {
        return Err(tr(
            "没有配置大模型的 API Key：在设置里填写",
            "No API key configured for the language model: add one in Settings",
        )
        .into());
    }
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 16000,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": temperature(cfg),
        "stream": true,
    });
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/messages", anthropic_v1(&cfg.base_url));
    let resp = send_llm(cfg, body, |body| {
        client.post(&url).header("x-api-key", &cfg.api_key).header("anthropic-version", "2023-06-01").json(body).send()
    })?;
    let mut raw = String::new();
    let mut shown = 0;
    let mut thought = 0usize;
    let mut raw_chars = 0usize;
    let mut next_repeat_check = 512usize;
    let mut complete = false;
    for line in BufReader::new(resp).lines() {
        let line = line.map_err(stream_read_error)?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        match parse_anthropic_event(data) {
            AnthropicEvent::Text(t) if !t.is_empty() => {
                raw_chars += t.chars().count();
                check_stream_size(raw_chars, thought)?;
                raw.push_str(&t);
                let visible = strip_think(&raw, true);
                if visible.chars().count() > MAX_MINUTES_CHARS {
                    return Err(tr("纪要正文异常过长，已停止生成。", "The minutes grew beyond the output limit; generation was stopped.").into());
                }
                if raw_chars >= next_repeat_check {
                    next_repeat_check = raw_chars + 512;
                    if repeated_tail(&raw) {
                        return Err(tr("模型持续重复同一段内容，已停止生成。", "The model kept repeating the same passage; generation was stopped.").into());
                    }
                }
                if visible.len() > shown {
                    on_token(&visible[shown..]);
                    shown = visible.len();
                }
            }
            AnthropicEvent::Thinking(n) => {
                thought += n;
                check_stream_size(raw_chars, thought)?;
                on_think(thought);
            }
            AnthropicEvent::Error(m) => {
                return Err(tr(&format!("大模型返回错误：{m}"), &format!("The language model returned an error: {m}")).into())
            }
            AnthropicEvent::Finish(reason) => { finish_reason(&reason)?; complete = true; }
            AnthropicEvent::Stop => { complete = true; break; }
            _ => {}
        }
    }
    completed_text(&raw, complete)
}

pub fn split_chunks(lines: &[&str], max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in lines {
        let n = line.chars().count();
        if !buf.is_empty() && size + n > max_chars {
            chunks.push(buf.join("\n"));
            buf.clear();
            size = 0;
        }
        buf.push(line);
        size += n + 1;
    }
    if !buf.is_empty() {
        chunks.push(buf.join("\n"));
    }
    chunks
}

pub struct Hooks<'a> {
    pub on_token: &'a mut dyn FnMut(&str),
    pub on_stage: &'a mut dyn FnMut(&str),
    /// 推理模型思考了多少字（还没开始写正文时用来显示进展）
    pub on_think: &'a mut dyn FnMut(usize),
}

/// 纪要语言：配置为 zh / en 时照办；auto 时跟转写的语言（汉字占比 ≥ 0.3 算中文）
pub fn minutes_lang(setting: &str, tr: &Transcript) -> Lang {
    i18n::resolve(setting, &transcript_words(tr))
}

/// 只看说的话，不看说话人名（"说话人1"会把英文会议算成中文）
fn transcript_words(tr: &Transcript) -> String {
    tr.sentences.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// `lang_setting` 是配置里的纪要语言（auto | zh | en）。
pub fn summarize(
    tr: &Transcript,
    cfg: &LlmConfig,
    meeting_date: NaiveDate,
    hotwords: &[String],
    lang_setting: &str,
    hooks: Hooks,
) -> Result<String, String> {
    let spoken = i18n::detect(&transcript_words(tr));
    let lang = minutes_lang(lang_setting, tr);
    let tr = &tr.localized(lang); // 英文纪要里没改名的说话人叫 Speaker N
    let pr = prompts(lang);
    let day = day_line(meeting_date, lang);
    let dated = format!("{day}\n\n{}", calendar_hint_in(meeting_date, lang));
    let gl = glossary_in(hotwords, lang);
    let translate = translate_note(lang, spoken);
    let max_chars = default_max_chars(cfg);
    let text = annotate_dates(&tr.to_prompt_text_in(lang), meeting_date);
    let minutes_prompt = pr
        .minutes
        .replace("{meeting_date}", &dated)
        .replace("{glossary}", &gl)
        .replace("{translate}", translate);

    let Hooks { on_token, on_stage, on_think } = hooks;
    let body = if text.chars().count() <= max_chars {
        chat_retry(
            cfg,
            pr.system,
            &minutes_prompt.replace("{transcript}", &text),
            true,
            "",
            on_token,
            on_think,
            on_stage,
        )?
    } else {
        let lines: Vec<&str> = text.lines().collect();
        let chunks = split_chunks(&lines, max_chars);
        let mut notes = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let (k, n) = (i + 1, chunks.len());
            let stage = tr_stage(k, n);
            on_stage(&stage);
            let p = pr
                .chunk
                .replace("{index}", &k.to_string())
                .replace("{total}", &n.to_string())
                .replace("{meeting_date}", &dated)
                .replace("{glossary}", &gl)
                .replace("{translate}", translate)
                .replace("{transcript}", chunk);
            // 分段提炼主要是摘条目，不需要思考；最后合并时再思考
            let head = match lang {
                Lang::Zh => format!("### 第 {k} 段"),
                Lang::En => format!("### Part {k}"),
            };
            notes.push(format!("{head}\n{}", chat_retry(cfg, pr.system, &p, false, &stage, &mut |_| {}, &mut |_| {}, on_stage)?));
        }
        let merging = i18n::tr("合并成纪要", "Merging into minutes");
        on_stage(merging);
        let instructions = minutes_prompt
            .split(pr.transcript_marker)
            .next()
            .unwrap_or("")
            .to_string();
        let p = pr
            .reduce
            .replace("{instructions}", &instructions)
            .replace("{notes}", &notes.join("\n\n"));
        chat_retry(cfg, pr.system, &p, true, merging, on_token, on_think, on_stage)?
    };
    Ok(with_meta(&body, tr, &day, lang))
}

/// 长会议的进度文字（界面靠它认出分段进度）：分段提炼 2/5 / Summarizing part 2/5
fn tr_stage(i: usize, n: usize) -> String {
    tr(&format!("分段提炼 {i}/{n}"), &format!("Summarizing part {i}/{n}")).to_string()
}

/// 会议时间、时长、参会人由程序填，不让模型猜。
fn with_meta(body: &str, tr: &Transcript, day: &str, lang: Lang) -> String {
    let body = strip_fence(body);
    let names: Vec<String> = tr
        .speakers_in_order()
        .into_iter()
        .map(|s| tr.name_of(s))
        .collect();
    let names = if names.is_empty() {
        lang.pick("无", "None").to_string()
    } else {
        names.join(lang.pick("、", ", "))
    };
    let meta = match lang {
        Lang::Zh => format!("- 日期：{day}\n- 时长：{}\n- 参会人：{names}", fmt_ts(tr.duration_ms)),
        Lang::En => format!("- Date: {day}\n- Duration: {}\n- Participants: {names}", fmt_ts(tr.duration_ms)),
    };
    let (first, rest) = body.split_once('\n').unwrap_or((&body, ""));
    let (title, rest) = if first.starts_with("# ") {
        (first, rest)
    } else {
        (lang.pick("# 会议纪要", "# Meeting Minutes"), body.as_str())
    };
    format!("{title}\n\n{meta}\n\n{}\n", rest.trim())
}

fn strip_fence(text: &str) -> String {
    let mut t = text.trim().to_string();
    if t.starts_with("```") {
        t = t
            .split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default();
        if t.trim_end().ends_with("```") {
            t = t.trim_end().trim_end_matches("```").to_string();
        }
    }
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本地假接口：依次用 `replies`（状态码, 正文）回答，返回收到的请求体
    fn fixture(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for (status, reply) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" { break; }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") { length = v.trim().parse().unwrap(); }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                bodies.push(serde_json::from_slice(&body).unwrap());
                write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        (url, server)
    }

    fn send_to(url: &str, cfg: &LlmConfig) -> Result<String, String> {
        let client = reqwest::blocking::Client::new();
        let body = serde_json::json!({"model": "kimi", "temperature": temperature(cfg)});
        send_llm(cfg, body, |b| client.post(url).json(b).send()).map(|r| r.text().unwrap())
    }

    #[test]
    fn temperature_rejection_is_resent_without_it() {
        // issue #2：Kimi 只接受 temperature=1
        let cfg = LlmConfig::default();
        let (url, server) = fixture(vec![(400, r#"{"error":{"message":"invalid temperature: only 1 is allowed for this model"}}"#), (200, "ok")]);
        assert_eq!(send_to(&url, &cfg).unwrap(), "ok");
        let bodies = server.join().unwrap();
        assert_eq!(bodies[0]["temperature"], 0.2);
        assert!(bodies[1].get("temperature").is_none());

        // 设置里填了温度就用填的
        let cfg = LlmConfig { temperature: Some(1.0), ..LlmConfig::default() };
        let (url, server) = fixture(vec![(200, "ok")]);
        send_to(&url, &cfg).unwrap();
        assert_eq!(server.join().unwrap()[0]["temperature"], 1.0);

        // 别的 400 不重发，原样报出来
        let (url, server) = fixture(vec![(400, "model not found")]);
        let err = send_to(&url, &LlmConfig::default()).unwrap_err();
        assert!(err.contains("400") && err.contains("model not found"), "{err}");
        assert_eq!(server.join().unwrap().len(), 1);

        // 去掉温度还是 400：报第二次的错，不再重发
        let (url, server) = fixture(vec![(400, "bad temperature"), (400, "still bad")]);
        assert!(send_to(&url, &LlmConfig::default()).unwrap_err().contains("still bad"));
        assert_eq!(server.join().unwrap().len(), 2);
    }

    fn send_thinking_off(url: &str) -> Result<String, String> {
        let client = reqwest::blocking::Client::new();
        let mut body = serde_json::json!({"model": "m", "temperature": 0.2});
        thinking_off(&mut body);
        send_llm(&LlmConfig::default(), body, |b| client.post(url).json(b).send()).map(|r| r.text().unwrap())
    }

    #[test]
    fn thinking_off_falls_back_to_thinking_less_then_to_the_default() {
        // 认 thinking.type 的服务（DeepSeek、Kimi、GLM、豆包）一次就成
        let (url, server) = fixture(vec![(200, "ok")]);
        send_thinking_off(&url).unwrap();
        let first = &server.join().unwrap()[0];
        assert_eq!((first["thinking"]["type"].as_str(), first["enable_thinking"].as_bool()), (Some("disabled"), Some(false)));

        // MiniMax-M3.1 关不掉思考（实测的报错原文）：改请它少想
        let minimax = r#"{"type":"error","error":{"type":"bad_request_error","message":"invalid params, model \"MiniMax-M3.1-Flash\" requires adaptive thinking; thinking.type=\"disabled\" (including reasoning.effort=none) is not allowed (2013)"}}"#;
        let (url, server) = fixture(vec![(400, minimax), (200, "ok")]);
        assert_eq!(send_thinking_off(&url).unwrap(), "ok");
        let bodies = server.join().unwrap();
        assert!(bodies[1].get("thinking").is_none() && bodies[1].get("enable_thinking").is_none());
        assert_eq!((bodies[1]["reasoning_effort"].as_str(), bodies[1]["temperature"].as_f64()), (Some("low"), Some(0.2)));

        // 连 reasoning_effort 也不认：什么都不带，用模型的默认
        let (url, server) = fixture(vec![(400, "Unrecognized request argument supplied: thinking"), (400, "Unsupported parameter: 'reasoning_effort'"), (200, "ok")]);
        assert_eq!(send_thinking_off(&url).unwrap(), "ok");
        let bodies = server.join().unwrap();
        assert!(bodies[2].get("reasoning_effort").is_none() && bodies[2].get("thinking").is_none());

        // 温度和思考都不认（Kimi 推理模型）：一个一个去掉
        let (url, server) = fixture(vec![(400, "invalid temperature"), (400, "thinking is not supported"), (200, "ok")]);
        assert_eq!(send_thinking_off(&url).unwrap(), "ok");
        let bodies = server.join().unwrap();
        assert!(bodies[2].get("temperature").is_none() && bodies[2].get("thinking").is_none());
    }

    fn fixture_chat(events: String) -> (Result<String, String>, String) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
          // The first local request probes Ollama; the second reads the completion stream.
          for events in [String::from("{}"), events] {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" { break; }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", events.len(), events).unwrap();
          }
        });
        let cfg = LlmConfig { base_url: format!("http://{addr}"), api_key: "fixture".into(), model: "fixture".into(), api: "openai".into(), temperature: None, think: false };
        let mut partial = String::new();
        let result = chat(&cfg, "test", "test", false, |text| partial.push_str(text), |_| {});
        server.join().unwrap();
        (result, partial)
    }

    /// 本地假模型：依次回答 `bodies`（本机地址每次请求前都会先探一下 Ollama，所以探测和正文交替出现）
    fn fixture_server(bodies: Vec<String>) -> (LlmConfig, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" { break; }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut buf = vec![0; length];
                reader.read_exact(&mut buf).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let cfg = LlmConfig { base_url: format!("http://{addr}"), api_key: "fixture".into(), model: "fixture".into(), api: "openai".into(), temperature: None, think: false };
        (cfg, server)
    }

    #[test]
    fn dropped_stream_is_retried_and_the_half_text_cleared() {
        let delta = |t: &str| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{t}\"}}}}]}}\n\n");
        let probe = String::from("{}");
        // 第一次只写了半截就断了，第二次完整
        let (cfg, server) = fixture_server(vec![probe.clone(), delta("半截"), probe, format!("{}data: [DONE]\n\n", delta("完整纪要"))]);
        let shown = std::cell::RefCell::new(String::new());
        let mut retries = Vec::new();
        let result = chat_retry(
            &cfg,
            "test",
            "test",
            false,
            "合并成纪要",
            &mut |t| shown.borrow_mut().push_str(t),
            &mut |_| {},
            &mut |stage| {
                retries.push(stage.to_string());
                shown.borrow_mut().clear();
            },
        );
        server.join().unwrap();
        assert_eq!(result.unwrap(), "完整纪要");
        assert_eq!(retries, ["合并成纪要 · 连接断了，正在重试（1/2）"]);
        assert_eq!(*shown.borrow(), "完整纪要");

        // 一直断：重试 RETRIES 次后报错，并说明怎么避免
        let mut bodies = Vec::new();
        for _ in 0..=RETRIES {
            bodies.extend([String::from("{}"), delta("半截")]);
        }
        let (cfg, server) = fixture_server(bodies);
        let mut retries = 0;
        let result = chat_retry(&cfg, "test", "test", false, "", &mut |_| {}, &mut |_| {}, &mut |_| retries += 1);
        server.join().unwrap();
        assert_eq!(retries, RETRIES);
        assert!(result.unwrap_err().contains("重试后仍然断开"));

        // 模型明确报错（不是断线）不重试
        let (cfg, server) = fixture_server(vec![String::from("{}"), "data: {\"error\":{\"message\":\"quota\"}}\n\n".into()]);
        let mut retries = 0;
        let result = chat_retry(&cfg, "test", "test", false, "", &mut |_| {}, &mut |_| {}, &mut |_| retries += 1);
        server.join().unwrap();
        assert_eq!(retries, 0);
        assert!(result.unwrap_err().contains("quota"));
    }

    #[test]
    fn stream_truncation_keeps_partial_but_never_returns_success() {
        let delta = "data: {\"choices\":[{\"delta\":{\"content\":\"草稿正文\"}}]}\n\n";
        let (truncated, partial) = fixture_chat(delta.into());
        assert!(truncated.is_err());
        assert_eq!(partial, "草稿正文");
        let (limited, partial) = fixture_chat(format!("{delta}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\n\n"));
        assert!(limited.is_err());
        assert_eq!(partial, "草稿正文");
        let (complete, _) = fixture_chat(format!("{delta}data: [DONE]\n\n"));
        assert_eq!(complete.unwrap(), "草稿正文");
    }

    fn reasoning_events(parts: impl Iterator<Item = String>) -> String {
        parts.map(|r| format!("data: {}\n\n", serde_json::json!({"choices": [{"delta": {"reasoning_content": r}}]}))).collect()
    }

    #[test]
    fn long_thinking_on_a_long_meeting_is_not_cut_off() {
        // 7 万字各不相同的思考（比 MAX_STREAM_CHARS 多），然后才写正文
        let thinking = reasoning_events((0..2000).map(|i| format!("核对第{i}条发言的时间戳{}与说话人，", i * 7 % 1000)));
        let done = r#"data: {"choices":[{"delta":{"content":"纪要正文"},"finish_reason":"stop"}]}"#;
        let (result, _) = fixture_chat(format!("{thinking}{done}\n\n"));
        assert_eq!(result.unwrap(), "纪要正文");
    }

    #[test]
    fn thinking_stuck_in_a_loop_is_stopped() {
        let thinking = reasoning_events(std::iter::repeat_n("我再检查一遍格式要求：标题、一句话总结、议题、决议、待办、风险。".to_string(), 400));
        let (result, _) = fixture_chat(format!("{thinking}data: [DONE]\n\n"));
        assert!(result.unwrap_err().contains("重复"));
    }

    #[test]
    fn stream_limits_reject_runaway_and_truncation() {
        assert!(check_stream_size(10_000, 20_000).is_ok());
        // 长会议 + 推理模型：思考 5.9 万字、正文 5 千字是正常的（实测 MiniMax-M3.1-Flash）
        assert!(check_stream_size(5_000, 59_000).is_ok());
        assert!(check_stream_size(5_000, MAX_THINK_CHARS + 1).is_err());
        assert!(check_stream_size(MAX_STREAM_CHARS + 1, 0).is_err());
        assert!(repeated_tail(&"这是模型异常重复的内容。".repeat(256)));
        assert!(!repeated_tail(&"正常纪要的一段。".repeat(3)));
        assert!(finish_reason("length").is_err());
        assert!(finish_reason("max_tokens").is_err());
        assert!(finish_reason("stop").is_ok());
        assert!(completed_text("未完成正文", false).is_err());
        assert!(completed_text("<think>想一想</think>", true).is_err());
        assert_eq!(completed_text("# 完整纪要", true).unwrap(), "# 完整纪要");
        assert_eq!(parse_anthropic_event(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#), AnthropicEvent::Finish("max_tokens".into()));
    }

    #[test]
    fn anthropic_url_and_events() {
        assert_eq!(anthropic_v1("https://api.anthropic.com"), "https://api.anthropic.com/v1");
        assert_eq!(anthropic_v1("https://api.anthropic.com/v1/"), "https://api.anthropic.com/v1");
        assert_eq!(
            parse_anthropic_event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#),
            AnthropicEvent::Text("你好".into())
        );
        assert_eq!(
            parse_anthropic_event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想一想"}}"#),
            AnthropicEvent::Thinking(3)
        );
        assert_eq!(
            parse_anthropic_event(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            AnthropicEvent::Error("Overloaded".into())
        );
        assert_eq!(parse_anthropic_event(r#"{"type":"message_stop"}"#), AnthropicEvent::Stop);
        assert_eq!(parse_anthropic_event(r#"{"type":"ping"}"#), AnthropicEvent::Other);
    }
    use crate::transcript::Sentence;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn annotate_dates_like_python() {
        let x = d(2026, 11, 2); // 星期一
        assert_eq!(annotate_dates("下周五完成", x), "下周五（11月13日）完成");
        assert_eq!(
            annotate_dates("下下周的周一合并", x),
            "下下周的周一（11月16日）合并"
        );
        assert_eq!(
            annotate_dates("这周三、下个星期二、下礼拜天", x),
            "这周三（11月4日）、下个星期二（11月10日）、下礼拜天（11月15日）"
        );
        assert_eq!(
            annotate_dates("明天和后天，大后天", x),
            "明天（11月3日）和后天（11月4日），大后天（11月5日）"
        );
        assert_eq!(
            annotate_dates("周五再说，今天天气好", x),
            "周五再说，今天天气好"
        );
        assert_eq!(
            annotate_dates("下周五（11月13日）", x),
            "下周五（11月13日）"
        );
        assert_eq!(
            annotate_dates("下下周一", d(2026, 11, 29)),
            "下下周一（12月7日）"
        );
    }

    #[test]
    fn calendar_and_strip() {
        assert!(calendar_hint(d(2026, 11, 2)).contains("下周：周一 11月9日"));
        assert_eq!(strip_think("<think>想</think>\n# 标题", false), "# 标题");
        assert_eq!(strip_think("<think>还在想", true), "");
        assert_eq!(strip_think("# 标题<thi", true), "# 标题");
        assert_eq!(strip_fence("```markdown\n# T\n```"), "# T");
    }

    #[test]
    fn chunks_respect_limit() {
        let lines: Vec<String> = (0..10).map(|i| format!("line{i:02}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let chunks = split_chunks(&refs, 20);
        assert!(chunks.iter().all(|c| c.chars().count() <= 21));
        assert_eq!(chunks.join("\n").lines().count(), 10);
    }

    #[test]
    fn meta_filled_by_program_and_glossary() {
        let mut tr = Transcript {
            source: "m.wav".into(),
            duration_ms: 90_000,
            speaker_names: Default::default(),
            sentences: vec![
                Sentence {
                    speaker: 0,
                    start_ms: 0,
                    end_ms: 900,
                    text: "下周五交付。".into(),
                },
                Sentence {
                    speaker: 1,
                    start_ms: 1000,
                    end_ms: 1900,
                    text: "好。".into(),
                },
            ],
        };
        tr.rename(&["张三".into()]);
        let out = with_meta(
            "```markdown\n# 周会\n\n## 一句话总结\n好\n```",
            &tr,
            "2026-11-02（星期一）",
            Lang::Zh,
        );
        assert!(out.starts_with("# 周会\n\n- 日期：2026-11-02（星期一）\n- 时长：01:30\n- 参会人：张三、说话人2\n\n## 一句话总结"));
        assert!(glossary(&["青鸾".into(), " ".into()]).contains("：青鸾。"));
        assert_eq!(glossary(&[]), "");
    }

    #[test]
    fn english_template_headings_match_spec() {
        let lines: Vec<&str> = MINUTES_PROMPT_EN.lines().collect();
        for h in [
            "# <a short title>",
            "## Summary",
            "## Discussion",
            "### 1. <topic>",
            "## Decisions",
            "## Action Items",
            "| Item | Owner | Due | Source |",
            "|---|---|---|---|",
            "## Open Questions & Risks",
            "## Root Cause Analysis",
            "```fishbone",
        ] {
            assert!(lines.contains(&h), "缺少 {h}");
        }
        // 结构与中文模板一一对应
        let heads = |t: &str| t.lines().filter(|l| l.starts_with('#')).count();
        assert_eq!(heads(MINUTES_PROMPT_EN), heads(MINUTES_PROMPT));
        for t in [MINUTES_PROMPT, MINUTES_PROMPT_EN, CHUNK_PROMPT, CHUNK_PROMPT_EN] {
            assert!(t.contains("{glossary}{translate}") && t.contains("{transcript}"));
        }
        // 术语表放在全文最后（鱼骨图之后），合并长会议时的格式要求里也有
        for (t, lang) in [(MINUTES_PROMPT, Lang::Zh), (MINUTES_PROMPT_EN, Lang::En)] {
            let fish = t.find("```fishbone").unwrap();
            let terms = t.find("```terms\n{\"terms\": [{\"term\": ").unwrap();
            assert!(fish < terms && terms < t.find(lang.pick("要求：", "Rules:")).unwrap());
            let instructions = t.split(prompts(lang).transcript_marker).next().unwrap();
            assert!(instructions.contains("```terms") && instructions.contains("{\"terms\": []}"));
        }
        assert!(glossary(&["青鸾".into()]).starts_with("\n9. "));
        // 合并时截取格式要求的分隔只出现一次
        assert_eq!(MINUTES_PROMPT_EN.matches(prompts(Lang::En).transcript_marker).count(), 1);
        assert_eq!(MINUTES_PROMPT.matches(prompts(Lang::Zh).transcript_marker).count(), 1);
        assert!(glossary_in(&["Kafka".into(), "Atlas".into()], Lang::En).contains(": Kafka, Atlas."));
        assert_eq!(translate_note(Lang::Zh, Lang::Zh), "");
        assert_eq!(translate_note(Lang::En, Lang::En), "");
        assert!(translate_note(Lang::En, Lang::Zh).contains("write the minutes in English, translating as needed"));
        assert!(translate_note(Lang::Zh, Lang::En).contains("纪要一律用中文撰写"));
    }

    #[test]
    fn english_dates_and_meta() {
        assert_eq!(day_line(d(2026, 10, 28), Lang::En), "2026-10-28 (Wednesday)");
        assert_eq!(day_line(d(2026, 10, 28), Lang::Zh), "2026-10-28（星期三）");
        let cal = calendar_hint_in(d(2026, 11, 2), Lang::En);
        assert!(cal.contains("- Today is Nov 2 (Monday); tomorrow is Nov 3"));
        assert!(cal.contains("- Next week: Mon Nov 9, Tue Nov 10"));
        assert_eq!(calendar_hint_in(d(2026, 11, 2), Lang::Zh), calendar_hint(d(2026, 11, 2)));
        let mut tr = Transcript {
            source: "m.wav".into(),
            duration_ms: 7_200_000 - 1000 * 3600,
            speaker_names: Default::default(),
            sentences: vec![
                Sentence { speaker: 0, start_ms: 0, end_ms: 900, text: "Let's ship it.".into() },
                Sentence { speaker: 1, start_ms: 1000, end_ms: 1900, text: "OK.".into() },
            ],
        };
        tr.rename(&["Lin Zhou".into()]);
        let tr = tr.localized(Lang::En);
        let out = with_meta("# Weekly sync\n\n## Summary\nShip it.", &tr, "2026-10-28 (Wednesday)", Lang::En);
        assert!(out.starts_with(
            "# Weekly sync\n\n- Date: 2026-10-28 (Wednesday)\n- Duration: 1:00:00\n- Participants: Lin Zhou, Speaker 2\n\n## Summary"
        ));
        assert!(with_meta("no title", &Transcript::default(), "x", Lang::En).starts_with("# Meeting Minutes\n\n- Date: x\n- Duration: 00:00\n- Participants: None"));
    }

    #[test]
    fn minutes_language_follows_transcript() {
        let mk = |texts: &[&str]| Transcript {
            sentences: texts
                .iter()
                .enumerate()
                .map(|(i, t)| Sentence { speaker: i % 2, start_ms: i as u64 * 1000, end_ms: i as u64 * 1000 + 900, text: t.to_string() })
                .collect(),
            ..Default::default()
        };
        let zh = mk(&["下周五交付 API 网关。", "好，Kafka 那边我来跟。"]);
        let en = mk(&["We will ship the gateway next Friday.", "Sounds good, I'll own Kafka."]);
        assert_eq!(minutes_lang("auto", &zh), Lang::Zh);
        assert_eq!(minutes_lang("auto", &en), Lang::En); // 默认名"说话人1"不算进去
        assert_eq!(minutes_lang("en", &zh), Lang::En);
        assert_eq!(minutes_lang("zh", &en), Lang::Zh);
    }

    #[test]
    fn stage_text_follows_ui_language() {
        let _g = i18n::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        i18n::set_ui(Lang::En);
        assert_eq!(tr_stage(2, 5), "Summarizing part 2/5");
        i18n::set_ui(Lang::Zh);
        assert_eq!(tr_stage(2, 5), "分段提炼 2/5");
    }
}
