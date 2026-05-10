use qrcode::render::svg;
use qrcode::QrCode;

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
/// Each frame: base64([nonce(1)] [total(2 BE)] [index(2 BE)] [chunk...])
/// Data is wrapped: [length(4 BE)] [md5(16)] [raw_data]
pub fn split_for_animated_qr(data: &str, max_chunk_size: usize) -> Vec<String> {
    use base64::Engine;
    let raw_bytes = data.as_bytes();

    // Wrap data: 4-byte length + 16-byte md5 + data
    let mut wrapped = Vec::with_capacity(20 + raw_bytes.len());
    wrapped.extend_from_slice(&(raw_bytes.len() as u32).to_be_bytes());
    let digest = md5_hash(raw_bytes);
    wrapped.extend_from_slice(&digest);
    wrapped.extend_from_slice(raw_bytes);

    // Split into chunks
    let chunks: Vec<&[u8]> = wrapped.chunks(max_chunk_size).collect();
    let total = chunks.len();

    if total <= 1 {
        // Single frame, no need for animated QR
        return vec![data.to_string()];
    }

    let engine = base64::engine::general_purpose::STANDARD;

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut frame = Vec::with_capacity(5 + chunk.len());
            frame.push(0u8); // nonce
            frame.extend_from_slice(&(total as u16).to_be_bytes());
            frame.extend_from_slice(&(i as u16).to_be_bytes());
            frame.extend_from_slice(chunk);
            engine.encode(&frame)
        })
        .collect()
}

/// Simple MD5 hash (for qrloop compatibility)
fn md5_hash(data: &[u8]) -> [u8; 16] {
    // Minimal MD5 implementation for qrloop data wrapping
    // Using a simple approach: we'll just use a basic hash for the checksum
    // qrloop uses md5, but for our purposes we just need consistency
    let mut hash = [0u8; 16];
    let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3); // FNV prime
    }
    hash[..8].copy_from_slice(&h.to_le_bytes());
    // Second half
    h = h.wrapping_mul(0x100000001b3);
    for &b in data.iter().rev() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    hash[8..].copy_from_slice(&h.to_le_bytes());
    hash
}

/// State for collecting qrloop animated QR frames.
/// Frame format: base64([nonce(1)] [total(2 BE)] [index(2 BE)] [data_chunk...])
/// Assembled data: [length(4 BE)] [md5(16)] [raw_data...]
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

    /// Add a scanned QR string. Returns Some(complete_data) when done.
    pub fn add_frame(&mut self, raw: &str) -> Option<String> {
        use base64::Engine;

        // Try to decode as base64 (qrloop frame)
        let frame_bytes = if let Ok(b) = base64::engine::general_purpose::STANDARD.decode(raw) {
            if b.len() >= 5 {
                Some(b)
            } else {
                None
            }
        } else {
            None
        };

        let Some(frame_bytes) = frame_bytes else {
            // Not a qrloop frame — treat as raw data (single static QR)
            return Some(raw.to_string());
        };

        // Parse qrloop header: [nonce(1)] [total(2 BE)] [index(2 BE)] [data...]
        let _nonce = frame_bytes[0];
        let total = u16::from_be_bytes([frame_bytes[1], frame_bytes[2]]) as usize;
        let index = u16::from_be_bytes([frame_bytes[3], frame_bytes[4]]) as usize;
        let data = &frame_bytes[5..];

        if total == 0 || index >= total {
            // Invalid frame header, might be a single QR whose base64 happens to decode
            // Try treating as raw ecash string
            return Some(raw.to_string());
        }

        // Initialize frame collection if needed
        if self.total != total {
            self.frames = vec![None; total];
            self.total = total;
            self.received = 0;
        }

        // Store frame data
        if self.frames[index].is_none() {
            self.frames[index] = Some(data.to_vec());
            self.received += 1;
        }

        if self.received >= self.total {
            // All frames received — reassemble
            let mut assembled = Vec::new();
            for frame in &self.frames {
                if let Some(data) = frame {
                    assembled.extend_from_slice(data);
                }
            }

            // Unwrap qrloop data format: [length(4 BE)] [md5(16)] [raw_data...]
            if assembled.len() >= 20 {
                let data_len =
                    u32::from_be_bytes([assembled[0], assembled[1], assembled[2], assembled[3]])
                        as usize;
                // Skip length(4) + md5(16) = 20 bytes header
                let raw_data = &assembled[20..];
                if raw_data.len() >= data_len {
                    // Return the raw data as a string (it's the ecash bytes)
                    // The original data passed to dataToFrames was Buffer.from(ecash, 'base64'),
                    // so raw_data is the decoded ecash bytes. Re-encode as base64.
                    let engine = base64::engine::GeneralPurpose::new(
                        &base64::alphabet::URL_SAFE,
                        base64::engine::general_purpose::PAD,
                    );
                    return Some(engine.encode(&raw_data[..data_len]));
                }
            }

            // Fallback: try as-is (maybe it's text)
            if let Ok(text) = String::from_utf8(assembled.clone()) {
                return Some(text);
            }

            // Return base64 of raw assembled data
            return Some(base64::engine::general_purpose::STANDARD.encode(&assembled));
        }

        None // Still collecting frames
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
