// SPDX-License-Identifier: AGPL-3.0-only

mod app;
mod config;
mod popup;
mod wayland;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    app::run()
}
