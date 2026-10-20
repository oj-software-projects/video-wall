use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP: usize = 10;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn log_dir() -> PathBuf {
    home().join(".video-wall").join("logs")
}

pub fn log_path() -> PathBuf {
    log_dir().join("app.log")
}

static SINK: LazyLock<Mutex<Option<File>>> = LazyLock::new(|| Mutex::new(open_sink()));

fn sink() -> &'static Mutex<Option<File>> {
    &SINK
}

fn open_sink() -> Option<File> {
    let dir = log_dir();
    fs::create_dir_all(&dir).ok()?;
    let path = log_path();
    rotate_if_needed(&path);
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn rotate_if_needed(path: &Path) {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_BYTES {
        return;
    }
    let _ = fs::remove_file(path.with_file_name(format!("app.{KEEP}.log")));
    for i in (1..KEEP).rev() {
        let from = path.with_file_name(format!("app.{i}.log"));
        let to = path.with_file_name(format!("app.{}.log", i + 1));
        let _ = fs::rename(from, to);
    }
    let _ = fs::rename(path, path.with_file_name("app.1.log"));
}

pub fn write(level: &str, logger: &str, message: &str, meta: Option<serde_json::Value>) {
    let mut line = format!("[{}] [{}] [{}] {}", iso_now(), level, logger, message);
    if let Some(meta) = meta {
        line.push(' ');
        line.push_str(&meta.to_string());
    }
    line.push('\n');

    if let Ok(mut guard) = sink().lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    match level {
        "ERROR" => eprint!("[ERROR] {logger}: {line}"),
        "WARN" => eprint!("warning: {logger}: {line}"),
        _ => {}
    }
}

pub fn debug(logger: &str, message: &str) {
    write("DEBUG", logger, message, None);
}

pub fn info(logger: &str, message: &str) {
    write("INFO", logger, message, None);
}

pub fn warn(logger: &str, message: &str) {
    write("WARN", logger, message, None);
}

pub fn error(logger: &str, message: &str) {
    write("ERROR", logger, message, None);
}

pub fn iso_now() -> String {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let ms = dur.subsec_millis();
    let (y, mo, d, h, mi, s) = civil_from_secs(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

fn civil_from_secs(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if mo <= 2 { y + 1 } else { y }, mo, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_epoch() {
        assert_eq!(
            civil_from_secs(0),
            (1970, 1, 1, 0, 0, 0)
        );
        assert_eq!(civil_from_secs(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    }
}
