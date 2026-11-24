//! 实时纪要：录音时每隔两分钟把新说的话交给大模型，边开会边整理一份「速记版」纪要。
//! 会议结束后照常用整场转写完整写一遍正式纪要，速记版只在开会时看。
//!
//! 每次只把最后几个议题和总结、决议、待办、风险交给模型改，更早的议题定稿后不再动。
//! 每次整份重写的话纪要越写越长、越来越慢：实测一小时的会，到第 24 分钟一次更新要 72 秒，
//! 纪要膨胀到 1.6 万字；只改最后几个议题，每次 4～31 秒，跟得上两分钟一次。
//! 不思考：要的是快，正式纪要在会后写。

use chrono::Datelike;

use crate::config::LlmConfig;
use crate::i18n::Lang;
use crate::summarize;

/// 两次更新至少隔这么久（秒）
pub const INTERVAL_S: f64 = 120.0;
/// 上次更新失败时隔这么久（秒）再试
pub const RETRY_S: f64 = 30.0;
/// 第一次整理前至少要说了这么多字，太少写不出东西
pub const FIRST_CHARS: usize = 300;
/// 之后每次至少有这么多新字才更新
pub const MIN_NEW_CHARS: usize = 120;
/// 留着可以改的议题数，更早的定稿
const LIVE_TOPICS: usize = 3;

const SYSTEM_ZH: &str = "你是会议记录员，正在开会的同时实时整理会议纪要。只输出要求的 Markdown，不要任何解释。";
const SYSTEM_EN: &str = "You are a minute-taker writing meeting minutes live while the meeting is still going. Output only the requested Markdown, with no explanations.";

const PROMPT_ZH: &str = "会议还在进行。{today}
前面已经定稿的议题（只给标题，不用再写）：
{frozen}

正在整理的部分（可以修改）：
{live}

之后新说的话：
{lines}

请输出更新后的「正在整理的部分」，格式固定：
## 一句话总结
（一句话，概括到目前为止的整场会议）
## 议题与讨论
### 议题名
- 要点（每条不超过 40 字，结尾标时间戳，如 [12:07]）
（只写正在整理的议题和新议题，不要重复定稿的议题。议题是大的讨论主题，不是每段话一个：新内容和正在整理的议题沾边就并进去、改个更概括的议题名；只有明显换了大话题才另起 ### 议题，一次最多新开一个；每个议题最多 6 条要点，多了就合并相近的要点）
## 决议
## 待办
| 事项 | 负责人 | 截止 |
|---|---|---|
## 待定问题与风险

要求：只依据转写，不编造；决议、待办、风险是整场会议的，保留已有的，按新内容增改，不要重复；没有就写「无」。听错的专有名词按上下文改正。{glossary}";

const PROMPT_EN: &str = "The meeting is still going. {today}
Topics already settled (titles only, don't write them again):
{frozen}

The part still being written (you may change it):
{live}

What has been said since:
{lines}

Output the updated \"part still being written\" in exactly this format:
## Summary
(one sentence covering the whole meeting so far)
## Discussion
### Topic name
- Point (at most 25 words, ending with a timestamp such as [12:07])
(Only the topics being written and new ones; never repeat a settled topic. A topic is a broad subject, not one per exchange: fold new content into a topic being written whenever it is related, renaming the topic more broadly if needed; start a new ### topic only when the discussion clearly moves to a new subject, and at most one per update; at most 6 points per topic, merging similar ones.)
## Decisions
## Action Items
| Item | Owner | Due |
|---|---|---|
## Open Questions & Risks

Rules: use only the transcript and make nothing up; decisions, action items and risks cover the whole meeting, so keep the existing ones and add or revise them from the new content without duplicates; write \"None\" when there are none. Fix misheard proper nouns from context.{glossary}

Write the minutes in English.";

const SUMMARY: [&str; 2] = ["一句话总结", "Summary"];
const TOPICS: [&str; 2] = ["议题与讨论", "Discussion"];

/// 一份速记版纪要
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub summary: String,
    /// 定稿的议题（「标题\n要点…」，不带 ###）
    frozen: Vec<String>,
    /// 还能改的最后几个议题
    live: Vec<String>,
    /// 决议、待办、待定问题与风险（原样的 Markdown，从「## 决议」起）
    rest: String,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.frozen.is_empty() && self.live.is_empty() && self.rest.is_empty()
    }

    pub fn topic_count(&self) -> usize {
        self.frozen.len() + self.live.len()
    }

    /// 整份速记版
    pub fn markdown(&self, lang: Lang) -> String {
        let topics: Vec<String> = self.frozen.iter().chain(&self.live).enumerate().map(|(i, t)| format!("### {}. {t}", i + 1)).collect();
        format!(
            "## {}\n{}\n\n## {}\n{}\n\n{}",
            lang.pick(SUMMARY[0], SUMMARY[1]),
            self.summary,
            lang.pick(TOPICS[0], TOPICS[1]),
            topics.join("\n\n"),
            self.rest
        )
    }

    /// 交给模型改的部分：总结、最后几个议题、决议待办风险
    fn editable(&self, lang: Lang) -> String {
        if self.is_empty() {
            return lang.pick("（还没有内容）", "(nothing yet)").into();
        }
        let topics: Vec<String> = self.live.iter().map(|t| format!("### {t}")).collect();
        format!(
            "## {}\n{}\n\n## {}\n{}\n\n{}",
            lang.pick(SUMMARY[0], SUMMARY[1]),
            self.summary,
            lang.pick(TOPICS[0], TOPICS[1]),
            topics.join("\n\n"),
            self.rest
        )
    }
}

/// 议题标题（第一行）
fn title(topic: &str) -> &str {
    topic.lines().next().unwrap_or("").trim()
}

/// 去掉模型自己加的编号：「1. 议题」「2、议题」
fn strip_number(t: &str) -> &str {
    let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() < t.len() {
        if let Some(r) = rest.strip_prefix(['.', '、', '．', ')']) {
            return r.trim_start();
        }
    }
    t
}

/// 拆模型的回复：一句话总结、议题、从决议起的其余部分。认中英文两种标题；缺了哪块就当回复不合格
fn parse(reply: &str) -> Option<(String, Vec<String>, String)> {
    let reply = reply.replace("\r\n", "\n");
    // 每个「## 」开头的节：(标题, 正文)
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in reply.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            sections.push((h.trim().to_string(), String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let is = |h: &str, names: [&str; 2]| names.iter().any(|n| h.starts_with(n));
    let summary = sections.iter().find(|(h, _)| is(h, SUMMARY))?.1.trim().to_string();
    let t = sections.iter().position(|(h, _)| is(h, TOPICS))?;
    let topics: Vec<String> = sections[t]
        .1
        .split("\n### ")
        .map(|x| x.trim().trim_start_matches("### ").trim())
        .filter(|x| !x.is_empty())
        .map(|x| strip_number(x).to_string())
        .collect();
    let rest: Vec<String> = sections[t + 1..].iter().map(|(h, b)| format!("## {h}\n{}", b.trim())).collect();
    if summary.is_empty() || topics.is_empty() || rest.is_empty() {
        return None;
    }
    Some((summary, topics, rest.join("\n\n")))
}

/// 用模型的回复更新纪要；回复格式不对时返回 None，原纪要不动
pub fn apply(notes: &Notes, reply: &str) -> Option<Notes> {
    let (summary, topics, rest) = parse(summarize::strip_think(reply, false).trim())?;
    let mut next = Notes { summary, frozen: notes.frozen.clone(), live: Vec::new(), rest };
    // 模型偶尔把定稿的议题又写一遍：按标题去掉
    let settled: Vec<&str> = notes.frozen.iter().map(|t| title(t)).collect();
    next.live = topics.into_iter().filter(|t| !settled.contains(&title(t))).collect();
    if next.live.is_empty() {
        return None;
    }
    while next.live.len() > LIVE_TOPICS {
        next.frozen.push(next.live.remove(0));
    }
    Some(next)
}

fn today(lang: Lang) -> String {
    let d = chrono::Local::now().date_naive();
    match lang {
        Lang::Zh => {
            let w = ["一", "二", "三", "四", "五", "六", "日"][d.weekday().num_days_from_monday() as usize];
            format!("今天是 {}（星期{w}）。", d.format("%Y-%m-%d"))
        }
        Lang::En => format!("Today is {}.", d.format("%A, %Y-%m-%d")),
    }
}

/// 给模型的提示词。`lines` 是新说的话，一行一段「[分:秒] 说话人：内容」
pub fn prompt(notes: &Notes, lines: &str, hotwords: &[String], lang: Lang) -> String {
    let frozen = if notes.frozen.is_empty() {
        lang.pick("（无）", "(none)").to_string()
    } else {
        notes.frozen.iter().map(|t| format!("- {}", title(t))).collect::<Vec<_>>().join("\n")
    };
    let words: Vec<&str> = hotwords.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    let glossary = if words.is_empty() {
        String::new()
    } else {
        match lang {
            Lang::Zh => format!("\n专有名词的正确写法：{}。", words.join("、")),
            Lang::En => format!("\nCorrect spellings of proper nouns: {}.", words.join(", ")),
        }
    };
    lang.pick(PROMPT_ZH, PROMPT_EN)
        .replace("{today}", &today(lang))
        .replace("{frozen}", &frozen)
        .replace("{live}", &notes.editable(lang))
        .replace("{lines}", lines)
        .replace("{glossary}", &glossary)
}

/// 把新说的话交给模型，返回更新后的纪要
pub fn update(cfg: &LlmConfig, notes: &Notes, lines: &str, hotwords: &[String], lang: Lang) -> Result<Notes, String> {
    let p = prompt(notes, lines, hotwords, lang);
    let reply = summarize::chat(cfg, lang.pick(SYSTEM_ZH, SYSTEM_EN), &p, false, |_| {}, |_| {})?;
    apply(notes, &reply).ok_or_else(|| {
        crate::i18n::tr("大模型这次回的格式不对，下次再更新", "The model's reply wasn't in the expected format; will try again next time").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(topics: &[&str]) -> String {
        let t: Vec<String> = topics.iter().map(|x| format!("### {x}\n- 要点 [01:00]")).collect();
        format!("## 一句话总结\n讨论了合作。\n\n## 议题与讨论\n{}\n\n## 决议\n- 无\n\n## 待办\n| 事项 | 负责人 | 截止 |\n|---|---|---|\n| 发报价 | 小王 | 周三 |\n\n## 待定问题与风险\n- 无", t.join("\n\n"))
    }

    #[test]
    fn later_topics_settle_and_stay_out_of_the_prompt() {
        let n = apply(&Notes::default(), &reply(&["1. 接口对接", "2、结算方式", "定价", "算力"])).unwrap();
        // 只留最后三个能改，编号去掉、渲染时统一重编
        assert_eq!((n.frozen.len(), n.live.len()), (1, 3));
        let md = n.markdown(Lang::Zh);
        assert!(md.contains("### 1. 接口对接") && md.contains("### 4. 算力") && md.contains("| 发报价 | 小王 | 周三 |"), "{md}");
        let p = prompt(&n, "[05:00] 说话人1：下周三前发报价。", &["星河科技".into()], Lang::Zh);
        assert!(p.contains("- 接口对接") && !p.contains("### 接口对接"), "定稿的只给标题");
        assert!(p.contains("### 算力") && p.contains("[05:00] 说话人1") && p.contains("专有名词的正确写法：星河科技"));
    }

    #[test]
    fn a_settled_topic_written_again_is_dropped() {
        let n = apply(&Notes::default(), &reply(&["接口对接", "结算方式", "定价", "算力"])).unwrap();
        // 「结算方式」「定价」没回来：模型把它们并进别的议题了（正在整理的议题允许合并）
        let n = apply(&n, &reply(&["接口对接", "算力", "部署"])).unwrap();
        assert_eq!(n.topic_count(), 3);
        assert_eq!(n.markdown(Lang::Zh).matches("接口对接").count(), 1);
    }

    #[test]
    fn a_malformed_reply_keeps_the_notes() {
        let n = apply(&Notes::default(), &reply(&["接口对接"])).unwrap();
        for bad in ["好的，我来整理。", "## 一句话总结\n讨论了合作。\n\n## 议题与讨论\n### 定价\n- 要点", "## 议题与讨论\n### 定价\n## 决议\n- 无"] {
            assert!(apply(&n, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn english_minutes_and_thinking_tags_are_understood() {
        let r = "<think>let me see</think>## Summary\nWe agreed on pricing.\n\n## Discussion\n### 1. Pricing\n- Ship by Friday [02:10]\n\n## Decisions\n- Ship by Friday\n\n## Action Items\n| Item | Owner | Due |\n|---|---|---|\n\n## Open Questions & Risks\n- None";
        let n = apply(&Notes::default(), r).unwrap();
        let md = n.markdown(Lang::En);
        assert!(md.starts_with("## Summary\nWe agreed on pricing.") && md.contains("### 1. Pricing") && md.contains("## Decisions"), "{md}");
        assert!(prompt(&n, "", &[], Lang::En).contains("Write the minutes in English."));
    }
}
