use qrcode::render::svg;
use qrcode::QrCode;

use base64::Engine;
use wasm_bindgen::JsCast;

/// Generate a static QR code as SVG string
pub fn generate_qr_svg(data: &str) -> Result<String, String> {
    let code = QrCode::new(data.as_bytes()).map_err(|e| format!("QR encode error: {e}"))?;
    let svg_str = code
        .render()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(svg_str)
}

/// Split a base64 ecash string into qrloop frames using the actual qrloop JS library.
/// Matches Fedi: `dataToFrames(Buffer.from(ecash, 'base64'))`.
pub fn split_for_animated_qr(ecash_base64: &str, _chunk_size: usize) -> Vec<String> {
    // Decode ecash base64 to raw bytes
    let raw_bytes = base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::general_purpose::PAD,
    )
    .decode(ecash_base64)
    .or_else(|_| base64::engine::general_purpose::STANDARD.decode(ecash_base64))
    .unwrap_or_else(|_| ecash_base64.as_bytes().to_vec());

    tracing::info!(
        ecash_len = ecash_base64.len(),
        raw_len = raw_bytes.len(),
        "split_for_animated_qr: encoding with qrloop"
    );

    // Call qrloop.dataToFrames(Uint8Array) from JS
    if let Some(frames) = call_qrloop_data_to_frames(&raw_bytes) {
        tracing::info!(num_frames = frames.len(), "split_for_animated_qr: got frames from qrloop");
        if !frames.is_empty() {
            return frames;
        }
    }

    tracing::warn!("split_for_animated_qr: qrloop not available, falling back to single QR");
    vec![ecash_base64.to_string()]
}

/// Call window.qrloop.dataToFrames(Uint8Array) -> string[]
fn call_qrloop_data_to_frames(data: &[u8]) -> Option<Vec<String>> {
    let window = web_sys::window()?;
    let qrloop = js_sys::Reflect::get(&window, &"qrloop".into()).ok()?;
    if qrloop.is_undefined() {
        return None;
    }
    let dtf: js_sys::Function = js_sys::Reflect::get(&qrloop, &"dataToFrames".into())
        .ok()?
        .dyn_into()
        .ok()?;

    let uint8 = js_sys::Uint8Array::from(data);
    let result = dtf
        .call1(&wasm_bindgen::JsValue::NULL, &uint8)
        .ok()?;
    let arr: js_sys::Array = result.dyn_into().ok()?;

    let mut frames = Vec::new();
    for i in 0..arr.length() {
        if let Some(s) = arr.get(i).as_string() {
            frames.push(s);
        }
    }
    Some(frames)
}

/// Process a scanned QR string through qrloop.parseFramesReducer.
/// Returns the collector state as an opaque JsValue.
pub struct QrloopCollector {
    state: wasm_bindgen::JsValue,
}

impl QrloopCollector {
    pub fn new() -> Self {
        Self {
            state: wasm_bindgen::JsValue::NULL,
        }
    }

    /// Feed a scanned frame. Returns ProcessResult.
    pub fn process_scan(&mut self, raw: &str) -> ProcessResult {
        let Some(window) = web_sys::window() else {
            return ProcessResult::NotAFrame;
        };
        let Ok(qrloop) = js_sys::Reflect::get(&window, &"qrloop".into()) else {
            return ProcessResult::NotAFrame;
        };
        if qrloop.is_undefined() {
            return ProcessResult::NotAFrame;
        }

        // parseFramesReducer(state, chunkStr)
        let Ok(pfr) = js_sys::Reflect::get(&qrloop, &"parseFramesReducer".into()) else {
            return ProcessResult::NotAFrame;
        };
        let Ok(pfr_fn) = pfr.dyn_into::<js_sys::Function>() else {
            return ProcessResult::NotAFrame;
        };

        let new_state = match pfr_fn.call2(
            &wasm_bindgen::JsValue::NULL,
            &self.state,
            &wasm_bindgen::JsValue::from_str(raw),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(?e, "qrloop parseFramesReducer threw");
                return ProcessResult::NotAFrame;
            }
        };

        self.state = new_state;

        // Get progress
        let progress = call_js_fn(&qrloop, "progressOfFrames", &self.state)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        tracing::debug!(progress, "qrloop: frame processed");

        // Check completion
        let complete = call_js_fn(&qrloop, "areFramesComplete", &self.state)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if complete {
            tracing::info!("qrloop: all frames collected, assembling");
            let ftd_result = js_sys::Reflect::get(&qrloop, &"framesToData".into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .map(|f| f.call1(&wasm_bindgen::JsValue::NULL, &self.state));

            match ftd_result {
                Some(Ok(data)) => {
                    if let Ok(uint8) = data.dyn_into::<js_sys::Uint8Array>() {
                        let bytes = uint8.to_vec();
                        tracing::info!(data_len = bytes.len(), "qrloop: assembled data");
                        let engine = base64::engine::GeneralPurpose::new(
                            &base64::alphabet::URL_SAFE,
                            base64::engine::general_purpose::PAD,
                        );
                        return ProcessResult::Complete(engine.encode(&bytes));
                    } else {
                        tracing::warn!("qrloop: framesToData returned non-Uint8Array");
                    }
                }
                Some(Err(e)) => {
                    tracing::warn!(?e, "qrloop: framesToData threw");
                }
                None => {
                    tracing::warn!("qrloop: framesToData function not found");
                }
            }
        }

        ProcessResult::Progress(progress)
    }

    pub fn reset(&mut self) {
        self.state = wasm_bindgen::JsValue::NULL;
    }
}

fn call_js_fn(obj: &wasm_bindgen::JsValue, name: &str, arg: &wasm_bindgen::JsValue) -> Option<wasm_bindgen::JsValue> {
    let func: js_sys::Function = js_sys::Reflect::get(obj, &name.into())
        .ok()?
        .dyn_into()
        .ok()?;
    func.call1(&wasm_bindgen::JsValue::NULL, arg).ok()
}

pub enum ProcessResult {
    Complete(String),
    Progress(f64),
    NotAFrame,
}
