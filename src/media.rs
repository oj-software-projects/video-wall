use serde_json::Value;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::scan::extension;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Direct,
    Remux,
    Transcode,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Direct => "direct",
            Mode::Remux => "remux",
            Mode::Transcode => "transcode",
        }
    }

    pub fn parse(value: &str) -> Option<Mode> {
        match value {
            "direct" => Some(Mode::Direct),
            "remux" => Some(Mode::Remux),
            "transcode" => Some(Mode::Transcode),
            _ => None,
        }
    }

    pub fn from_ext(ext: &str) -> Mode {
        match ext {
            "mkv" | "avi" | "flv" | "wmv" | "3gp" => Mode::Remux,
            "mpg" | "mpeg" => Mode::Transcode,
            _ => Mode::Direct,
        }
    }
}

/// Extensions that get an alternate transcode URL for fallback playback.
pub fn is_fallback_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mov" | "m4v" | "mkv" | "avi" | "flv" | "wmv" | "mpg" | "mpeg" | "3gp" | "ogv"
    )
}

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: Option<PathBuf>,
}

fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    #[cfg(target_os = "windows")]
    {
        dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("WinGet").join("Links"));
        }
    }
    dirs
}

fn works(binary: &Path) -> bool {
    let mut child = match Command::new(binary)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if started.elapsed() > Duration::from_secs(3) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
}

fn find_binary(env_var: &str, name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os(env_var) {
        candidates.push(PathBuf::from(explicit));
    }
    for dir in candidate_dirs() {
        candidates.push(dir.join(name));
    }
    candidates.into_iter().find(|candidate| works(candidate))
}

pub fn discover() -> Option<Tools> {
    let ffmpeg = find_binary("FFMPEG_PATH", "ffmpeg")?;
    let ffprobe = find_binary("FFPROBE_PATH", "ffprobe");
    crate::log::info(
        "main",
        &format!(
            "ffmpeg: {} (ffprobe: {})",
            ffmpeg.display(),
            ffprobe
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not found".into())
        ),
    );
    Some(Tools { ffmpeg, ffprobe })
}

/// ffmpeg arguments for a piped stream. `-ss` goes before `-i`.
pub fn ffmpeg_args(mode: Mode, file: &Path, seek: Option<f64>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(seconds) = seek {
        args.push("-ss".into());
        args.push(format!("{:.3}", seconds.max(0.0)).into());
        args.push("-noaccurate_seek".into());
    }
    args.push("-i".into());
    args.push(file.into());
    let tail: &[&str] = match mode {
        Mode::Remux => &[
            "-c",
            "copy",
            "-metadata:s:v",
            "rotate=0",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov",
            "-",
        ],
        Mode::Transcode => &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "28",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov",
            "-",
        ],
        Mode::Direct => &[],
    };
    args.extend(tail.iter().map(OsString::from));
    args
}

pub fn probe(ffprobe: &Path, file: &Path) -> Result<Value, String> {
    let mut child = Command::new(ffprobe)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    let mut stdout = child.stdout.take().ok_or("ffprobe stdout unavailable")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(buf) => {
            let status = child.wait().map_err(|err| err.to_string())?;
            if !status.success() {
                return Err(format!("ffprobe exited with {status}"));
            }
            serde_json::from_slice(&buf).map_err(|err| format!("bad ffprobe output: {err}"))
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("ffprobe timeout".to_string())
        }
    }
}

pub fn fmt_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".to_string();
    }
    let total = seconds.floor() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_fps(rate: &str) -> Option<String> {
    let (num, den) = rate.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 || num == 0.0 {
        return None;
    }
    let fps = num / den;
    let rounded = (fps * 100.0).round() / 100.0;
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        Some(format!("{}", rounded.round() as i64))
    } else {
        Some(format!("{rounded}"))
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
    })
}

fn stream_rotation(stream: &Value) -> Option<i64> {
    if let Some(rotate) = stream
        .get("tags")
        .and_then(|tags| tags.get("rotate"))
        .and_then(|v| v.as_str())
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        return Some(rotate.round() as i64);
    }
    stream
        .get("side_data_list")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter()
                .find_map(|item| number(item, "rotation"))
        })
        .map(|v| v.round() as i64)
}

/// Human-readable metadata block, one line per available field.
pub fn format_metadata(meta: &Value, path: &Path) -> String {
    let format = meta.get("format").cloned().unwrap_or(Value::Null);
    let streams = meta
        .get("streams")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| text(s, "codec_type") == Some("video"));
    let audio = streams
        .iter()
        .find(|s| text(s, "codec_type") == Some("audio"));

    let mut lines = Vec::new();
    lines.push(format!("File: {}", path.display()));

    if let Some(container) = text(&format, "format_long_name") {
        lines.push(format!("Container: {container}"));
    }
    if let Some(duration) = number(&format, "duration").or_else(|| {
        video
            .and_then(|s| number(s, "duration"))
            .or_else(|| audio.and_then(|s| number(s, "duration")))
    }) {
        lines.push(format!("Duration: {}", fmt_duration(duration)));
    }
    if let Some(size) = number(&format, "size").or_else(|| {
        std::fs::metadata(path).ok().map(|m| m.len() as f64)
    }) {
        lines.push(format!("Size: {}", human_size(size as u64)));
    }
    if let Some(bitrate) = number(&format, "bit_rate") {
        lines.push(format!("Bitrate: {} kbps", (bitrate / 1000.0).round() as u64));
    }
    if let Some(video) = video {
        let codec = text(video, "codec_long_name")
            .or_else(|| text(video, "codec"))
            .unwrap_or("unknown");
        let width = number(video, "width").unwrap_or(0.0) as u64;
        let height = number(video, "height").unwrap_or(0.0) as u64;
        let fps = text(video, "avg_frame_rate")
            .and_then(fmt_fps)
            .or_else(|| text(video, "r_frame_rate").and_then(fmt_fps))
            .unwrap_or_else(|| "0".to_string());
        let pix_fmt = text(video, "pix_fmt").unwrap_or("");
        let mut line = format!("Video: {codec} {width}x{height} {fps}");
        if !pix_fmt.is_empty() {
            line.push(' ');
            line.push_str(pix_fmt);
        }
        lines.push(line);
        if let Some(profile) = text(video, "profile") {
            lines.push(format!("Video Profile: {profile}"));
        }
        if let Some(rotation) = stream_rotation(video) {
            lines.push(format!("Rotation: {rotation}°"));
        }
    }
    if let Some(audio) = audio {
        let codec = text(audio, "codec_long_name")
            .or_else(|| text(audio, "codec"))
            .unwrap_or("unknown");
        let rate = number(audio, "sample_rate").unwrap_or(0.0) as u64;
        let channels = number(audio, "channels").unwrap_or(0.0) as u64;
        lines.push(format!("Audio: {codec} {rate}Hz {channels}ch"));
    }

    lines.join("\n")
}

/// Caps concurrent transcodes at two; remux is unlimited.
pub struct Slots {
    state: Mutex<usize>,
    cv: Condvar,
    max: usize,
}

impl Slots {
    pub fn new(max: usize) -> Self {
        Slots {
            state: Mutex::new(0),
            cv: Condvar::new(),
            max,
        }
    }

    pub fn acquire(self: &std::sync::Arc<Self>) -> SlotGuard {
        let mut active = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while *active >= self.max {
            active = self.cv.wait(active).unwrap_or_else(|e| e.into_inner());
        }
        *active += 1;
        SlotGuard {
            slots: std::sync::Arc::clone(self),
        }
    }
}

pub struct SlotGuard {
    slots: std::sync::Arc<Slots>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut active = self.slots.state.lock().unwrap_or_else(|e| e.into_inner());
        *active = active.saturating_sub(1);
        self.slots.cv.notify_one();
    }
}

pub fn mode_for(ext: &str, ffmpeg_available: bool) -> Mode {
    if !ffmpeg_available {
        Mode::Direct
    } else {
        Mode::from_ext(ext)
    }
}

pub fn ext_of(path: &Path) -> String {
    extension(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_mapping() {
        assert_eq!(Mode::from_ext("mkv"), Mode::Remux);
        assert_eq!(Mode::from_ext("avi"), Mode::Remux);
        assert_eq!(Mode::from_ext("mpg"), Mode::Transcode);
        assert_eq!(Mode::from_ext("mp4"), Mode::Direct);
        assert_eq!(mode_for("mkv", false), Mode::Direct);
        assert_eq!(mode_for("mkv", true), Mode::Remux);
    }

    #[test]
    fn fallback_extensions() {
        for ext in ["mov", "m4v", "mkv", "avi", "flv", "wmv", "mpg", "mpeg", "3gp", "ogv"] {
            assert!(is_fallback_ext(ext), "{ext}");
        }
        assert!(!is_fallback_ext("mp4"));
        assert!(!is_fallback_ext("webm"));
    }

    #[test]
    fn args_for_modes() {
        let args: Vec<String> = ffmpeg_args(Mode::Remux, Path::new("/a.mkv"), None)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "-i");
        assert_eq!(args[1], "/a.mkv");
        assert!(args.windows(2).any(|w| w == ["-c", "copy"]));
        assert!(args.windows(2).any(|w| w == ["-movflags", "frag_keyframe+empty_moov"]));

        let args: Vec<String> = ffmpeg_args(Mode::Transcode, Path::new("/a.mpg"), Some(12.7))
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(&args[..3], ["-ss", "12.700", "-noaccurate_seek"]);
        assert!(args.windows(2).any(|w| w == ["-preset", "ultrafast"]));
        assert!(args.windows(2).any(|w| w == ["-b:a", "128k"]));
    }

    #[test]
    fn durations_and_sizes() {
        assert_eq!(fmt_duration(0.0), "0:00");
        assert_eq!(fmt_duration(65.9), "1:05");
        assert_eq!(fmt_duration(3599.0), "59:59");
        assert_eq!(human_size(25_100_000), "23.9 MB");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(12), "12 B");
    }

    #[test]
    fn metadata_lines() {
        let meta = json!({
            "format": {
                "format_long_name": "QuickTime / MOV",
                "duration": "125.4",
                "size": "25100000",
                "bit_rate": "1601000"
            },
            "streams": [
                {
                    "codec_type": "video",
                    "codec_long_name": "H.264 / AVC",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "30000/1001",
                    "pix_fmt": "yuv420p",
                    "profile": "High",
                    "side_data_list": [{ "rotation": -90 }]
                },
                {
                    "codec_type": "audio",
                    "codec": "aac",
                    "sample_rate": "48000",
                    "channels": 2
                }
            ]
        });
        let text = format_metadata(&meta, Path::new("/v/a.mov"));
        assert!(text.starts_with("File: /v/a.mov"));
        assert!(text.contains("Container: QuickTime / MOV"));
        assert!(text.contains("Duration: 2:05"));
        assert!(text.contains("Size: 23.9 MB"));
        assert!(text.contains("Bitrate: 1601 kbps"));
        assert!(text.contains("Video: H.264 / AVC 1920x1080 29.97 yuv420p"));
        assert!(text.contains("Video Profile: High"));
        assert!(text.contains("Rotation: -90°"));
        assert!(text.contains("Audio: aac 48000Hz 2ch"));
        let order: Vec<&str> = text.lines().map(|l| l.split(':').next().unwrap()).collect();
        assert_eq!(
            order,
            vec!["File", "Container", "Duration", "Size", "Bitrate", "Video", "Video Profile", "Rotation", "Audio"]
        );
    }

    #[test]
    fn metadata_omits_missing() {
        let meta = json!({ "format": { "format_long_name": "Matroska" }, "streams": [] });
        let text = format_metadata(&meta, Path::new("/v/a.mkv"));
        assert_eq!(text, "File: /v/a.mkv\nContainer: Matroska");
    }

    #[test]
    fn transcode_slots_limit() {
        let slots = std::sync::Arc::new(Slots::new(2));
        let a = slots.acquire();
        let b = slots.acquire();
        let started = std::time::Instant::now();
        let slots2 = std::sync::Arc::clone(&slots);
        let waiter = std::thread::spawn(move || {
            let _c = slots2.acquire();
            std::time::Instant::now()
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(a);
        let unblocked = waiter.join().unwrap();
        assert!(unblocked.duration_since(started) >= Duration::from_millis(80));
        drop(b);
    }
}
