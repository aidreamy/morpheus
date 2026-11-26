mod app;
mod component;
mod left_panel;
mod routes;
mod theme;

use gpui::{Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};

fn main() {
    Application::new().run(|cx| {
        app::bind_keys(cx);
        cx.on_window_closed(|cx| {
            // GPUI's X11 backend still holds its platform state here.
            cx.spawn(async |cx| {
                cx.update(|cx| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
            })
            .detach();
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1080.), px(720.)), cx);
        let result = cx.open_window(
            WindowOptions {
                app_id: Some("morpheus-gui".into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(360.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| app::Desktop::new(window, cx)),
        );
        if let Err(error) = result {
            eprintln!("morpheus-gui: {error}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}
