//! Minimal ecash note parsing without depending on the full fedimint crates.
//! Supports both v1 OOBNotes and v2 ECash formats.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use base64::Engine;

/// Federation ID prefix (first 4 bytes of the 32-byte federation ID)
pub type FederationIdPrefix = [u8; 4];

/// A parsed ecash note bundle
#[derive(Clone, Debug)]
pub struct ParsedNotes {
    pub federation_id_prefix: FederationIdPrefix,
    /// denomination_msats -> count of notes at that denomination
    pub denominations: BTreeMap<u64, usize>,
    /// The raw bytes for each denomination: denomination_msats -> list of raw note bytes
    raw_notes: BTreeMap<u64, Vec<Vec<u8>>>,
    /// Which format was used (for re-encoding)
    format: NoteFormat,
}

#[derive(Clone, Debug)]
enum NoteFormat {
    V1, // OOBNotes
    V2, // mintv2 ECash
}

impl ParsedNotes {
    pub fn total_msats(&self) -> u64 {
        self.denominations
            .iter()
            .map(|(denom, count)| denom * (*count as u64))
            .sum()
    }
}

/// Parse ecash string (fedimint base32, base64url, or base64 standard)
pub fn parse_notes(s: &str) -> Result<ParsedNotes, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();

    // Try all decodings and all formats, collect errors
    let decodings = decode_all_variants(&s);
    if decodings.is_empty() {
        return Err("Could not decode ecash string".to_string());
    }

    let mut last_err = String::new();
    for bytes in &decodings {
        // Try v1 OOBNotes format
        if let Ok(parsed) = parse_v1_oob_notes(bytes) {
            return Ok(parsed);
        }
        // Try v2 ECash format
        match parse_v2_ecash(bytes) {
            Ok(parsed) => return Ok(parsed),
            Err(e) => last_err = e,
        }
    }

    Err(format!(
        "Parse failed: {last_err} (input_len: {}, starts: {:?}, decodings: {}, first_decode_len: {}, first_bytes: {:02x?})",
        s.len(),
        &s[..s.len().min(20)],
        decodings.len(),
        decodings[0].len(),
        &decodings[0][..decodings[0].len().min(12)],
    ))
}

/// Check if federation prefix matches
pub fn check_federation(notes: &ParsedNotes, expected_prefix: &FederationIdPrefix) -> bool {
    notes.federation_id_prefix == *expected_prefix
}

/// Parse a federation invite code to extract the federation ID (32 bytes).
pub fn parse_invite_code(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    let lower = s.to_lowercase();
    // Try fedimint base32hex "fedimint..." format
    if lower.starts_with("fedimint") {
        let bytes = decode_fedimint_base32(&lower)?;
        return parse_invite_bytes(&bytes);
    }
    // Try bech32 "fed1..." format
    if lower.starts_with("fed1") {
        let (_hrp, data) =
            bech32::decode(&lower).map_err(|e| format!("Invalid invite code: {e}"))?;
        return parse_invite_bytes(&data);
    }
    Err("Invite code must start with 'fed1' or 'fedimint'".to_string())
}

/// Get prefix from full federation ID
pub fn federation_id_to_prefix(fed_id: &[u8; 32]) -> FederationIdPrefix {
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&fed_id[..4]);
    prefix
}

/// Split notes by denomination into separate re-encodable strings
pub fn split_notes_by_denomination(notes: &ParsedNotes) -> BTreeMap<u64, Vec<String>> {
    let mut result: BTreeMap<u64, Vec<String>> = BTreeMap::new();

    match notes.format {
        NoteFormat::V1 => {
            for (denom, note_list) in &notes.raw_notes {
                for note_bytes in note_list {
                    let encoded =
                        encode_single_note_v1_oob(&notes.federation_id_prefix, *denom, note_bytes);
                    let encoded_str = BASE64_URL_SAFE.encode(&encoded);
                    result.entry(*denom).or_default().push(encoded_str);
                }
            }
        }
        NoteFormat::V2 => {
            for (denom, note_list) in &notes.raw_notes {
                for note_bytes in note_list {
                    let encoded =
                        encode_single_note_v2_ecash(&notes.federation_id_prefix, note_bytes);
                    let encoded_str = BASE64_URL_SAFE.encode(&encoded);
                    result.entry(*denom).or_default().push(encoded_str);
                }
            }
        }
    }

    result
}

/// Combine multiple single-note strings into one ecash string
pub fn combine_note_strings(note_strings: &[String]) -> Result<String, String> {
    if note_strings.is_empty() {
        return Err("No notes to combine".to_string());
    }

    let mut fed_prefix: Option<FederationIdPrefix> = None;
    let mut all_notes: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();
    let mut format = NoteFormat::V1;

    for s in note_strings {
        let decodings = decode_all_variants(s);
        let mut parsed = None;
        for bytes in &decodings {
            if let Ok(p) = parse_v1_oob_notes(bytes) {
                parsed = Some(p);
                break;
            }
            if let Ok(p) = parse_v2_ecash(bytes) {
                parsed = Some(p);
                break;
            }
        }
        let p = parsed.ok_or("Could not parse note for combining")?;

        if let Some(existing) = &fed_prefix {
            if *existing != p.federation_id_prefix {
                return Err("Mixed federation notes".to_string());
            }
        } else {
            fed_prefix = Some(p.federation_id_prefix);
            format = p.format;
        }
        for (denom, notes) in p.raw_notes {
            all_notes.entry(denom).or_default().extend(notes);
        }
    }

    let prefix = fed_prefix.ok_or("No notes")?;
    let encoded = match format {
        NoteFormat::V1 => encode_multi_note_v1_oob(&prefix, &all_notes),
        NoteFormat::V2 => encode_multi_note_v2_ecash(&prefix, &all_notes),
    };
    Ok(BASE64_URL_SAFE.encode(&encoded))
}

// ─── Decoding ────────────────────────────────────────────────────

const BASE64_URL_SAFE: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::general_purpose::PAD,
);

const BASE64_URL_SAFE_NO_PAD: base64::engine::GeneralPurpose =
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::general_purpose::NO_PAD,
    );

const BASE64_STANDARD_NO_PAD: base64::engine::GeneralPurpose =
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::general_purpose::NO_PAD,
    );

/// Try all possible decodings and return all that succeed
fn decode_all_variants(s: &str) -> Vec<Vec<u8>> {
    let lower = s.to_lowercase();
    let mut results = Vec::new();

    // Try fedimint base32hex first (highest priority — most specific prefix)
    if lower.starts_with("fedimint") {
        if let Ok(bytes) = decode_fedimint_base32(&lower) {
            if !bytes.is_empty() {
                results.push(bytes);
            }
        }
    }

    // Try base64 variants
    fn try_b64(results: &mut Vec<Vec<u8>>, s: &str, engine: &impl base64::Engine) {
        if let Ok(bytes) = engine.decode(s) {
            if !bytes.is_empty() && !results.iter().any(|r| r == &bytes) {
                results.push(bytes);
            }
        }
    }
    try_b64(&mut results, s, &BASE64_URL_SAFE);
    try_b64(&mut results, s, &BASE64_URL_SAFE_NO_PAD);
    try_b64(&mut results, s, &base64::engine::general_purpose::STANDARD);
    try_b64(&mut results, s, &BASE64_STANDARD_NO_PAD);

    results
}

/// Fedimint uses RFC 4648 base32hex (lowercase) with "fedimint" text prefix.
fn decode_fedimint_base32(s: &str) -> Result<Vec<u8>, String> {
    const PREFIX: &str = "fedimint";
    if !s.starts_with(PREFIX) {
        return Err("Missing fedimint prefix".to_string());
    }
    base32hex_decode(&s[PREFIX.len()..])
}

/// RFC 4648 base32hex decode (lowercase alphabet: 0-9 a-v)
fn base32hex_decode(input: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";

    let mut output = Vec::with_capacity((5 * input.len()) / 8 + 1);
    let mut buffer: usize = 0;
    let mut bits: usize = 0;

    for &byte in input.as_bytes() {
        let value = ALPHABET
            .iter()
            .position(|&c| c == byte)
            .ok_or_else(|| format!("Invalid base32 character: '{}'", byte as char))?;

        buffer |= value << bits;
        bits += 5;

        while bits >= 8 {
            output.push((buffer & 0xFF) as u8);
            buffer >>= 8;
            bits -= 8;
        }
    }

    Ok(output)
}

/// RFC 4648 base32hex encode (lowercase)
fn base32hex_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";

    let mut output = Vec::with_capacity((8 * input.len()) / 5 + 1);
    let mut buffer: usize = 0;
    let mut bits: usize = 0;

    for &byte in input {
        buffer |= (byte as usize) << bits;
        bits += 8;

        while bits >= 5 {
            output.push(ALPHABET[buffer & 0b11111]);
            buffer >>= 5;
            bits -= 5;
        }
    }

    if bits > 0 {
        output.push(ALPHABET[buffer & 0b11111]);
    }

    String::from_utf8(output).unwrap_or_default()
}

// ─── Varint (BigSize) ────────────────────────────────────────────

fn read_varint(r: &mut impl Read) -> Result<u64, String> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)
        .map_err(|e| format!("Read error: {e}"))?;
    match first[0] {
        0..=0xfc => Ok(first[0] as u64),
        0xfd => {
            let mut buf = [0u8; 2];
            r.read_exact(&mut buf)
                .map_err(|e| format!("Read error: {e}"))?;
            Ok(u16::from_be_bytes(buf) as u64)
        }
        0xfe => {
            let mut buf = [0u8; 4];
            r.read_exact(&mut buf)
                .map_err(|e| format!("Read error: {e}"))?;
            Ok(u32::from_be_bytes(buf) as u64)
        }
        0xff => {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf)
                .map_err(|e| format!("Read error: {e}"))?;
            Ok(u64::from_be_bytes(buf))
        }
    }
}

fn write_varint(w: &mut Vec<u8>, val: u64) {
    if val <= 0xfc {
        w.push(val as u8);
    } else if val <= 0xffff {
        w.push(0xfd);
        w.extend_from_slice(&(val as u16).to_be_bytes());
    } else if val <= 0xffffffff {
        w.push(0xfe);
        w.extend_from_slice(&(val as u32).to_be_bytes());
    } else {
        w.push(0xff);
        w.extend_from_slice(&val.to_be_bytes());
    }
}

// ─── V1 OOBNotes parsing ────────────────────────────────────────

/// Parse v1 OOBNotes: Vec<OOBNotesPart>
/// OOBNotesPart variants:
///   0 = Notes(TieredMulti<SpendableNote>)
///   1 = FederationIdPrefix(4 bytes)
///   2 = Invite { peer_apis, federation_id }
///   3 = ApiSecret(String)
fn parse_v1_oob_notes(bytes: &[u8]) -> Result<ParsedNotes, String> {
    let mut cursor = Cursor::new(bytes);

    let num_parts = read_varint(&mut cursor)?;
    if num_parts == 0 || num_parts > 100 {
        return Err(format!("Invalid v1 part count: {num_parts}"));
    }

    let mut federation_id_prefix: Option<FederationIdPrefix> = None;
    let mut denominations: BTreeMap<u64, usize> = BTreeMap::new();
    let mut raw_notes: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();

    for _ in 0..num_parts {
        let variant_idx = read_varint(&mut cursor)?;
        let body_len = read_varint(&mut cursor)?;
        if body_len > 10_000_000 {
            return Err("Body too large".to_string());
        }
        let mut body = vec![0u8; body_len as usize];
        cursor
            .read_exact(&mut body)
            .map_err(|e| format!("Read error: {e}"))?;

        match variant_idx {
            0 => {
                // Notes: TieredMulti<SpendableNote> = BTreeMap<Amount, Vec<SpendableNote>>
                parse_v1_tiered_multi(&body, &mut denominations, &mut raw_notes)?;
            }
            1 => {
                if body.len() < 4 {
                    return Err("FederationIdPrefix too short".to_string());
                }
                let mut prefix = [0u8; 4];
                prefix.copy_from_slice(&body[..4]);
                federation_id_prefix = Some(prefix);
            }
            2 => {
                // Invite: extract federation_id if we don't have prefix yet
                if federation_id_prefix.is_none() {
                    if let Ok(fid) = extract_federation_id_from_invite(&body) {
                        let mut prefix = [0u8; 4];
                        prefix.copy_from_slice(&fid[..4]);
                        federation_id_prefix = Some(prefix);
                    }
                }
            }
            _ => {} // skip unknown
        }
    }

    let federation_id_prefix =
        federation_id_prefix.ok_or("No federation ID in v1 notes")?;

    if denominations.is_empty() {
        return Err("No notes in v1 data".to_string());
    }

    Ok(ParsedNotes {
        federation_id_prefix,
        denominations,
        raw_notes,
        format: NoteFormat::V1,
    })
}

/// Parse v1 TieredMulti: BTreeMap<Amount, Vec<SpendableNote>>
/// SpendableNote v1 = signature(48 bytes BLS) + spend_key(32 bytes Keypair) = 80 bytes
fn parse_v1_tiered_multi(
    body: &[u8],
    denominations: &mut BTreeMap<u64, usize>,
    raw_notes: &mut BTreeMap<u64, Vec<Vec<u8>>>,
) -> Result<(), String> {
    let mut cursor = Cursor::new(body);
    let num_tiers = read_varint(&mut cursor)?;
    if num_tiers > 1000 {
        return Err("Too many tiers".to_string());
    }

    for _ in 0..num_tiers {
        let amount_msats = read_varint(&mut cursor)?;
        let num_notes = read_varint(&mut cursor)?;
        if num_notes > 100_000 {
            return Err("Too many notes in tier".to_string());
        }

        for _ in 0..num_notes {
            let mut note_bytes = vec![0u8; 80];
            cursor
                .read_exact(&mut note_bytes)
                .map_err(|e| format!("Failed to read v1 note: {e}"))?;
            raw_notes
                .entry(amount_msats)
                .or_default()
                .push(note_bytes);
        }

        *denominations.entry(amount_msats).or_insert(0) += num_notes as usize;
    }

    Ok(())
}

// ─── V2 ECash parsing ───────────────────────────────────────────

/// Parse v2 ECash: Vec<ECashField>
/// ECashField variants:
///   0 = Mint(FederationId) — 32 bytes
///   1 = Note(SpendableNote) — denomination(1 byte u8) + keypair(32 bytes) + signature(48 bytes) = 81 bytes
fn parse_v2_ecash(bytes: &[u8]) -> Result<ParsedNotes, String> {
    let mut cursor = Cursor::new(bytes);

    let num_fields = read_varint(&mut cursor)?;
    if num_fields == 0 || num_fields > 100_000 {
        return Err(format!("Invalid v2 field count: {num_fields}"));
    }

    let mut federation_id: Option<[u8; 32]> = None;
    let mut denominations: BTreeMap<u64, usize> = BTreeMap::new();
    let mut raw_notes: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();

    for _ in 0..num_fields {
        let variant_idx = read_varint(&mut cursor)?;
        let body_len = read_varint(&mut cursor)?;
        if body_len > 10_000_000 {
            return Err("Body too large".to_string());
        }
        let mut body = vec![0u8; body_len as usize];
        cursor
            .read_exact(&mut body)
            .map_err(|e| format!("Read v2 error: {e}"))?;

        match variant_idx {
            0 => {
                // Mint(FederationId) — 32 bytes
                if body.len() >= 32 {
                    let mut fid = [0u8; 32];
                    fid.copy_from_slice(&body[..32]);
                    federation_id = Some(fid);
                }
            }
            1 => {
                // Note(SpendableNote) — denomination(u8=1) + keypair(32) + signature(48) = 81 bytes
                if body.len() >= 81 {
                    let denomination_byte = body[0];
                    let amount_msats = 1u64 << (denomination_byte as u64);
                    *denominations.entry(amount_msats).or_insert(0) += 1;
                    raw_notes
                        .entry(amount_msats)
                        .or_default()
                        .push(body.to_vec());
                }
            }
            _ => {} // skip unknown
        }
    }

    let federation_id = federation_id.ok_or("No federation ID in v2 ecash")?;
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&federation_id[..4]);

    if denominations.is_empty() {
        return Err("No notes in v2 ecash".to_string());
    }

    Ok(ParsedNotes {
        federation_id_prefix: prefix,
        denominations,
        raw_notes,
        format: NoteFormat::V2,
    })
}

// ─── Invite code parsing ────────────────────────────────────────

fn extract_federation_id_from_invite(body: &[u8]) -> Result<[u8; 32], String> {
    let mut cursor = Cursor::new(body);
    // peer_apis: Vec<(PeerId(u16 varint), SafeUrl(String))>
    let num_peers = read_varint(&mut cursor)?;
    for _ in 0..num_peers {
        let _peer_id = read_varint(&mut cursor)?;
        let url_len = read_varint(&mut cursor)?;
        let mut url_bytes = vec![0u8; url_len as usize];
        cursor
            .read_exact(&mut url_bytes)
            .map_err(|e| format!("Read: {e}"))?;
    }
    let mut fed_id = [0u8; 32];
    cursor
        .read_exact(&mut fed_id)
        .map_err(|e| format!("Read fed_id: {e}"))?;
    Ok(fed_id)
}

fn parse_invite_bytes(bytes: &[u8]) -> Result<[u8; 32], String> {
    let mut cursor = Cursor::new(bytes);
    let num_parts = read_varint(&mut cursor)?;

    for _ in 0..num_parts {
        let variant_idx = read_varint(&mut cursor)?;
        let body_len = read_varint(&mut cursor)?;
        let mut body = vec![0u8; body_len as usize];
        cursor
            .read_exact(&mut body)
            .map_err(|e| format!("Read: {e}"))?;

        match variant_idx {
            1 => {
                // FederationId: 32 bytes
                if body.len() >= 32 {
                    let mut fed_id = [0u8; 32];
                    fed_id.copy_from_slice(&body[..32]);
                    return Ok(fed_id);
                }
            }
            _ => {}
        }
    }

    Err("No FederationId found in invite code".to_string())
}

// ─── V1 Encoding ────────────────────────────────────────────────

fn encode_single_note_v1_oob(
    prefix: &FederationIdPrefix,
    denom_msats: u64,
    note_bytes: &[u8],
) -> Vec<u8> {
    let mut m = BTreeMap::new();
    m.insert(denom_msats, vec![note_bytes.to_vec()]);
    encode_multi_note_v1_oob(prefix, &m)
}

fn encode_multi_note_v1_oob(
    prefix: &FederationIdPrefix,
    notes: &BTreeMap<u64, Vec<Vec<u8>>>,
) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(&mut out, 2); // 2 parts

    // FederationIdPrefix (variant 1)
    write_varint(&mut out, 1);
    write_varint(&mut out, 4);
    out.extend_from_slice(prefix);

    // Notes (variant 0)
    write_varint(&mut out, 0);
    let mut notes_body = Vec::new();
    write_varint(&mut notes_body, notes.len() as u64);
    for (denom_msats, note_list) in notes {
        write_varint(&mut notes_body, *denom_msats);
        write_varint(&mut notes_body, note_list.len() as u64);
        for note_bytes in note_list {
            notes_body.extend_from_slice(note_bytes);
        }
    }
    write_varint(&mut out, notes_body.len() as u64);
    out.extend_from_slice(&notes_body);

    out
}

// ─── V2 Encoding ────────────────────────────────────────────────

fn encode_single_note_v2_ecash(
    fed_prefix: &FederationIdPrefix,
    note_bytes: &[u8],
) -> Vec<u8> {
    // We need the full 32-byte federation ID but only have 4-byte prefix.
    // We store the full note body which includes denomination, so we can reconstruct.
    // For v2 we need to store a dummy fed ID with matching prefix.
    let mut fed_id = [0u8; 32];
    fed_id[..4].copy_from_slice(fed_prefix);

    let mut out = Vec::new();
    write_varint(&mut out, 2); // 2 fields: Mint + Note

    // Mint (variant 0)
    write_varint(&mut out, 0);
    write_varint(&mut out, 32);
    out.extend_from_slice(&fed_id);

    // Note (variant 1)
    write_varint(&mut out, 1);
    write_varint(&mut out, note_bytes.len() as u64);
    out.extend_from_slice(note_bytes);

    out
}

fn encode_multi_note_v2_ecash(
    fed_prefix: &FederationIdPrefix,
    notes: &BTreeMap<u64, Vec<Vec<u8>>>,
) -> Vec<u8> {
    let mut fed_id = [0u8; 32];
    fed_id[..4].copy_from_slice(fed_prefix);

    let total_notes: usize = notes.values().map(|v| v.len()).sum();

    let mut out = Vec::new();
    write_varint(&mut out, 1 + total_notes as u64); // Mint + all Notes

    // Mint (variant 0)
    write_varint(&mut out, 0);
    write_varint(&mut out, 32);
    out.extend_from_slice(&fed_id);

    // Notes (variant 1 each)
    for note_list in notes.values() {
        for note_bytes in note_list {
            write_varint(&mut out, 1);
            write_varint(&mut out, note_bytes.len() as u64);
            out.extend_from_slice(note_bytes);
        }
    }

    out
}
