//! 从备份的会议根目录恢复 iOS 中断录音，不加载识别模型，不修改原始音轨。
//! cargo run --example recover -- <会议根目录>
fn main() {
    let root = std::env::args_os().nth(1).expect("usage: recover <meetings-root>");
    tingo_engine::record::recover_interrupted(std::path::Path::new(&root));
}
