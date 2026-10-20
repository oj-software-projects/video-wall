#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

/// WebKitGTK's DMABUF renderer leaves a blank window on some drivers and in
/// virtual machines. Disable it unless the user explicitly picked a value.
#[cfg(target_os = "linux")]
fn linux_graphics_defaults() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_graphics_defaults() {}

mod cli;
mod config;
mod log;
mod media;
mod scan;
mod select;
mod server;

use std::sync::Arc;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

fn main() {
    linux_graphics_defaults();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match cli::parse(&args) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("Error: {err}");
            eprintln!("{}", cli::USAGE);
            std::process::exit(2);
        }
    };
    if cli.help {
        print!("{}", cli::help_text());
        return;
    }

    log::info("main", "starting video-wall 0.1.4");
    log::info("cli", &format!("args: {args:?}"));

    let tools = media::discover();
    let mut cfg = config::Config::load(&config::config_path());
    cli.apply(&mut cfg);
    let auto_start = cli.dirs.is_some();

    if cli.headless {
        let state = Arc::new(server::State::new(
            cfg,
            tools,
            server::Dialogs::headless(),
            auto_start,
        ));
        let app = server::start_app(state);
        println!("URL {}", app.url);
        log::info("main", &format!("listening on {}", app.url));
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let event_loop = EventLoopBuilder::<server::UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let dialogs = server::Dialogs::new(proxy);
    let state = Arc::new(server::State::new(cfg, tools, dialogs, auto_start));
    let app = server::start_app(state);
    log::info("main", &format!("listening on {}", app.url));

    // A normal window, not a near-fullscreen one: at most 1280x800 points,
    // shrunk on small displays. The monitor reports physical pixels, so the
    // size must be converted to logical points first.
    let (width, height) = event_loop
        .primary_monitor()
        .map(|monitor| {
            let logical = monitor.size().to_logical::<f64>(monitor.scale_factor());
            ((logical.width * 0.9).min(1280.0), (logical.height * 0.9).min(800.0))
        })
        .unwrap_or((1280.0, 800.0));

    let window = WindowBuilder::new()
        .with_title("video-wall - Multi-Video Player")
        .with_inner_size(tao::dpi::LogicalSize::new(width, height))
        .with_visible(true)
        .build(&event_loop)
        .expect("cannot create window");

    if let Some(monitor) = event_loop.primary_monitor() {
        let scale = monitor.scale_factor();
        let size = monitor.size().to_logical::<f64>(scale);
        let origin = monitor.position().to_logical::<f64>(scale);
        window.set_outer_position(tao::dpi::LogicalPosition::new(
            origin.x + (size.width - width) / 2.0,
            origin.y + (size.height - height) / 2.0,
        ));
    }

    let app_origin = app.url.trim_end_matches('/').to_string();
    let app_base = format!("{app_origin}/");
    let webview = WebViewBuilder::new()
        .with_url(app.url.as_str())
        // The renderer never navigates away; deny anything else and any
        // attempt to open a new window.
        .with_navigation_handler(move |url| url == app_origin || url.starts_with(&app_base))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .build(&window)
        .expect("cannot create webview");
    if cli.dev {
        webview.open_devtools();
    }
    window.set_focus();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(tao::event::StartCause::Init) => {
                window.set_focus();
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                log::info("main", "window closed");
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(server::UserEvent::PickDirs(reply)) => {
                let dirs = rfd::FileDialog::new()
                    .pick_folders()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|path| path.to_string_lossy().to_string())
                    .collect();
                let _ = reply.send(dirs);
            }
            Event::UserEvent(server::UserEvent::ConfirmDelete(path, reply)) => {
                let result = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Warning)
                    .set_title("Delete video")
                    .set_description(format!(
                        "Delete video from disk?\n\n{}\n\nThis cannot be undone.",
                        path.display()
                    ))
                    .set_buttons(rfd::MessageButtons::OkCancelCustom(
                        "Delete".to_string(),
                        "Cancel".to_string(),
                    ))
                    .show();
                let _ = reply.send(matches!(result, rfd::MessageDialogResult::Ok));
            }
            Event::UserEvent(server::UserEvent::DevTools) => {
                webview.open_devtools();
            }
            _ => {}
        }
    });
}
