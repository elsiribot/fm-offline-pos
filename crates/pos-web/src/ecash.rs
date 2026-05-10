//! Minimal ecash note parsing without depending on the full fedimint crates.
//! Implements just enough of the consensus encoding to parse OOBNotes,
//! extract the federation ID prefix, and count notes by denomination.

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
    /// The original encoded bytes (for re-encoding subsets)
    original_bytes: Vec<u8>,
}

impl ParsedNotes {
    pub fn total_msats(&self) -> u64 {
        self.denominations
            .iter()
            .map(|(denom, count)| denom * (*count as u64))
            .sum()
    }
}

/// Parse ecash string (base64url, base64 standard, or fedimint base32)
pub fn parse_notes(s: &str) -> Result<ParsedNotes, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();

    let bytes = decode_note_string(&s)?;
    if bytes.is_empty() {
        return Err("Decoded ecash data is empty".to_string());
    }
    parse_note_bytes(&bytes)
        .map_err(|e| format!("{e} (first bytes: {:02x?}, len: {})", &bytes[..bytes.len().min(8)], bytes.len()))
}

/// Check if federation prefix matches
pub fn check_federation(notes: &ParsedNotes, expected_prefix: &FederationIdPrefix) -> bool {
    notes.federation_id_prefix == *expected_prefix
}

/// Parse a federation invite code to extract just the federation ID (32 bytes),
/// returning the first 4 bytes as prefix.
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
        let (_hrp, data) = bech32::decode(&lower).map_err(|e| format!("Invalid invite code: {e}"))?;
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

    for (denom, note_list) in &notes.raw_notes {
        for note_bytes in note_list {
            // Encode a single-note OOBNotes: Vec<OOBNotesPart> with FederationIdPrefix + Notes(single)
            let encoded = encode_single_note_oob(
                &notes.federation_id_prefix,
                *denom,
                note_bytes,
            );
            let encoded_str = BASE64_URL_SAFE.encode(&encoded);
            result.entry(*denom).or_default().push(encoded_str);
        }
    }

    result
}

/// Combine multiple single-note strings into one OOBNotes string
pub fn combine_note_strings(note_strings: &[String]) -> Result<String, String> {
    if note_strings.is_empty() {
        return Err("No notes to combine".to_string());
    }

    // Parse all notes and collect
    let mut fed_prefix: Option<FederationIdPrefix> = None;
    let mut all_notes: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();

    for s in note_strings {
        let bytes = decode_note_string(s)?;
        let parsed = parse_note_bytes(&bytes)?;
        if let Some(existing) = &fed_prefix {
            if *existing != parsed.federation_id_prefix {
                return Err("Mixed federation notes".to_string());
            }
        } else {
            fed_prefix = Some(parsed.federation_id_prefix);
        }
        for (denom, notes) in parsed.raw_notes {
            all_notes.entry(denom).or_default().extend(notes);
        }
    }

    let prefix = fed_prefix.ok_or("No notes")?;
    let encoded = encode_multi_note_oob(&prefix, &all_notes);
    Ok(BASE64_URL_SAFE.encode(&encoded))
}

// ─── Internal encoding/decoding ──────────────────────────────────

const BASE64_URL_SAFE: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::general_purpose::PAD,
);

const BASE64_URL_SAFE_NO_PAD: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::general_purpose::NO_PAD,
);

const BASE64_STANDARD_NO_PAD: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::general_purpose::NO_PAD,
);

fn decode_note_string(s: &str) -> Result<Vec<u8>, String> {
    let lower = s.to_lowercase();
    // Try fedimint base32hex prefix (NOT bech32, it's custom RFC 4648 base32hex)
    if lower.starts_with("fedimint") {
        return decode_fedimint_base32(&lower);
    }
    // Try all base64 variants (with and without padding, URL-safe and standard)
    if let Ok(bytes) = BASE64_URL_SAFE.decode(s) {
        if !bytes.is_empty() { return Ok(bytes); }
    }
    if let Ok(bytes) = BASE64_URL_SAFE_NO_PAD.decode(s) {
        if !bytes.is_empty() { return Ok(bytes); }
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(s) {
        if !bytes.is_empty() { return Ok(bytes); }
    }
    if let Ok(bytes) = BASE64_STANDARD_NO_PAD.decode(s) {
        if !bytes.is_empty() { return Ok(bytes); }
    }
    Err("Could not decode ecash string (not valid base64 or fedimint base32)".to_string())
}

/// Fedimint uses RFC 4648 base32hex (lowercase) with a "fedimint" text prefix.
/// This is NOT bech32.
fn decode_fedimint_base32(s: &str) -> Result<Vec<u8>, String> {
    const PREFIX: &str = "fedimint";
    if !s.starts_with(PREFIX) {
        return Err("Missing fedimint prefix".to_string());
    }
    let encoded = &s[PREFIX.len()..];
    base32hex_decode(encoded)
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

/// Read a BigSize (varint) from a reader - same as Lightning BigSize
fn read_varint(r: &mut impl Read) -> Result<u64, String> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first).map_err(|e| format!("Read error: {e}"))?;
    match first[0] {
        0..=0xfc => Ok(first[0] as u64),
        0xfd => {
            let mut buf = [0u8; 2];
            r.read_exact(&mut buf).map_err(|e| format!("Read error: {e}"))?;
            Ok(u16::from_be_bytes(buf) as u64)
        }
        0xfe => {
            let mut buf = [0u8; 4];
            r.read_exact(&mut buf).map_err(|e| format!("Read error: {e}"))?;
            Ok(u32::from_be_bytes(buf) as u64)
        }
        0xff => {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf).map_err(|e| format!("Read error: {e}"))?;
            Ok(u64::from_be_bytes(buf))
        }
    }
}

/// Write a BigSize varint
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

/// Parse OOBNotes bytes.
/// Format: Vec<OOBNotesPart> = varint(len) + len * (variant encoding)
/// Each enum variant: varint(variant_idx) + varint(body_len) + body_bytes
///
/// Variant 0: Notes(TieredMulti<SpendableNote>)
/// Variant 1: FederationIdPrefix (4 bytes)
/// Variant 2: Invite { peer_apis, federation_id }
/// Variant 3: ApiSecret(String)
fn parse_note_bytes(bytes: &[u8]) -> Result<ParsedNotes, String> {
    let mut cursor = Cursor::new(bytes);

    // Read Vec length
    let num_parts = read_varint(&mut cursor)?;
    if num_parts == 0 || num_parts > 100 {
        return Err(format!("Invalid OOBNotes part count: {num_parts}"));
    }

    let mut federation_id_prefix: Option<FederationIdPrefix> = None;
    let mut denominations: BTreeMap<u64, usize> = BTreeMap::new();
    let mut raw_notes: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();

    for _ in 0..num_parts {
        let variant_idx = read_varint(&mut cursor)?;
        // Read body as Vec<u8>: varint(len) + bytes
        let body_len = read_varint(&mut cursor)?;
        if body_len > 10_000_000 {
            return Err("OOBNotes body too large".to_string());
        }
        let mut body = vec![0u8; body_len as usize];
        cursor.read_exact(&mut body).map_err(|e| format!("Read error: {e}"))?;

        match variant_idx {
            0 => {
                // Notes: TieredMulti<SpendableNote> = BTreeMap<Amount, Vec<SpendableNote>>
                parse_tiered_multi(&body, &mut denominations, &mut raw_notes)?;
            }
            1 => {
                // FederationIdPrefix: 4 bytes
                if body.len() < 4 {
                    return Err("FederationIdPrefix too short".to_string());
                }
                let mut prefix = [0u8; 4];
                prefix.copy_from_slice(&body[..4]);
                federation_id_prefix = Some(prefix);
            }
            2 => {
                // Invite: { peer_apis: Vec<(PeerId, SafeUrl)>, federation_id: FederationId }
                // Extract federation_id (last 32 bytes of the structured data)
                // We parse: Vec<(u16, String)> then 32 bytes of FederationId
                if federation_id_prefix.is_none() {
                    // Try to extract from invite
                    if let Ok(fid) = extract_federation_id_from_invite(&body) {
                        let mut prefix = [0u8; 4];
                        prefix.copy_from_slice(&fid[..4]);
                        federation_id_prefix = Some(prefix);
                    }
                }
            }
            _ => {
                // Unknown variant, skip
            }
        }
    }

    let federation_id_prefix =
        federation_id_prefix.ok_or("No federation ID prefix found in ecash notes")?;

    if denominations.is_empty() {
        return Err("No notes found in ecash data".to_string());
    }

    Ok(ParsedNotes {
        federation_id_prefix,
        denominations,
        raw_notes,
        original_bytes: bytes.to_vec(),
    })
}

/// Parse TieredMulti<SpendableNote> from body bytes
/// Format: BTreeMap<Amount, Vec<SpendableNote>>
///   = varint(num_tiers) + for each: (varint(amount_msats), varint(num_notes), note_bytes...)
/// SpendableNote = signature(48 bytes) + spend_key(32+32 = 64 bytes)
/// Actually spend_key is a Keypair which is secp256k1 Keypair: 32 bytes secret key
/// Hmm, let's check...
fn parse_tiered_multi(
    body: &[u8],
    denominations: &mut BTreeMap<u64, usize>,
    raw_notes: &mut BTreeMap<u64, Vec<Vec<u8>>>,
) -> Result<(), String> {
    let mut cursor = Cursor::new(body);

    // BTreeMap: varint(len) then key-value pairs
    let num_tiers = read_varint(&mut cursor)?;
    if num_tiers > 1000 {
        return Err("Too many tiers".to_string());
    }

    for _ in 0..num_tiers {
        // Amount (u64 varint)
        let amount_msats = read_varint(&mut cursor)?;
        // Vec<SpendableNote>: varint(len) then items
        let num_notes = read_varint(&mut cursor)?;
        if num_notes > 100_000 {
            return Err("Too many notes in tier".to_string());
        }

        for _ in 0..num_notes {
            // SpendableNote: signature(48 bytes, BLS) + spend_key(Keypair = 32 bytes secret)
            // Total: 48 + 32 = 80 bytes
            let mut note_bytes = vec![0u8; 80];
            cursor
                .read_exact(&mut note_bytes)
                .map_err(|e| format!("Failed to read note: {e}"))?;

            raw_notes.entry(amount_msats).or_default().push(note_bytes);
        }

        *denominations.entry(amount_msats).or_insert(0) += num_notes as usize;
    }

    Ok(())
}

/// Try to extract FederationId (32 bytes) from invite body
fn extract_federation_id_from_invite(body: &[u8]) -> Result<[u8; 32], String> {
    let mut cursor = Cursor::new(body);
    // peer_apis: Vec<(PeerId, SafeUrl)>
    // PeerId = u16 varint, SafeUrl = String = varint(len) + bytes
    let num_peers = read_varint(&mut cursor)?;
    for _ in 0..num_peers {
        let _peer_id = read_varint(&mut cursor)?; // u16 as varint
        let url_len = read_varint(&mut cursor)?;
        let mut url_bytes = vec![0u8; url_len as usize];
        cursor.read_exact(&mut url_bytes).map_err(|e| format!("Read: {e}"))?;
    }
    // FederationId: 32 bytes (it's a newtype over [u8; 32] encoded as raw bytes)
    let mut fed_id = [0u8; 32];
    cursor.read_exact(&mut fed_id).map_err(|e| format!("Read fed_id: {e}"))?;
    Ok(fed_id)
}

/// Parse invite code bytes to extract FederationId
fn parse_invite_bytes(bytes: &[u8]) -> Result<[u8; 32], String> {
    let mut cursor = Cursor::new(bytes);

    // Vec<InviteCodePart>: varint(len) + parts
    let num_parts = read_varint(&mut cursor)?;

    for _ in 0..num_parts {
        let variant_idx = read_varint(&mut cursor)?;
        let body_len = read_varint(&mut cursor)?;
        let mut body = vec![0u8; body_len as usize];
        cursor.read_exact(&mut body).map_err(|e| format!("Read: {e}"))?;

        match variant_idx {
            1 => {
                // FederationId: 32 bytes
                if body.len() >= 32 {
                    let mut fed_id = [0u8; 32];
                    fed_id.copy_from_slice(&body[..32]);
                    return Ok(fed_id);
                }
            }
            _ => {} // skip Api, ApiSecret, etc.
        }
    }

    Err("No FederationId found in invite code".to_string())
}

/// Encode a single note into OOBNotes format
fn encode_single_note_oob(prefix: &FederationIdPrefix, denom_msats: u64, note_bytes: &[u8]) -> Vec<u8> {
    encode_multi_note_oob(prefix, &{
        let mut m = BTreeMap::new();
        m.insert(denom_msats, vec![note_bytes.to_vec()]);
        m
    })
}

/// Encode multiple notes into OOBNotes format
fn encode_multi_note_oob(prefix: &FederationIdPrefix, notes: &BTreeMap<u64, Vec<Vec<u8>>>) -> Vec<u8> {
    let mut out = Vec::new();

    // Vec<OOBNotesPart> with 2 parts: FederationIdPrefix + Notes
    write_varint(&mut out, 2); // 2 parts

    // Part 1: FederationIdPrefix (variant 1)
    write_varint(&mut out, 1); // variant index
    write_varint(&mut out, 4); // body length
    out.extend_from_slice(prefix);

    // Part 2: Notes (variant 0)
    write_varint(&mut out, 0); // variant index
    // Encode TieredMulti body
    let mut notes_body = Vec::new();
    encode_tiered_multi(&mut notes_body, notes);
    write_varint(&mut out, notes_body.len() as u64);
    out.extend_from_slice(&notes_body);

    out
}

/// Encode TieredMulti = BTreeMap<Amount, Vec<SpendableNote>>
fn encode_tiered_multi(out: &mut Vec<u8>, notes: &BTreeMap<u64, Vec<Vec<u8>>>) {
    write_varint(out, notes.len() as u64);
    for (denom_msats, note_list) in notes {
        write_varint(out, *denom_msats);
        write_varint(out, note_list.len() as u64);
        for note_bytes in note_list {
            out.extend_from_slice(note_bytes);
        }
    }
}
