//! Browser file open and save dialogs.

use std::cell::RefCell;
use std::rc::Rc;

// =============================================================================
// File I/O using browser APIs
// =============================================================================

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// Load a file using the browser's file input dialog
pub(crate) async fn load_file_dialog() -> Result<Vec<u8>, String> {
    use web_sys::{Document, HtmlInputElement};

    let window = web_sys::window().ok_or("No window")?;
    let document: Document = window.document().ok_or("No document")?;

    // Create a hidden file input
    let input: HtmlInputElement = document
        .create_element("input")
        .map_err(|_| "Failed to create input")?
        .dyn_into()
        .map_err(|_| "Not an input element")?;

    input.set_type("file");
    input.set_accept(".bin,.rom,.img,*/*");

    // Use a promise to wait for the file selection
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<u8>, String>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let closure = {
        let input = input.clone();
        let tx = tx.clone();

        Closure::once(Box::new(move || {
            let files = input.files();
            if let Some(files) = files
                && files.length() > 0
                && let Some(file) = files.get(0)
            {
                let tx = tx.clone();
                let reader = web_sys::FileReader::new().unwrap();
                let reader_clone = reader.clone();

                let onload = Closure::once(Box::new(move || {
                    let result = reader_clone.result().unwrap();
                    let array = js_sys::Uint8Array::new(&result);
                    let data = array.to_vec();
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(Ok(data));
                    }
                }) as Box<dyn FnOnce()>);

                reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                onload.forget();

                reader.read_as_array_buffer(&file).unwrap();
                return;
            }
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Err("No file selected".to_string()));
            }
        }) as Box<dyn FnOnce()>)
    };

    input.set_onchange(Some(closure.as_ref().unchecked_ref()));
    closure.forget();

    // Click the input to open dialog
    input.click();

    // Wait for result
    rx.await.map_err(|_| "Channel closed".to_string())?
}

/// Save a file using the browser's download functionality
pub(crate) async fn save_file_dialog(data: &[u8], filename: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("No window")?;
    let document = window.document().ok_or("No document")?;

    // Create a Blob from the data
    let array = js_sys::Uint8Array::from(data);
    let blob_parts = js_sys::Array::new();
    blob_parts.push(&array);

    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/octet-stream");

    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&blob_parts, &options)
        .map_err(|_| "Failed to create blob")?;

    // Create a URL for the blob
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "Failed to create URL")?;

    // Create a download link and click it
    let a: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| "Failed to create anchor")?
        .dyn_into()
        .map_err(|_| "Not an anchor element")?;

    a.set_href(&url);
    a.set_download(filename);
    a.click();

    // Clean up the URL
    web_sys::Url::revoke_object_url(&url).map_err(|_| "Failed to revoke URL")?;

    Ok(())
}
