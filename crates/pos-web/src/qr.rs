use qrcode::QrCode;
use qrcode::render::svg;

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

/// Split data into frames for animated QR display (qrloop-compatible format).
/// Each frame is: "frameIndex/totalFrames|data_chunk"
pub fn split_for_animated_qr(data: &str, max_chunk_size: usize) -> Vec<String> {
    let chunks: Vec<&str> = data
        .as_bytes()
        .chunks(max_chunk_size)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    let total = chunks.len();
    if total <= 1 {
        return vec![data.to_string()];
    }

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| format!("{}/{}/{}", i + 1, total, chunk))
        .collect()
}

/// Check if a scanned string is a qrloop frame
pub fn is_animated_frame(s: &str) -> bool {
    // Format: "index/total/data"
    let parts: Vec<&str> = s.splitn(3, '/').collect();
    if parts.len() != 3 {
        return false;
    }
    parts[0].parse::<usize>().is_ok() && parts[1].parse::<usize>().is_ok()
}

/// State for collecting animated QR frames
#[derive(Clone, Debug)]
pub struct AnimatedQrCollector {
    frames: Vec<Option<String>>,
    total: usize,
    received: usize,
}

impl AnimatedQrCollector {
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            total: 0,
            received: 0,
        }
    }

    /// Add a frame. Returns Some(complete_data) when all frames collected.
    pub fn add_frame(&mut self, raw: &str) -> Option<String> {
        // Try parsing as animated frame
        let parts: Vec<&str> = raw.splitn(3, '/').collect();
        if parts.len() != 3 {
            // Single static QR, return as-is
            return Some(raw.to_string());
        }

        let Ok(index) = parts[0].parse::<usize>() else {
            return Some(raw.to_string());
        };
        let Ok(total) = parts[1].parse::<usize>() else {
            return Some(raw.to_string());
        };

        if total == 0 || index == 0 || index > total {
            return Some(raw.to_string());
        }

        // Initialize if needed
        if self.total != total {
            self.frames = vec![None; total];
            self.total = total;
            self.received = 0;
        }

        let idx = index - 1;
        if self.frames[idx].is_none() {
            self.frames[idx] = Some(parts[2].to_string());
            self.received += 1;
        }

        if self.received >= self.total {
            let data: String = self
                .frames
                .iter()
                .filter_map(|f| f.as_ref())
                .cloned()
                .collect();
            Some(data)
        } else {
            None
        }
    }

    pub fn progress(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.received as f64 / self.total as f64
    }

    pub fn reset(&mut self) {
        self.frames.clear();
        self.total = 0;
        self.received = 0;
    }
}
