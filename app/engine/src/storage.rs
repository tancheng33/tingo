//! Copy a meeting library without deleting or overwriting the original files.
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::i18n::tr;

#[derive(Clone, Default, Serialize)]
pub struct Progress {
    pub active: bool,
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

pub fn destination(source: &Path, input: &str) -> Result<PathBuf, String> {
    let path = crate::platform::expand_home(input.trim()).ok_or("HOME unavailable")?;
    if !path.is_absolute() { return Err(tr("请选择绝对路径的文件夹", "Choose an absolute folder path").into()); }
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.is_dir() { return Err(tr("请选择文件夹", "Choose a folder").into()); }
    let source = source.canonicalize().unwrap_or_else(|_| source.to_path_buf());
    if path == source { return Ok(path); }
    if path.starts_with(&source) || source.starts_with(&path) {
        return Err(tr("新旧目录不能互相包含，请选择另一个独立文件夹", "The folders must not contain each other").into());
    }
    let probe = path.join(format!(".tingo-write-check-{}", std::process::id()));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&probe).map_err(|e| e.to_string())?;
    let result = file.write_all(b"Tingo").and_then(|_| file.sync_all());
    drop(file);
    let _ = fs::remove_file(probe);
    result.map_err(|e| e.to_string())?;
    Ok(path)
}

fn inventory(dir: &Path, entries: &mut Vec<(PathBuf, bool, u64)>, root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_dir() && !meta.is_file() {
            return Err(format!("{}: {}", tr("目录中有链接或特殊文件，无法自动复制", "Cannot copy links or special files"), path.display()));
        }
        entries.push((path.strip_prefix(root).unwrap().to_path_buf(), meta.is_dir(), meta.len()));
        if meta.is_dir() { inventory(&path, entries, root)?; }
    }
    Ok(())
}

/// Stage and verify every file first. Errors never remove the source library.
pub fn copy_library(source: &Path, target: &Path, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    if fs::read_dir(target).map_err(|e| e.to_string())?.next().is_some() {
        return Err(tr("复制会议需要一个空文件夹，避免覆盖已有文件", "Choose an empty folder to copy meetings without overwriting files").into());
    }
    let mut entries = Vec::new();
    inventory(source, &mut entries, source)?;
    let total = entries.iter().filter(|(_, d, _)| !d).map(|(_, _, n)| n).sum();
    if crate::meetings::free_bytes(target).is_some_and(|free| free < total) {
        return Err(tr("目标磁盘空间不足", "Not enough space on the destination disk").into());
    }
    progress(0, total);
    let stage = target.join(format!(".tingo-copy-{}", std::process::id()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut copied = 0;
        let mut buffer = vec![0u8; 1024 * 1024];
        for (relative, directory, _) in entries {
            let src = source.join(&relative);
            let dst = stage.join(&relative);
            if directory { fs::create_dir(&dst).map_err(|e| e.to_string())?; continue; }
            let mut input = fs::File::open(&src).map_err(|e| e.to_string())?;
            let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&dst).map_err(|e| e.to_string())?;
            loop {
                let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                copied += n as u64;
                progress(copied, total);
            }
            // Keep history timestamps and the dates of imported meetings.
            output.set_times(fs::FileTimes::new().set_modified(input.metadata().map_err(|e| e.to_string())?.modified().map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
            if crate::update::sha256_file(&src).map_err(|e| e.to_string())? != crate::update::sha256_file(&dst).map_err(|e| e.to_string())? {
                return Err(tr("复制校验失败，仍使用原目录", "Copy verification failed; the original folder is still in use").into());
            }
        }
        for entry in fs::read_dir(&stage).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let destination = target.join(entry.file_name());
            if destination.exists() { return Err(tr("目标目录出现同名文件，已停止切换", "A conflicting file appeared; switching stopped").into()); }
            fs::rename(entry.path(), destination).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(stage);
    result
}
