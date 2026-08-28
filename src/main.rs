mod heatmap;
mod models;
mod persistence;
mod services;
mod utils;
mod widgets;
mod window;

use libadwaita::prelude::*;
use libadwaita::Application;

const APP_ID: &str = "io.github.PatrLind.wifichecker";

fn main() {
    env_logger::init();

    let app = Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_activate(|app| {
        load_css();
        let win = window::Window::new(app);
        win.window.present();
    });

    app.run();
}

/// App-specific styles (channel-report table rows: hover highlight that
/// stays in sync with the chart bars).
fn load_css() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let css = r#"
label.channel-row {
    padding: 1px 4px;
    border-radius: 3px;
}
label.channel-row-hover {
    background-color: rgba(90, 140, 255, 0.30);
}
"#;
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(css);
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::StyleContext::add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
}
