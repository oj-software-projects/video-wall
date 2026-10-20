use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::config::{self, Config, Grid};
use crate::media::{self, Mode, Slots, Tools};
use crate::{log, scan, select};

pub const MEDIA_SERVERS: usize = 16;
const RANGE_CHUNK: u64 = 2 * 1024 * 1024;
const MAX_API_BODY: usize = 64 * 1024;

pub const INDEX_HTML: &str = include_str!("assets/index.html");
pub const APP_CSS: &str = include_str!("assets/app.css");
pub const APP_JS: &str = include_str!("assets/app.js");

pub type IdMap = Arc<Mutex<HashMap<String, PathBuf>>>;

// ---------------------------------------------------------------- media side

pub struct Media {
    pub ports: Vec<u16>,
    pub token: String,
    pub ids: IdMap,
}

impl Media {
    pub fn start(tools: Option<Tools>) -> Media {
        let token = new_token();
        let ids: IdMap = Arc::new(Mutex::new(HashMap::new()));
        let slots = Arc::new(Slots::new(2));
        let mut ports = Vec::new();

        for _ in 0..MEDIA_SERVERS {
            let server = match Server::http("127.0.0.1:0") {
                Ok(server) => server,
                Err(err) => {
                    log::error("main", &format!("cannot start media server: {err}"));
                    continue;
                }
            };
            let port = match server.server_addr().to_ip() {
                Some(addr) => addr.port(),
                None => continue,
            };
            ports.push(port);
            let ids = Arc::clone(&ids);
            let token = token.clone();
            let slots = Arc::clone(&slots);
            let tools = tools.clone();
            thread::spawn(move || serve_media(server, ids, token, slots, tools));
        }

        log::info(
            "main",
            &format!("media servers on {} ports", ports.len()),
        );
        Media { ports, token, ids }
    }

    pub fn url(&self, id: &str, index: usize, mode: Option<Mode>, seek: Option<u64>) -> String {
        let port = self.ports[index % self.ports.len()];
        let mut url = format!(
            "http://127.0.0.1:{port}/video?id={id}&token={}",
            self.token
        );
        if let Some(mode) = mode {
            url.push_str("&mode=");
            url.push_str(mode.as_str());
        }
        if let Some(seconds) = seek {
            url.push_str(&format!("&t={seconds}"));
        }
        url
    }
}

fn new_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0..16u8)))
        .collect()
}

fn serve_media(
    server: Server,
    ids: IdMap,
    token: String,
    slots: Arc<Slots>,
    tools: Option<Tools>,
) {
    for request in server.incoming_requests() {
        let ids = Arc::clone(&ids);
        let token = token.clone();
        let slots = Arc::clone(&slots);
        let tools = tools.clone();
        thread::spawn(move || {
            if let Err(err) = handle_media(request, &ids, &token, &slots, tools.as_ref()) {
                log::debug("main", &format!("media request failed: {err}"));
            }
        });
    }
}

fn handle_media(
    req: Request,
    ids: &IdMap,
    token: &str,
    slots: &Arc<Slots>,
    tools: Option<&Tools>,
) -> io::Result<()> {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    if path != "/video" {
        return respond_text(req, 404, "not found");
    }
    let query = parse_query(query);
    let authorized = query
        .get("token")
        .map(|presented| ct_eq(presented, token))
        .unwrap_or(false);
    if !authorized {
        return respond_text(req, 403, "forbidden");
    }
    let Some(id) = query.get("id") else {
        return respond_text(req, 404, "not found");
    };
    let file = {
        let ids = ids.lock().unwrap_or_else(|e| e.into_inner());
        match ids.get(id) {
            Some(file) => file.clone(),
            None => return respond_text(req, 404, "not found"),
        }
    };

    let ext = media::ext_of(&file);
    let mode = match tools {
        None => Mode::Direct,
        Some(_) => query
            .get("mode")
            .and_then(|value| Mode::parse(value))
            .unwrap_or_else(|| Mode::from_ext(&ext)),
    };

    match mode {
        Mode::Direct => serve_direct(req, &file, &ext),
        _ => match tools {
            Some(tools) => {
                let seek = query.get("t").and_then(|t| t.parse::<f64>().ok());
                serve_ffmpeg(req, &file, mode, seek, slots, tools)
            }
            None => serve_direct(req, &file, &ext),
        },
    }
}

enum RangeSpec {
    None,
    Invalid,
    Valid(u64, u64),
}

fn parse_range(header: Option<&str>, size: u64) -> RangeSpec {
    let Some(value) = header else {
        return RangeSpec::None;
    };
    let Some(rest) = value.trim().strip_prefix("bytes=") else {
        return RangeSpec::Invalid;
    };
    let Some((start, end)) = rest.split_once('-') else {
        return RangeSpec::Invalid;
    };
    let Ok(start) = start.trim().parse::<i64>() else {
        return RangeSpec::Invalid;
    };
    if start < 0 || size == 0 || start as u64 >= size {
        return RangeSpec::Invalid;
    }
    let start = start as u64;
    let end = if end.trim().is_empty() {
        size - 1
    } else {
        match end.trim().parse::<u64>() {
            Ok(end) if end >= start => end.min(size - 1),
            _ => return RangeSpec::Invalid,
        }
    };
    RangeSpec::Valid(start, (start + RANGE_CHUNK - 1).min(end))
}

fn serve_direct(req: Request, file: &Path, ext: &str) -> io::Result<()> {
    let mut file = File::open(file)?;
    let size = file.metadata()?.len();
    let ctype = content_type(ext);
    let range_header = req
        .headers()
        .iter()
        .find(|header| header.field.equiv("Range"))
        .map(|header| header.value.as_str().to_string());

    match parse_range(range_header.as_deref(), size) {
        RangeSpec::Invalid => {
            let mut headers = base_headers(ctype);
            headers.push(h("Content-Range", &format!("bytes */{size}")));
            req.respond(
                Response::new(StatusCode(416), headers, io::empty(), Some(0), None)
                    .with_chunked_threshold(usize::MAX),
            )
        }
        RangeSpec::Valid(start, end) => {
            let len = end - start + 1;
            file.seek(SeekFrom::Start(start))?;
            let mut headers = base_headers(ctype);
            headers.push(h(
                "Content-Range",
                &format!("bytes {start}-{end}/{size}"),
            ));
            req.respond(
                Response::new(StatusCode(206), headers, file.take(len), Some(len as usize), None)
                    .with_chunked_threshold(usize::MAX),
            )
        }
        RangeSpec::None => req.respond(
            Response::new(
                StatusCode(200),
                base_headers(ctype),
                file,
                Some(size as usize),
                None,
            )
            .with_chunked_threshold(usize::MAX),
        ),
    }
}

fn serve_ffmpeg(
    req: Request,
    file: &Path,
    mode: Mode,
    seek: Option<f64>,
    slots: &Arc<Slots>,
    tools: &Tools,
) -> io::Result<()> {
    let _slot = if mode == Mode::Transcode {
        Some(slots.acquire())
    } else {
        None
    };

    let mut child = Command::new(&tools.ffmpeg)
        .args(media::ffmpeg_args(mode, file, seek))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "ffmpeg stdout unavailable"))?;
    let reader = ChildReader {
        child: Some(child),
        out: stdout,
    };
    req.respond(Response::new(
        StatusCode(200),
        base_headers("video/mp4"),
        reader,
        None,
        None,
    ))
}

struct ChildReader {
    child: Option<Child>,
    out: ChildStdout,
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.out.read(buf)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn content_type(ext: &str) -> &'static str {
    match ext {
        "mp4" | "mov" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" => "video/3gpp",
        "ogv" => "video/ogg",
        _ => "application/octet-stream",
    }
}

fn base_headers(ctype: &str) -> Vec<Header> {
    vec![
        h("Content-Type", ctype),
        h("Cache-Control", "no-cache"),
        h("Accept-Ranges", "bytes"),
        h("Access-Control-Allow-Origin", "*"),
        h("Vary", "Origin"),
        h("Access-Control-Allow-Headers", "Range"),
        h("Access-Control-Expose-Headers", "Content-Length, Content-Range"),
        h("X-Content-Type-Options", "nosniff"),
    ]
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Constant-time compare for secrets. Length is not secret (fixed-size tokens).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn respond_text(req: Request, status: u16, body: &str) -> io::Result<()> {
    req.respond(
        Response::from_string(body)
            .with_status_code(status)
            .with_header(h("Content-Type", "text/plain; charset=utf-8")),
    )
}

// ------------------------------------------------------------------ app side

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub fallback_url: Option<String>,
    pub path: String,
    pub size: u64,
    pub ext: String,
    pub mode: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Wall {
    pub grid: Grid,
    pub videos: Vec<Entry>,
    pub pool: Vec<Entry>,
    #[serde(rename = "loop")]
    pub loop_: bool,
    pub mute: bool,
    pub ffmpeg_available: bool,
}

pub enum UserEvent {
    PickDirs(std::sync::mpsc::Sender<Vec<String>>),
    ConfirmDelete(PathBuf, std::sync::mpsc::Sender<bool>),
    DevTools,
}

#[derive(Clone)]
pub struct Dialogs {
    proxy: Option<tao::event_loop::EventLoopProxy<UserEvent>>,
}

impl Dialogs {
    pub fn headless() -> Self {
        Dialogs { proxy: None }
    }

    pub fn new(proxy: tao::event_loop::EventLoopProxy<UserEvent>) -> Self {
        Dialogs { proxy: Some(proxy) }
    }

    pub fn pick_dirs(&self) -> Vec<String> {
        let Some(proxy) = &self.proxy else {
            return Vec::new();
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if proxy.send_event(UserEvent::PickDirs(tx)).is_err() {
            return Vec::new();
        }
        rx.recv_timeout(std::time::Duration::from_secs(300))
            .unwrap_or_default()
    }

    pub fn confirm_delete(&self, path: &Path) -> bool {
        let Some(proxy) = &self.proxy else {
            return false;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if proxy
            .send_event(UserEvent::ConfirmDelete(path.to_path_buf(), tx))
            .is_err()
        {
            return false;
        }
        rx.recv_timeout(std::time::Duration::from_secs(300))
            .unwrap_or(false)
    }

    pub fn devtools(&self) {
        if let Some(proxy) = &self.proxy {
            let _ = proxy.send_event(UserEvent::DevTools);
        }
    }
}

pub struct State {
    pub app_token: String,
    pub config: Mutex<Config>,
    pub wall: Mutex<Option<Wall>>,
    pub media: Media,
    pub tools: Option<Tools>,
    pub dialogs: Dialogs,
    pub auto_start: AtomicBool,
}

impl State {
    pub fn new(config: Config, tools: Option<Tools>, dialogs: Dialogs, auto_start: bool) -> State {
        let media = Media::start(tools.clone());
        State {
            app_token: new_token(),
            config: Mutex::new(config),
            wall: Mutex::new(None),
            media,
            tools,
            dialogs,
            auto_start: AtomicBool::new(auto_start),
        }
    }
}

pub struct App {
    pub url: String,
}

pub fn start_app(state: Arc<State>) -> App {
    let server = Server::http("127.0.0.1:0").expect("cannot start app server");
    let port = server
        .server_addr()
        .to_ip()
        .expect("app server has a TCP address")
        .port();
    let url = format!("http://127.0.0.1:{port}/?token={}", state.app_token);
    let shared = Arc::clone(&state);
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let state = Arc::clone(&shared);
            thread::spawn(move || {
                if let Err(err) = handle_app(request, &state) {
                    log::debug("main", &format!("app request failed: {err}"));
                }
            });
        }
    });
    App { url }
}

fn handle_app(mut req: Request, state: &State) -> io::Result<()> {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();

    // Only the renderer that loaded `/` knows the run token; rejects other
    // senders, mirroring the Electron IPC sender check.
    if path.starts_with("/api/") {
        let presented = req
            .headers()
            .iter()
            .find(|header| header.field.equiv("x-tmv-token"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        if !ct_eq(&presented, &state.app_token) {
            return respond_text(req, 403, "forbidden");
        }
    }

    match path.as_str() {
        "/" | "/index.html" => {
            // The page carries the run token; do not hand it to anyone who
            // cannot already prove they know it.
            let authorized = url
                .split_once('?')
                .map(|(_, query)| parse_query(query))
                .and_then(|query| query.get("token").cloned())
                .map(|presented| ct_eq(&presented, &state.app_token))
                .unwrap_or(false);
            if !authorized {
                return respond_text(req, 403, "forbidden");
            }
            let html = INDEX_HTML.replace("__TMV_TOKEN__", &state.app_token);
            req.respond(
                Response::from_string(html)
                    .with_header(h("Content-Type", "text/html; charset=utf-8"))
                    .with_header(h("X-Content-Type-Options", "nosniff")),
            )
        }
        "/app.css" => {
            req.respond(
                Response::from_string(APP_CSS)
                    .with_header(h("Content-Type", "text/css; charset=utf-8"))
                    .with_header(h("X-Content-Type-Options", "nosniff")),
            )
        }
        "/app.js" => {
            req.respond(
                Response::from_string(APP_JS)
                    .with_header(h("Content-Type", "text/javascript; charset=utf-8"))
                    .with_header(h("X-Content-Type-Options", "nosniff")),
            )
        }
        "/api/lobby" => json_response(req, lobby(state)),
        "/api/start-wall" => {
            let body = read_json(&mut req);
            json_response(req, start_wall(state, &body))
        }
        "/api/scan" => {
            let body = read_json(&mut req);
            json_response(req, scan_dirs(&body))
        }
        "/api/pick-directories" => json_response(
            req,
            json!({ "dirs": state.dialogs.pick_dirs() }),
        ),
        "/api/meta" => {
            let body = read_json(&mut req);
            json_response(req, video_meta(state, &body))
        }
        "/api/delete" => {
            let body = read_json(&mut req);
            json_response(req, delete_video(state, &body))
        }
        "/api/reshuffle" => json_response(req, reshuffle(state)),
        "/api/log" => {
            let body = read_json(&mut req);
            let level = body
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("INFO")
                .to_ascii_uppercase();
            let message = body
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .replace(['\n', '\r'], " ");
            log::write(&level, "renderer", &message, body.get("meta").cloned());
            json_response(req, json!({ "ok": true }))
        }
        "/api/quit" => {
            let _ = json_response(req, json!({ "ok": true }));
            log::info("main", "quit requested");
            std::process::exit(0);
        }
        "/api/devtools" => {
            state.dialogs.devtools();
            json_response(req, json!({ "ok": true }))
        }
        _ => respond_text(req, 404, "not found"),
    }
}

fn json_response(req: Request, value: Value) -> io::Result<()> {
    req.respond(
        Response::from_string(value.to_string())
            .with_header(h("Content-Type", "application/json; charset=utf-8"))
            .with_header(h("X-Content-Type-Options", "nosniff")),
    )
}

fn read_json(req: &mut Request) -> Value {
    let mut body = String::new();
    // API payloads are a few hundred bytes; cap the read so a local client
    // cannot exhaust memory with a huge body.
    let _ = req
        .as_reader()
        .take(MAX_API_BODY as u64)
        .read_to_string(&mut body);
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

fn lobby(state: &State) -> Value {
    let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let auto_start = state.auto_start.swap(false, Ordering::SeqCst) && !cfg.dirs.is_empty();
    json!({
        "dirs": cfg.dirs,
        "grid": { "rows": cfg.grid.rows, "cols": cfg.grid.cols },
        "shuffle": cfg.shuffle,
        "loop": cfg.loop_,
        "mute": cfg.mute,
        "ffmpegAvailable": state.tools.is_some(),
        "autoStart": auto_start,
    })
}

fn start_wall(state: &State, body: &Value) -> Value {
    let dirs: Vec<String> = body
        .get("dirs")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let dirs = config::sanitize_dirs(dirs);
    if dirs.is_empty() {
        return json!({ "ok": false, "error": "Add at least one folder" });
    }

    let mut cfg = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    cfg.dirs = dirs;
    if let Some(grid) = body.get("grid") {
        cfg.grid = Config::from_json(&json!({ "grid": grid })).grid;
    }
    if let Some(value) = body.get("shuffle").and_then(|v| v.as_bool()) {
        cfg.shuffle = value;
    }
    if let Some(value) = body.get("loop").and_then(|v| v.as_bool()) {
        cfg.loop_ = value;
    }
    if let Some(value) = body.get("mute").and_then(|v| v.as_bool()) {
        cfg.mute = value;
    }

    match build_wall(state, &cfg) {
        Ok(wall) => {
            if let Err(err) = cfg.save(&config::config_path()) {
                log::warn("config", &format!("cannot save config: {err}"));
            }
            *state.config.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
            *state.wall.lock().unwrap_or_else(|e| e.into_inner()) = Some(wall.clone());
            log::info("main", &format!("wall started: {} videos", wall.videos.len()));
            json!({ "ok": true, "wall": wall })
        }
        Err(err) => {
            log::warn("main", &format!("wall start failed: {err}"));
            json!({ "ok": false, "error": err })
        }
    }
}

fn build_wall(state: &State, cfg: &Config) -> Result<Wall, String> {
    let paths = scan::scan(&cfg.dirs);
    if paths.is_empty() {
        return Err("No video files found in the selected folders".to_string());
    }
    let selected = select::select(&paths, cfg.grid.cells(), cfg.shuffle);

    let mut ids = state.media.ids.lock().unwrap_or_else(|e| e.into_inner());
    ids.clear();

    let mut pool = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let id = format!("p{index}");
        ids.insert(id.clone(), path.clone());
        pool.push(entry(state, &id, index, path));
    }
    let mut videos = Vec::with_capacity(selected.len());
    for (index, path) in selected.iter().enumerate() {
        let id = format!("v{index}");
        ids.insert(id.clone(), path.clone());
        videos.push(entry(state, &id, index, path));
    }
    drop(ids);

    Ok(Wall {
        grid: cfg.grid,
        videos,
        pool,
        loop_: cfg.loop_,
        mute: cfg.mute,
        ffmpeg_available: state.tools.is_some(),
    })
}

fn entry(state: &State, id: &str, index: usize, path: &Path) -> Entry {
    let ext = media::ext_of(path);
    let mode = media::mode_for(&ext, state.tools.is_some());
    let url = state.media.url(
        id,
        index,
        if mode == Mode::Direct {
            None
        } else {
            Some(mode)
        },
        None,
    );
    let fallback_url = if state.tools.is_some()
        && mode != Mode::Transcode
        && media::is_fallback_ext(&ext)
    {
        Some(state.media.url(id, index, Some(Mode::Transcode), None))
    } else {
        None
    };
    Entry {
        id: id.to_string(),
        url,
        fallback_url,
        path: path.display().to_string(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ext,
        mode: mode.as_str().to_string(),
    }
}

fn scan_dirs(body: &Value) -> Value {
    let dirs: Vec<String> = body
        .get("dirs")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let dirs = config::sanitize_dirs(dirs);
    let counts: Vec<Value> = dirs
        .iter()
        .take(50)
        .map(|dir| match scan::count(dir) {
            Ok(count) => json!({ "dir": dir, "count": count }),
            Err(error) => json!({ "dir": dir, "count": Value::Null, "error": error }),
        })
        .collect();
    json!({ "counts": counts })
}

fn video_meta(state: &State, body: &Value) -> Value {
    let Some(id) = body.get("id").and_then(|v| v.as_str()) else {
        return json!({ "error": "unknown video" });
    };
    let path = {
        let ids = state.media.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.get(id).cloned()
    };
    let Some(path) = path else {
        return json!({ "error": "unknown video" });
    };
    let Some(ffprobe) = state.tools.as_ref().and_then(|tools| tools.ffprobe.as_ref()) else {
        return json!({ "error": "ffprobe not available" });
    };
    match media::probe(ffprobe, &path) {
        Ok(meta) => json!({ "text": media::format_metadata(&meta, &path) }),
        Err(err) => json!({ "error": err }),
    }
}

fn delete_video(state: &State, body: &Value) -> Value {
    let Some(id) = body.get("id").and_then(|v| v.as_str()) else {
        return json!({ "error": "unknown video" });
    };
    let path = {
        let ids = state.media.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.get(id).cloned()
    };
    let Some(path) = path else {
        return json!({ "error": "unknown video" });
    };
    if !state.dialogs.confirm_delete(&path) {
        return json!({ "cancelled": true });
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            {
                let mut ids = state.media.ids.lock().unwrap_or_else(|e| e.into_inner());
                ids.retain(|_, mapped| mapped != &path);
            }
            if let Some(wall) = state.wall.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                wall.videos.retain(|entry| entry.path != path.display().to_string());
                wall.pool.retain(|entry| entry.path != path.display().to_string());
            }
            log::info("main", &format!("deleted {}", path.display()));
            json!({ "deleted": true, "path": path.display().to_string() })
        }
        Err(err) => json!({ "error": err.to_string() }),
    }
}

fn reshuffle(state: &State) -> Value {
    let Some(wall) = state.wall.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return json!({ "error": "No wall is running" });
    };
    let pool_paths: Vec<PathBuf> = wall.pool.iter().map(|e| PathBuf::from(&e.path)).collect();
    let selected = select::select(&pool_paths, wall.grid.cells(), true);

    {
        let mut ids = state.media.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.retain(|key, _| !key.starts_with('v'));
        for (index, path) in selected.iter().enumerate() {
            ids.insert(format!("v{index}"), path.clone());
        }
    }

    let videos: Vec<Entry> = selected
        .iter()
        .enumerate()
        .map(|(index, path)| entry(state, &format!("v{index}"), index, path))
        .collect();

    if let Some(current) = state.wall.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        current.videos = videos.clone();
    }

    json!({
        "videos": videos,
        "pool": wall.pool,
        "loop": wall.loop_,
        "mute": wall.mute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert!(matches!(parse_range(None, 100), RangeSpec::None));
        assert!(matches!(parse_range(Some("bytes=0-9"), 100), RangeSpec::Valid(0, 9)));
        assert!(matches!(parse_range(Some("bytes=90-"), 100), RangeSpec::Valid(90, 99)));
        assert!(matches!(parse_range(Some("bytes=90-500"), 100), RangeSpec::Valid(90, 99)));
        assert!(matches!(parse_range(Some("bytes=-5"), 100), RangeSpec::Invalid));
        assert!(matches!(parse_range(Some("bytes=100-200"), 100), RangeSpec::Invalid));
        assert!(matches!(parse_range(Some("bytes=abc-1"), 100), RangeSpec::Invalid));
        assert!(matches!(parse_range(Some("bytes=5-1"), 100), RangeSpec::Invalid));
    }

    #[test]
    fn range_caps_at_two_mib() {
        let size = 10 * 1024 * 1024;
        match parse_range(Some("bytes=0-"), size) {
            RangeSpec::Valid(start, end) => {
                assert_eq!(start, 0);
                assert_eq!(end, RANGE_CHUNK - 1);
            }
            _ => panic!("expected valid range"),
        }
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("mkv"), "video/x-matroska");
        assert_eq!(content_type("mp4"), "video/mp4");
        assert_eq!(content_type("xyz"), "application/octet-stream");
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn token_is_128_bits_hex() {
        let token = new_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
