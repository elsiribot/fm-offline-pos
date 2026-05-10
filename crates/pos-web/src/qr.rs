use qrcode::render::svg;
use qrcode::QrCode;

use base64::Engine;

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

/// Split data into qrloop-compatible frames for animated QR display.
pub fn split_for_animated_qr(data: &str, max_chunk_size: usize) -> Vec<String> {
    let raw_bytes = data.as_bytes();

    // Wrap data: 4-byte length + 16-byte md5-like hash + data
    let mut wrapped = Vec::with_capacity(20 + raw_bytes.len());
    wrapped.extend_from_slice(&(raw_bytes.len() as u32).to_be_bytes());
    wrapped.extend_from_slice(&simple_hash(raw_bytes));
    wrapped.extend_from_slice(raw_bytes);

    let chunks: Vec<&[u8]> = wrapped.chunks(max_chunk_size).collect();
    let total = chunks.len();

    if total <= 1 {
        return vec![data.to_string()];
    }

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut frame = Vec::with_capacity(5 + chunk.len());
            frame.push(0u8); // nonce
            frame.extend_from_slice(&(total as u16).to_be_bytes());
            frame.extend_from_slice(&(i as u16).to_be_bytes());
            frame.extend_from_slice(chunk);
            base64::engine::general_purpose::STANDARD.encode(&frame)
        })
        .collect()
}

fn simple_hash(data: &[u8]) -> [u8; 16] {
    let mut hash = [0u8; 16];
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    hash[..8].copy_from_slice(&h.to_le_bytes());
    h = h.wrapping_mul(0x100000001b3);
    for &b in data.iter().rev() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    hash[8..].copy_from_slice(&h.to_le_bytes());
    hash
}

/// Try to parse a scanned QR string as a qrloop frame.
/// Returns (total_frames, frame_index, data_chunk) if valid.
fn try_parse_qrloop_frame(raw: &str) -> Option<(usize, usize, Vec<u8>)> {
    let frame_bytes = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    if frame_bytes.len() < 6 {
        return None; // Too short for qrloop header + any data
    }

    let _nonce = frame_bytes[0];
    let total = u16::from_be_bytes([frame_bytes[1], frame_bytes[2]]) as usize;
    let index = u16::from_be_bytes([frame_bytes[3], frame_bytes[4]]) as usize;
    let data = frame_bytes[5..].to_vec();

    // Strict validation
    if total < 2 || total > 500 {
        return None; // Single-frame wouldn't use qrloop; >500 is unreasonable
    }
    if index >= total {
        return None;
    }
    if data.is_empty() {
        return None;
    }

    Some((total, index, data))
}

/// Assemble collected qrloop frames into the final ecash string.
/// Unwraps qrloop data format: [length(4 BE)] [md5(16)] [raw_ecash_bytes]
/// Since Fedi passes Buffer.from(ecash, 'base64') to dataToFrames,
/// the raw bytes are the decoded ecash — we re-encode as base64 URL-safe.
pub fn assemble_qrloop_frames(frames: &[Vec<u8>]) -> Option<String> {
    let mut assembled = Vec::new();
    for frame in frames {
        assembled.extend_from_slice(frame);
    }

    if assembled.len() < 20 {
        return None;
    }

    let data_len =
        u32::from_be_bytes([assembled[0], assembled[1], assembled[2], assembled[3]]) as usize;

    let raw_data = &assembled[20..]; // skip length(4) + hash(16)

    if raw_data.len() < data_len {
        return None;
    }

    let ecash_bytes = &raw_data[..data_len];

    // Re-encode as base64 URL-safe (this is what ecash::parse_notes expects)
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::general_purpose::PAD,
    );
    Some(engine.encode(ecash_bytes))
}

/// State for collecting qrloop animated QR frames.
#[derive(Clone, Debug)]
pub struct AnimatedQrCollector {
    frames: Vec<Option<Vec<u8>>>,
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

    /// Process a scanned QR string.
    /// Returns:
    ///   ProcessResult::Complete(ecash_string) — ready to parse
    ///   ProcessResult::Progress(fraction) — still collecting frames
    ///   ProcessResult::NotAFrame — not a qrloop frame, caller should try direct parse
    pub fn process_scan(&mut self, raw: &str) -> ProcessResult {
        let Some((total, index, data)) = try_parse_qrloop_frame(raw) else {
            return ProcessResult::NotAFrame;
        };

        // If total changed and we had progress, don't reset — skip this frame
        if self.total != 0 && self.total != total {
            return ProcessResult::Progress(self.progress());
        }

        // Initialize on first valid frame
        if self.total == 0 {
            self.frames = vec![None; total];
            self.total = total;
            self.received = 0;
        }

        if self.frames[index].is_none() {
            self.frames[index] = Some(data);
            self.received += 1;
        }

        if self.received >= self.total {
            let frame_data: Vec<Vec<u8>> = self
                .frames
                .iter()
                .filter_map(|f| f.clone())
                .collect();

            if let Some(ecash_str) = assemble_qrloop_frames(&frame_data) {
                return ProcessResult::Complete(ecash_str);
            }
        }

        ProcessResult::Progress(self.progress())
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

pub enum ProcessResult {
    /// All frames collected, here's the ecash string
    Complete(String),
    /// Still collecting, here's the progress (0.0-1.0)
    Progress(f64),
    /// Not a qrloop frame — the raw string might be direct ecash
    NotAFrame,
}
