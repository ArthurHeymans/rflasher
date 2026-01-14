//! rflasher-wasm - Web interface for rflasher using egui and WebSerial
//!
//! This crate provides a browser-based interface for programming flash chips
//! via WebSerial-connected serprog programmers.

#![warn(missing_docs)]

mod app;
mod transport;

pub use app::RflasherApp;
pub use transport::WebSerialTransport;

use wasm_bindgen::prelude::*;

/// Initialize the web application
///
/// This is the entry point called from the HTML page.
#[wasm_bindgen(start)]
pub fn main() {
    // Set up panic hook for better error messages
    console_error_panic_hook::set_once();

    // Initialize logging
    console_log::init_with_level(log::Level::Debug).expect("Failed to initialize logger");

    log::info!("rflasher-wasm starting...");

    // Start the egui app
    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let window = web_sys::window().expect("No window");
        let document = window.document().expect("No document");

        let canvas = document
            .get_element_by_id("rflasher_canvas")
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("Failed to find canvas element 'rflasher_canvas'");

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(RflasherApp::new(cc)))),
            )
            .await;

        match result {
            Ok(()) => {
                // Hide the loading screen now that the app is ready
                if let Some(loading) = document.get_element_by_id("loading") {
                    if let Some(style) = loading.dyn_ref::<web_sys::HtmlElement>() {
                        let _ = style.style().set_property("display", "none");
                    }
                }
                log::info!("rflasher-wasm started successfully");
            }
            Err(e) => {
                log::error!("Failed to start eframe: {:?}", e);
                // Show error in the loading screen
                if let Some(loading) = document.get_element_by_id("loading") {
                    loading.set_inner_html(&format!(
                        "<h1>rflasher</h1><div class=\"error\"><strong>Failed to start</strong><p>{:?}</p></div>",
                        e
                    ));
                }
            }
        }
    });
}
