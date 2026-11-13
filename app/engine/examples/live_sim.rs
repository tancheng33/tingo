//! 模拟边录边转：把一个 16 kHz wav 按真实速度写进正在增长的音轨，量每句从说完到识别出来的延迟，
//! 并和整段文件转写逐字对比，看分段变短后准确率有没有变化。
//! cargo run --release --example live_sim -- <模型目录> <wav> [倍速，默认 1] [标准文本 transcript.json]
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tingo_engine::{asr, audio, chunked, record};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let speed: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let engine = Arc::new(asr::Engine::load(&asr::ModelPaths::new(&args[1]), asr::AsrKind::Paraformer, &[], "zh", 4).expect("加载模型"));
    let samples = audio::read_wav16k(Path::new(&args[2])).expect("读 wav");
    let dir = std::env::temp_dir().join(format!("tingo-live-sim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let track = dir.join("system.wav");
    record::write_wav16(&track, &[]).unwrap();

    let live = chunked::LiveTranscriber::start(engine.clone(), &dir, &["system".to_string()]);
    let writer = {
        let (track, samples) = (track.clone(), samples.clone());
        std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new().append(true).open(&track).unwrap();
            let step = asr::SR / 10;
            for part in samples.chunks(step) {
                let bytes: Vec<u8> = part.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
                f.write_all(&bytes).unwrap();
                std::thread::sleep(Duration::from_secs_f64(0.1 / speed));
            }
        })
    };
    let t0 = Instant::now();
    let mut seen = 0;
    // 每句出来 0.5 秒后显示的说话人（界面每 0.4 秒取一次，看到的就是这个）
    let (mut first_shown, mut appeared): (Vec<Option<usize>>, Vec<Instant>) = (Vec::new(), Vec::new());
    while !writer.is_finished() {
        let lines = live.recent(usize::MAX);
        appeared.extend(lines[seen..].iter().map(|_| Instant::now()));
        while first_shown.len() < appeared.len() && appeared[first_shown.len()].elapsed() >= Duration::from_millis(500) {
            first_shown.push(lines[first_shown.len()].speaker);
        }
        for l in &lines[seen..] {
            println!("{:>6.1}s 出字 | {:>5.1}s 开始 | 说话人{} | {}", t0.elapsed().as_secs_f64() * speed, l.start_ms as f64 / 1000.0,
                l.speaker.map_or("?".into(), |s| (s + 1).to_string()), l.text);
        }
        seen = lines.len();
        std::thread::sleep(Duration::from_millis(50));
    }
    writer.join().unwrap();
    let last_shown: Vec<Option<usize>> = live.recent(usize::MAX).iter().map(|l| l.speaker).collect();
    let (tr_live, t) = live.finish(&engine, &track).expect("结束");
    // 以结束后整场聚类为准，看录音中显示的说话人对不对（每个临时编号映射到它最常对应的最终说话人）
    let fin: Vec<usize> = tr_live.sentences.iter().map(|s| s.speaker).collect();
    let agree = |shown: &[Option<usize>]| {
        let mut m = std::collections::HashMap::<(usize, usize), usize>::new();
        for (s, f) in shown.iter().zip(&fin) {
            if let Some(s) = s { *m.entry((*s, *f)).or_default() += 1; }
        }
        // 一一对应：按共现次数从多到少配对，每个临时编号、每位最终说话人只用一次
        let mut pairs: Vec<_> = m.into_iter().collect();
        pairs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let (mut used_s, mut used_f, mut n) = (std::collections::HashSet::new(), std::collections::HashSet::new(), 0);
        for ((s, f), c) in pairs {
            if !used_s.contains(&s) && !used_f.contains(&f) {
                used_s.insert(s);
                used_f.insert(f);
                n += c;
            }
        }
        let k = shown.iter().flatten().collect::<std::collections::HashSet<_>>().len();
        format!("{} 位，和最终一致 {:.1}%", k, n as f64 * 100.0 / shown.len().max(1) as f64)
    };
    println!("说话人：每句出来 0.5 秒后 {}；录音结束时 {}", agree(&first_shown), agree(&last_shown));
    println!("\n说完到出字：平均 {:.2}s，最长 {:.2}s；结束后收尾 {:.2}s", t.live_lag_avg_s, t.live_lag_max_s, t.tail_s);

    let tr_file = chunked::transcribe_samples(&engine, &samples, &track, |_| {}).expect("整段转写");
    let plain = |tr: &tingo_engine::transcript::Transcript| -> Vec<char> {
        tr.sentences.iter().flat_map(|s| s.text.chars()).filter(|c| c.is_alphanumeric()).collect()
    };
    let (a, b) = (plain(&tr_file), plain(&tr_live));
    let d = edit_distance(&a, &b);
    println!("整段转写 {} 字，边录边转 {} 字，逐字差异 {} 处（{:.1}%）", a.len(), b.len(), d, d as f64 * 100.0 / a.len().max(1) as f64);
    println!("说话人：整段 {} 位，边录边转结束后聚类 {} 位", tr_file.speakers_in_order().len(), tr_live.speakers_in_order().len());
    // 有人工校对过的标准文本时，分别算两种方式的字错率
    if let Some(reference) = args.get(4) {
        let r = plain(&tingo_engine::transcript::Transcript::load(Path::new(reference)).expect("读标准文本"));
        let cer = |x: &[char]| edit_distance(&r, x) as f64 * 100.0 / r.len().max(1) as f64;
        println!("字错率（对照标准文本 {} 字）：整段转写 {:.1}%，边录边转 {:.1}%", r.len(), cer(&a), cer(&b));
    }
    std::fs::remove_dir_all(dir).ok();
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (x != y) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}
