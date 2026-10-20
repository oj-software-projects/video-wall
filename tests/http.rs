use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct App {
    child: Child,
    port: u16,
    token: String,
}

impl App {
    fn spawn(home: &Path, args: &[&str]) -> App {
        let mut child = Command::new(env!("CARGO_BIN_EXE_video-wall"))
            .arg("--headless")
            .args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("APPDATA", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn app");
        let stdout = child.stdout.take().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let mut url = String::new();
        for _ in 0..50 {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read url") == 0 {
                panic!("app exited before printing its URL");
            }
            if let Some(rest) = line.strip_prefix("URL ") {
                url = rest.trim().to_string();
                break;
            }
        }
        assert!(!url.is_empty(), "no URL printed");
        let port = url
            .trim_end_matches('/')
            .rsplit(':')
            .next()
            .expect("port")
            .parse()
            .expect("numeric port");

        // The run token is embedded in the served page; every API call needs it.
        let page = http(port, "GET", "/", None, None);
        assert_eq!(page.status, 200);
        let html = String::from_utf8_lossy(&page.body).to_string();
        let marker = "<meta name=\"tmv-token\" content=\"";
        let start = html.find(marker).expect("token meta") + marker.len();
        let token = html[start..]
            .split('"')
            .next()
            .expect("token value")
            .to_string();
        App {
            child,
            port,
            token,
        }
    }

    fn get(&self, path: &str) -> HttpResponse {
        http(self.port, "GET", path, None, Some(&self.token))
    }

    fn post(&self, path: &str, body: &str) -> HttpResponse {
        http(
            self.port,
            "POST",
            path,
            Some(body.as_bytes()),
            Some(&self.token),
        )
    }

    fn json(&self, path: &str, body: &str) -> Value {
        let response = self.post(path, body);
        assert_eq!(response.status, 200, "{path}");
        serde_json::from_slice(&response.body).expect("json body")
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    app_token: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let body = body.unwrap_or(b"");
    let auth = match app_token {
        Some(token) => format!("X-Tmv-Token: {token}\r\n"),
        None => String::new(),
    };
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(body).expect("write body");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator");
    let head_text = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head_text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status code");

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let mut body = raw[split + 4..].to_vec();
    if headers
        .get("transfer-encoding")
        .map(|value| value.contains("chunked"))
        .unwrap_or(false)
    {
        body = dechunk(&body);
    }
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(pos) = data.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_text = String::from_utf8_lossy(&data[..pos]).to_string();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("0").trim(), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = pos + 2;
        if data.len() < start + size {
            out.extend_from_slice(&data[start..]);
            break;
        }
        out.extend_from_slice(&data[start..start + size]);
        data = &data[start + size + 2..];
    }
    out
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("tmv-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn make_video(path: &Path, codec: &str) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x480:rate=30", "-t", "3"])
        .args(codec.split(' '))
        .args(["-y"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {}", path.display());
    assert!(
        std::fs::metadata(path).unwrap().len() >= 1_048_576,
        "fixture {} too small",
        path.display()
    );
}

fn probe_streams(path: &Path) -> Value {
    let output = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_streams"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    serde_json::from_slice(&output.stdout).expect("ffprobe json")
}

fn media_port(url: &str) -> u16 {
    url.split('/').nth(2).unwrap().rsplit(':').next().unwrap().parse().unwrap()
}

#[test]
fn end_to_end_wall_media_and_config() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let root = temp_root("e2e");
    let videos = root.join("videos");
    std::fs::create_dir_all(&videos).unwrap();

    let base_mp4 = root.join("base.mp4");
    let base_mkv = root.join("base.mkv");
    let base_mpg = root.join("base.mpg");
    make_video(&base_mp4, "-c:v libx264 -preset ultrafast -crf 0 -pix_fmt yuv420p");
    make_video(&base_mkv, "-c:v libx264 -preset ultrafast -crf 0 -pix_fmt yuv420p");
    make_video(&base_mpg, "-c:v mpeg2video -b:v 4M");

    for index in 0..16 {
        std::fs::copy(&base_mp4, videos.join(format!("clip{index:02}.mp4"))).unwrap();
    }
    std::fs::copy(&base_mkv, videos.join("clip.mkv")).unwrap();
    std::fs::copy(&base_mpg, videos.join("clip.mpg")).unwrap();
    std::fs::write(videos.join("notes.txt"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    std::fs::write(videos.join("._fork.mp4"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dir = videos.to_string_lossy().to_string();

    let app = App::spawn(&home, &[]);

    // Lobby defaults, no scanning at launch.
    let lobby: Value = serde_json::from_slice(&app.get("/api/lobby").body).unwrap();
    assert_eq!(lobby["dirs"], serde_json::json!([]));
    assert_eq!(lobby["grid"]["rows"], 2);
    assert_eq!(lobby["grid"]["cols"], 2);
    assert_eq!(lobby["shuffle"], true);
    assert_eq!(lobby["loop"], false);
    assert_eq!(lobby["mute"], true);
    assert_eq!(lobby["ffmpegAvailable"], true);
    assert_eq!(lobby["autoStart"], false);

    // API calls without the run token are rejected.
    let unauthenticated = http(
        app.port,
        "POST",
        "/api/scan",
        Some(br#"{"dirs":[]}"#),
        None,
    );
    assert_eq!(unauthenticated.status, 403);

    // Per-folder counts, including a bogus path.
    let scan = app.json("/api/scan", &format!(r#"{{"dirs":["{dir}","/definitely/not/here"]}}"#));
    assert_eq!(scan["counts"][0]["count"], 18);
    assert_eq!(scan["counts"][1]["error"], "not found");

    // Start a 4x4 wall: 18 pool files, 16 distinct selected.
    let started = app.json(
        "/api/start-wall",
        &format!(r#"{{"dirs":["{dir}"],"grid":{{"rows":4,"cols":4}},"shuffle":true,"loop":false,"mute":true}}"#),
    );
    assert_eq!(started["ok"], true, "{started}");
    let wall = &started["wall"];
    assert_eq!(wall["grid"]["rows"], 4);
    assert_eq!(wall["grid"]["cols"], 4);
    assert_eq!(wall["pool"].as_array().unwrap().len(), 18);
    let selected = wall["videos"].as_array().unwrap();
    assert_eq!(selected.len(), 16);
    let unique: HashSet<&str> = selected
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(unique.len(), 16, "duplicate videos in wall");
    assert_eq!(wall["loop"], false);
    assert_eq!(wall["mute"], true);
    assert_eq!(wall["ffmpegAvailable"], true);

    // Config is written only after a successful start.
    let config_path = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/video-wall/config.json")
    } else if cfg!(target_os = "windows") {
        home.join("video-wall/config.json")
    } else {
        home.join(".config/video-wall/config.json")
    };
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(config["grid"]["rows"], 4);
    assert_eq!(config["grid"]["cols"], 4);
    assert_eq!(config["dirs"][0], dir);

    // Direct media: full, ranged, invalid range, bad token, unknown id.
    let direct = selected
        .iter()
        .find(|entry| entry["ext"] == "mp4")
        .unwrap();
    let url = direct["url"].as_str().unwrap();
    let port = media_port(url);
    let path = url.splitn(4, '/').nth(3).map(|rest| format!("/{rest}")).unwrap();

    let full = http(port, "GET", &path, None, None);
    assert_eq!(full.status, 200);
    assert_eq!(full.headers["content-type"], "video/mp4");
    assert_eq!(full.headers["accept-ranges"], "bytes");
    assert_eq!(full.headers["access-control-allow-origin"], "*");
    assert!(full.body.len() >= 1_048_576);
    assert_eq!(
        full.headers["content-length"].parse::<usize>().unwrap(),
        full.body.len(),
        "direct responses must carry Content-Length"
    );

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=0-1023\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw[..raw.len().min(400)]).to_ascii_lowercase();
    assert!(text.starts_with("http/1.1 206"), "{text}");
    assert!(text.contains("content-range: bytes 0-1023/"), "{text}");
    assert!(text.contains("content-length: 1024"), "{text}");

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=999999999-\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw[..raw.len().min(400)]).to_ascii_lowercase();
    assert!(text.starts_with("http/1.1 416"), "{text}");

    let no_token = url.split("token=").next().unwrap().to_string();
    let no_token_path = no_token.splitn(4, '/').nth(3).map(|rest| format!("/{rest}")).unwrap();
    assert_eq!(http(port, "GET", &no_token_path, None, None).status, 403);
    assert_eq!(
        http(port, "GET", &format!("/video?id=nope&token={}", token_of(url)), None, None).status,
        404
    );

    // Remux (.mkv) and transcode (.mpg) produce playable MP4.
    let pool = wall["pool"].as_array().unwrap();
    for (ext, expect_mode) in [("mkv", "remux"), ("mpg", "transcode")] {
        let entry = pool.iter().find(|entry| entry["ext"] == ext).unwrap();
        assert_eq!(entry["mode"], expect_mode);
        let url = entry["url"].as_str().unwrap();
        assert!(url.contains(&format!("mode={expect_mode}")), "{url}");
        let port = media_port(url);
        let path = url.splitn(4, '/').nth(3).map(|rest| format!("/{rest}")).unwrap();
        let response = http(port, "GET", &path, None, None);
        assert_eq!(response.status, 200, "{ext}: {:?}", response.status);
        assert_eq!(response.headers["content-type"], "video/mp4");
        assert!(!response.body.is_empty());
        let output = root.join(format!("out.{ext}.mp4"));
        std::fs::write(&output, &response.body).unwrap();
        let streams = probe_streams(&output);
        let video = streams["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .unwrap_or_else(|| panic!("no video stream in {ext} output"));
        assert_eq!(video["codec_name"], "h264");
    }

    // Seek on a piped stream restarts ffmpeg at the timestamp.
    let mkv = pool.iter().find(|entry| entry["ext"] == "mkv").unwrap();
    let url = format!("{}&t=1", mkv["url"].as_str().unwrap());
    let port = media_port(&url);
    let path = url.splitn(4, '/').nth(3).map(|rest| format!("/{rest}")).unwrap();
    let seeked = http(port, "GET", &path, None, None);
    assert_eq!(seeked.status, 200);
    assert!(!seeked.body.is_empty());

    // 16 simultaneous streams.
    let handles: Vec<_> = selected
        .iter()
        .map(|entry| {
            let url = entry["url"].as_str().unwrap().to_string();
            std::thread::spawn(move || {
                let port = media_port(&url);
                let path = url.splitn(4, '/').nth(3).map(|rest| format!("/{rest}")).unwrap();
                let response = http(port, "GET", &path, None, None);
                (response.status, response.body.len())
            })
        })
        .collect();
    for handle in handles {
        let (status, len) = handle.join().unwrap();
        assert_eq!(status, 200);
        assert!(len > 0);
    }

    // Reshuffle keeps uniqueness and the 16-cell cap.
    let reshuffled = app.json("/api/reshuffle", "{}");
    let videos = reshuffled["videos"].as_array().unwrap();
    assert_eq!(videos.len(), 16);
    let unique: HashSet<&str> = videos.iter().map(|entry| entry["path"].as_str().unwrap()).collect();
    assert_eq!(unique.len(), 16);

    // Metadata endpoint returns the formatted block.
    let meta = app.json("/api/meta", &format!(r#"{{"id":"{}"}}"#, direct["id"].as_str().unwrap()));
    let text = meta["text"].as_str().expect("metadata text");
    assert!(text.contains("File: "));
    assert!(text.contains("Container: "));
    assert!(text.contains("Video: "));

    // Persistence across restarts.
    app.stop();
    let app = App::spawn(&home, &[]);
    let lobby: Value = serde_json::from_slice(&app.get("/api/lobby").body).unwrap();
    assert_eq!(lobby["grid"]["rows"], 4);
    assert_eq!(lobby["grid"]["cols"], 4);
    assert_eq!(lobby["dirs"][0], dir);
    assert_eq!(lobby["autoStart"], false);
    app.stop();

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_dirs_skip_lobby_and_override_saved_config() {
    let root = temp_root("cli");
    let videos = root.join("videos");
    std::fs::create_dir_all(&videos).unwrap();
    for index in 0..4 {
        std::fs::write(videos.join(format!("v{index}.mp4")), vec![0u8; 1_048_576]).unwrap();
    }
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dir = videos.to_string_lossy().to_string();

    // Pre-seed saved settings that the CLI must override.
    let config_dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/video-wall")
    } else if cfg!(target_os = "windows") {
        home.join("video-wall")
    } else {
        home.join(".config/video-wall")
    };
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        r#"{"dirs":["/saved"],"grid":{"rows":3,"cols":3},"shuffle":false,"loop":true,"mute":false}"#,
    )
    .unwrap();

    let app = App::spawn(
        &home,
        &["--dirs", &dir, "--grid", "2x2", "--no-mute", "--shuffle"],
    );
    let lobby: Value = serde_json::from_slice(&app.get("/api/lobby").body).unwrap();
    assert_eq!(lobby["autoStart"], true);
    assert_eq!(lobby["dirs"][0], dir);
    assert_eq!(lobby["grid"]["rows"], 2);
    assert_eq!(lobby["grid"]["cols"], 2);
    assert_eq!(lobby["shuffle"], true);
    assert_eq!(lobby["mute"], false);
    assert_eq!(lobby["loop"], true);

    let started = app.json(
        "/api/start-wall",
        &format!(r#"{{"dirs":["{dir}"],"grid":{{"rows":2,"cols":2}},"shuffle":true,"loop":true,"mute":false}}"#),
    );
    assert_eq!(started["ok"], true);
    assert_eq!(started["wall"]["videos"].as_array().unwrap().len(), 4);

    // Failed starts do not clobber saved settings.
    let before = std::fs::read_to_string(config_dir.join("config.json")).unwrap();
    let failed = app.json(
        "/api/start-wall",
        r#"{"dirs":["/definitely/not/here"],"grid":{"rows":8,"cols":8}}"#,
    );
    assert_eq!(failed["ok"], false);
    assert!(failed["error"].as_str().unwrap().contains("No video files"));
    let after = std::fs::read_to_string(config_dir.join("config.json")).unwrap();
    assert_eq!(before, after);

    // Missing folder count.
    let scan = app.json("/api/scan", r#"{"dirs":["/definitely/not/here"]}"#);
    assert_eq!(scan["counts"][0]["error"], "not found");

    app.stop();
    let _ = std::fs::remove_dir_all(&root);
}

fn token_of(url: &str) -> &str {
    url.split("token=").nth(1).unwrap().split('&').next().unwrap()
}
