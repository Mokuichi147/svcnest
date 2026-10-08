use crate::{paths::Paths, process::interrupt};
use anyhow::Result;
use chrono::Local;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
pub const LOG_GENERATIONS: usize = 5;
const MAX_LINE_BYTES: usize = 64 * 1024;

pub struct RotatingLog {
    path: PathBuf,
    file: File,
    bytes: u64,
    max_bytes: u64,
    generations: usize,
}
pub type SharedLog = Arc<Mutex<RotatingLog>>;

impl RotatingLog {
    pub fn new(path: &Path) -> Result<Self> {
        Self::with_limits(path, MAX_LOG_BYTES, LOG_GENERATIONS)
    }
    pub fn with_limits(path: &Path, max_bytes: u64, generations: usize) -> Result<Self> {
        anyhow::ensure!(
            max_bytes > 0 && generations > 0,
            "Invalid log rotation limits"
        );
        let file = open_log(path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            path: path.to_owned(),
            file,
            bytes,
            max_bytes,
            generations,
        })
    }
    pub fn record(&mut self, stream: &str, bytes: &[u8]) -> Result<()> {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches('\r');
        let line = format!(
            "{} {stream} | {text}\n",
            Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
        );
        if self.bytes > 0 && self.bytes + line.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.bytes += line.len() as u64;
        Ok(())
    }
    fn rotate(&mut self) -> Result<()> {
        // Windows でも rename 前に書き込み側のファイルハンドルを閉じる。
        let replacement = tempfile::tempfile()?;
        let old = std::mem::replace(&mut self.file, replacement);
        old.sync_data()?;
        drop(old);
        if self.generations > 1 {
            remove_if_exists(&generation(&self.path, self.generations - 1))?;
            for index in (1..self.generations - 1).rev() {
                let from = generation(&self.path, index);
                if from.exists() {
                    fs::rename(from, generation(&self.path, index + 1))?;
                }
            }
            fs::rename(&self.path, generation(&self.path, 1))?;
        } else {
            remove_if_exists(&self.path)?;
        }
        self.file = open_log(&self.path)?;
        self.bytes = 0;
        Ok(())
    }
}

fn open_log(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

pub fn generation(path: &Path, number: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{number}"));
    PathBuf::from(name)
}

pub fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn purge(paths: &Paths, name: &str) -> Result<()> {
    remove_if_exists(&paths.log(name))?;
    for number in 1..LOG_GENERATIONS {
        remove_if_exists(&generation(&paths.log(name), number))?;
    }
    Ok(())
}

pub async fn capture(
    mut input: impl AsyncRead + Unpin,
    stream: &'static str,
    log: SharedLog,
) -> Result<()> {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            if *byte == b'\n' || pending.len() >= MAX_LINE_BYTES {
                log.lock()
                    .map_err(|_| anyhow::anyhow!("Log writer lock poisoned"))?
                    .record(stream, &pending)?;
                pending.clear();
            }
            if *byte != b'\n' {
                pending.push(*byte);
            }
        }
    }
    if !pending.is_empty() {
        log.lock()
            .map_err(|_| anyhow::anyhow!("Log writer lock poisoned"))?
            .record(stream, &pending)?;
    }
    Ok(())
}

pub fn tail(path: &Path, count: usize) -> Result<String> {
    let current = read_log(path)?
        .map(|file| {
            let end = file.metadata()?.len();
            Ok::<_, anyhow::Error>((file, end))
        })
        .transpose()?;
    tail_snapshot(path, count, current)
}

fn read_log(path: &Path) -> Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn tail_snapshot(path: &Path, count: usize, mut current: Option<(File, u64)>) -> Result<String> {
    // ファイル全体をメモリへ読み込まず、後ろから必要な行を探す。
    let mut remaining = count;
    let mut chunks = Vec::new();
    let mut seen = Vec::new();
    for number in 0..LOG_GENERATIONS {
        let (mut file, mut position) = if number == 0 {
            let Some(current) = current.take() else {
                continue;
            };
            current
        } else {
            let Some(file) = read_log(&generation(path, number))? else {
                continue;
            };
            let end = file.metadata()?.len();
            (file, end)
        };
        if seen
            .iter()
            .any(|old| same_file(old, &file).unwrap_or(false))
        {
            continue;
        }
        let mut parts = Vec::new();
        let mut lines = 0;
        while position > 0 && lines <= remaining {
            let length = position.min(8192) as usize;
            position -= length as u64;
            file.seek(SeekFrom::Start(position))?;
            let mut part = vec![0u8; length];
            file.read_exact(&mut part)?;
            lines += part.iter().filter(|byte| **byte == b'\n').count();
            parts.push(part);
        }
        let bytes = parts.into_iter().rev().flatten().collect::<Vec<_>>();
        seen.push(file);
        let text = String::from_utf8_lossy(&bytes);
        let selected = text
            .lines()
            .rev()
            .take(remaining)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        remaining -= selected.len();
        if !selected.is_empty() {
            chunks.push(format!("{}\n", selected.join("\n")));
        }
        if remaining == 0 {
            break;
        }
    }
    Ok(chunks.into_iter().rev().collect())
}

pub async fn show(path: &Path, count: usize, follow: bool) -> Result<()> {
    let mut current = read_log(path)?;
    let mut position = current
        .as_ref()
        .map(|file| file.metadata().map(|m| m.len()))
        .transpose()?
        .unwrap_or(0);
    let snapshot = current
        .as_ref()
        .map(|file| file.try_clone().map(|file| (file, position)))
        .transpose()?;
    print!("{}", tail_snapshot(path, count, snapshot)?);
    std::io::stdout().flush()?;
    if !follow {
        return Ok(());
    }
    let signal = interrupt();
    tokio::pin!(signal);
    let mut interval = tokio::time::interval(Duration::from_millis(150));
    loop {
        tokio::select! {
            _ = &mut signal => break,
            _ = interval.tick() => {
                follow_step(path, &mut current, &mut position, &mut std::io::stdout())?;
                std::io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

fn follow_step(
    path: &Path,
    current: &mut Option<File>,
    position: &mut u64,
    output: &mut impl Write,
) -> Result<()> {
    if let Some(file) = current {
        if file.metadata()?.len() < *position {
            *position = 0;
        }
        file.seek(SeekFrom::Start(*position))?;
        *position += std::io::copy(file, output)?;
    }
    let Some(active) = read_log(path)? else {
        return Ok(());
    };
    if current
        .as_ref()
        .map(|old| same_file(old, &active))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(());
    }
    let mut generations = Vec::new();
    let mut anchor = 0;
    for number in 1..LOG_GENERATIONS {
        if let Some(file) = read_log(&generation(path, number))? {
            // 列挙中にも rotate する。取得した active より新しい世代は次の poll へ回す。
            if same_file(&file, &active)? {
                anchor = generations.len() + 1;
            }
            generations.push(file);
        }
    }
    let mut files = generations
        .into_iter()
        .skip(anchor)
        .rev()
        .collect::<Vec<_>>();
    files.push(active);
    let mut begin = files.len() - 1;
    if let Some(old) = current.as_ref() {
        begin = 0;
        for (index, file) in files.iter().enumerate() {
            if same_file(old, file)? {
                begin = index + 1;
                break;
            }
        }
    }
    let mut seen = Vec::new();
    for mut file in files.into_iter().skip(begin) {
        if seen
            .iter()
            .any(|old| same_file(old, &file).unwrap_or(false))
        {
            continue;
        }
        file.seek(SeekFrom::Start(0))?;
        let end = std::io::copy(&mut file, output)?;
        seen.push(file.try_clone()?);
        *current = Some(file);
        *position = end;
    }
    Ok(())
}

fn same_file(a: &File, b: &File) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = a.metadata()?;
        let b = b.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        crate::platform::windows::same_file(a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn following_starts_at_the_same_snapshot_that_was_printed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("service.log");
        let mut log = RotatingLog::new(&path).unwrap();
        log.record("stdout", b"first").unwrap();
        let mut reader = File::open(&path).unwrap();
        let end = reader.metadata().unwrap().len();
        log.record("stdout", b"second").unwrap();
        let initial = tail_snapshot(&path, 100, Some((reader.try_clone().unwrap(), end))).unwrap();
        assert!(initial.contains("first"));
        assert!(!initial.contains("second"));
        reader.seek(SeekFrom::Start(end)).unwrap();
        let mut appended = String::new();
        reader.read_to_string(&mut appended).unwrap();
        assert!(appended.contains("second"));
        assert!(!appended.contains("first"));
    }

    #[test]
    fn follow_reads_every_retained_generation_after_multiple_rotations() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("service.log");
        let mut log = RotatingLog::with_limits(&path, 100, 5).unwrap();
        log.record("stdout", format!("record-0 {}", "x".repeat(40)).as_bytes())
            .unwrap();
        let mut current = Some(File::open(&path).unwrap());
        let mut position = current.as_ref().unwrap().metadata().unwrap().len();
        for index in 1..4 {
            log.record(
                "stdout",
                format!("record-{index} {}", "x".repeat(40)).as_bytes(),
            )
            .unwrap();
        }
        let mut output = Vec::new();
        follow_step(&path, &mut current, &mut position, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("record-0"));
        for index in 1..4 {
            assert_eq!(text.matches(&format!("record-{index}")).count(), 1);
        }
        let mut output = Vec::new();
        follow_step(&path, &mut current, &mut position, &mut output).unwrap();
        assert!(output.is_empty());
    }
}
