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

    log::info("main", "starting video-wall 0.1.3");
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

    let (width, height) = event_loop
        .primary_monitor()
        .map(|monitor| {
            let size = monitor.size();
            (size.width as f64 * 0.9, size.height as f64 * 0.9)
        })
        .unwrap_or((1280.0, 800.0));

    let window = WindowBuilder::new()
        .with_title("video-wall - Multi-Video Player")
        .with_inner_size(tao::dpi::LogicalSize::new(width, height))
        .build(&event_loop)
        .expect("cannot create window");

    let webview = WebViewBuilder::new()
        .with_url(app.url.as_str())
        .build(&window)
        .expect("cannot create webview");
    if cli.dev {
        webview.open_devtools();
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
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
