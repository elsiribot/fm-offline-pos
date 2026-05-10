use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, MediaStreamConstraints,
};

/// Start camera stream attached to a video element.
pub async fn start_camera(video: &HtmlVideoElement) -> Result<web_sys::MediaStream, String> {
    let window = web_sys::window().ok_or("No window")?;
    let navigator = window.navigator();
    let media_devices = navigator
        .media_devices()
        .map_err(|_| "No media devices available")?;

    let mut constraints = MediaStreamConstraints::new();
    let video_constraints = js_sys::Object::new();
    js_sys::Reflect::set(
        &video_constraints,
        &"facingMode".into(),
        &"environment".into(),
    )
    .map_err(|_| "reflect error")?;
    // Request decent resolution for QR scanning
    js_sys::Reflect::set(
        &video_constraints,
        &"width".into(),
        &wasm_bindgen::JsValue::from_f64(1280.0),
    )
    .ok();
    js_sys::Reflect::set(
        &video_constraints,
        &"height".into(),
        &wasm_bindgen::JsValue::from_f64(720.0),
    )
    .ok();

    constraints.video(&video_constraints.into());
    constraints.audio(&false.into());

    let promise = media_devices
        .get_user_media_with_constraints(&constraints)
        .map_err(|_| "getUserMedia failed")?;

    let stream_js = JsFuture::from(promise)
        .await
        .map_err(|e| format!("Camera access denied: {e:?}"))?;

    let stream: web_sys::MediaStream = stream_js.dyn_into().map_err(|_| "Not a MediaStream")?;

    video.set_src_object(Some(&stream));
    let play_promise = video.play().map_err(|_| "play failed")?;
    // Wait for play to resolve
    let _ = JsFuture::from(play_promise).await;

    Ok(stream)
}

pub fn stop_camera(stream: &web_sys::MediaStream) {
    let tracks = stream.get_tracks();
    for i in 0..tracks.length() {
        let track = tracks.get(i);
        if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
            track.stop();
        }
    }
}

/// Capture a frame from video and attempt QR decode.
/// Uses jsQR (loaded via CDN) with BarcodeDetector as fallback.
pub async fn scan_frame(video: &HtmlVideoElement) -> Option<String> {
    let w = video.video_width();
    let h = video.video_height();
    if w == 0 || h == 0 {
        return None;
    }

    let document = web_sys::window()?.document()?;
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .ok()?
        .dyn_into()
        .ok()?;

    canvas.set_width(w);
    canvas.set_height(h);

    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .ok()??
        .dyn_into()
        .ok()?;

    ctx.draw_image_with_html_video_element(video, 0.0, 0.0)
        .ok()?;

    // Try jsQR first (works everywhere)
    if let Some(result) = decode_with_jsqr(&ctx, w, h) {
        return Some(result);
    }

    // Fallback to BarcodeDetector API (Chrome/Edge)
    if let Some(result) = detect_barcode_from_canvas(&canvas).await {
        return Some(result);
    }

    None
}

/// Decode QR using the jsQR library loaded from CDN
fn decode_with_jsqr(ctx: &CanvasRenderingContext2d, w: u32, h: u32) -> Option<String> {
    let window = web_sys::window()?;

    // Check if jsQR is loaded
    let jsqr_fn = js_sys::Reflect::get(&window, &"jsQR".into()).ok()?;
    if jsqr_fn.is_undefined() || !jsqr_fn.is_function() {
        return None;
    }
    let jsqr: js_sys::Function = jsqr_fn.dyn_into().ok()?;

    // Get image data from canvas
    let image_data = ctx
        .get_image_data(0.0, 0.0, w as f64, h as f64)
        .ok()?;

    let data = image_data.data();

    // Create a Uint8ClampedArray view for jsQR
    let uint8_array = js_sys::Uint8ClampedArray::from(data.as_ref());

    // Call jsQR(imageData, width, height)
    let result = jsqr.call3(
        &wasm_bindgen::JsValue::NULL,
        &uint8_array.into(),
        &wasm_bindgen::JsValue::from_f64(w as f64),
        &wasm_bindgen::JsValue::from_f64(h as f64),
    )
    .ok()?;

    if result.is_null() || result.is_undefined() {
        return None;
    }

    // result.data contains the decoded string
    let data_val = js_sys::Reflect::get(&result, &"data".into()).ok()?;
    data_val.as_string()
}

async fn detect_barcode_from_canvas(canvas: &HtmlCanvasElement) -> Option<String> {
    let window = web_sys::window()?;
    let barcode_detector_class = js_sys::Reflect::get(&window, &"BarcodeDetector".into()).ok()?;
    if barcode_detector_class.is_undefined() {
        return None;
    }

    let formats = js_sys::Array::new();
    formats.push(&"qr_code".into());

    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"formats".into(), &formats.into()).ok()?;

    let args = js_sys::Array::new();
    args.push(&opts.into());
    let detector = js_sys::Reflect::construct(
        &barcode_detector_class.dyn_into::<js_sys::Function>().ok()?,
        &args,
    )
    .ok()?;

    let detect_fn: js_sys::Function =
        js_sys::Reflect::get(&detector, &"detect".into())
            .ok()?
            .dyn_into()
            .ok()?;

    let promise: js_sys::Promise = detect_fn
        .call1(&detector, canvas)
        .ok()?
        .dyn_into()
        .ok()?;

    let result = JsFuture::from(promise).await.ok()?;
    let arr: js_sys::Array = result.dyn_into().ok()?;

    if arr.length() == 0 {
        return None;
    }

    let first = arr.get(0);
    let raw_value = js_sys::Reflect::get(&first, &"rawValue".into()).ok()?;
    raw_value.as_string()
}
