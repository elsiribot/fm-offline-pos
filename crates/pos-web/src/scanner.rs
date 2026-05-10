use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, MediaStreamConstraints,
};

/// Start camera and scan QR codes, calling on_scan for each detected QR.
/// Returns a cleanup closure.
pub async fn start_camera(
    video: &HtmlVideoElement,
) -> Result<web_sys::MediaStream, String> {
    let window = web_sys::window().ok_or("No window")?;
    let navigator = window.navigator();
    let media_devices = navigator
        .media_devices()
        .map_err(|_| "No media devices")?;

    let mut constraints = MediaStreamConstraints::new();
    let video_constraints = js_sys::Object::new();
    js_sys::Reflect::set(
        &video_constraints,
        &"facingMode".into(),
        &"environment".into(),
    )
    .map_err(|_| "reflect error")?;
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
    let _ = video.play().map_err(|_| "play failed")?;

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
/// Uses a simple JS-based QR scanning approach via canvas + jsQR (loaded externally).
/// For this MVP, we'll use a paste-based approach as fallback and integrate
/// camera scanning via the BarcodeDetector API where available.
pub async fn scan_frame(video: &HtmlVideoElement) -> Option<String> {
    let document = web_sys::window()?.document()?;
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .ok()?
        .dyn_into()
        .ok()?;

    let w = video.video_width();
    let h = video.video_height();
    if w == 0 || h == 0 {
        return None;
    }

    canvas.set_width(w);
    canvas.set_height(h);

    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .ok()??
        .dyn_into()
        .ok()?;

    ctx.draw_image_with_html_video_element(video, 0.0, 0.0)
        .ok()?;

    // Try BarcodeDetector API (available in Chrome/Edge)
    let barcode_result = detect_barcode_from_canvas(&canvas).await;
    barcode_result
}

async fn detect_barcode_from_canvas(canvas: &HtmlCanvasElement) -> Option<String> {
    // Check if BarcodeDetector is available
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
