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
    // TODO: Change to Debug after verifying the backpressure fix works
    console_log::init_with_level(log::Level::Trace).expect("Failed to initialize logger");

    log::info!("rflasher-wasm starting...");

    // Start the egui app
    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("rflasher_canvas"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("Failed to find canvas element 'rflasher_canvas'");

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(RflasherApp::new(cc)))),
            )
            .await;

        if let Err(e) = result {
            log::error!("Failed to start eframe: {:?}", e);
        }
    });
}
