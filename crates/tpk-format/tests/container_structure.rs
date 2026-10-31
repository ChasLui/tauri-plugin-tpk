//! Raw fixtures bypass ZipWriter's duplicate-name protection.
use tpk_format::container::{UnverifiedPack, MANIFEST_NAME, MAX_CONTAINER_ENTRIES};
use tpk_format::error::FormatError;

fn put16(out: &mut Vec<u8>, n: u16) {
    out.extend(n.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}
fn put64(out: &mut Vec<u8>, n: u64) {
    out.extend(n.to_le_bytes());
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
fn footer(out: &mut Vec<u8>, count: u16, size: u32, start: u32) {
    out.extend(b"PK\x05\x06");
    put16(out, 0);
    put16(out, 0);
    put16(out, count);
    put16(out, count);
    put32(out, size);
    put32(out, start);
    put16(out, 0);
}
fn raw_zip(names: &[&str], zip64: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for &name in names {
        let offset = out.len() as u32;
        let content = b"{}";
        out.extend(b"PK\x03\x04");
        put16(&mut out, 20);
        put16(&mut out, 0x800);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put32(&mut out, crc32(content));
        put32(&mut out, 2);
        put32(&mut out, 2);
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0);
        out.extend(name.as_bytes());
        out.extend(content);
        central.extend(b"PK\x01\x02");
        put16(&mut central, 20);
        put16(&mut central, 20);
        put16(&mut central, 0x800);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, crc32(content));
        put32(&mut central, 2);
        put32(&mut central, 2);
        put16(&mut central, name.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, offset);
        central.extend(name.as_bytes());
    }
    let start = out.len() as u32;
    let size = central.len() as u32;
    out.extend(central);
    if zip64 {
        let zip64_start = out.len() as u64;
        out.extend(b"PK\x06\x06");
        put64(&mut out, 44);
        put16(&mut out, 45);
        put16(&mut out, 45);
        put32(&mut out, 0);
        put32(&mut out, 0);
        put64(&mut out, names.len() as u64);
        put64(&mut out, names.len() as u64);
        put64(&mut out, size as u64);
        put64(&mut out, start as u64);
        out.extend(b"PK\x06\x07");
        put32(&mut out, 0);
        put64(&mut out, zip64_start);
        put32(&mut out, 1);
        footer(&mut out, u16::MAX, u32::MAX, u32::MAX);
    } else {
        footer(&mut out, names.len() as u16, size, start);
    }
    out
}
fn open(bytes: &[u8]) -> Result<UnverifiedPack, FormatError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("structure.tpk");
    std::fs::write(&path, bytes).unwrap();
    UnverifiedPack::open(&path)
}
fn rejection(bytes: &[u8], expected: &str) {
    let err = open(bytes).unwrap_err();
    assert!(matches!(err, FormatError::Spec(_)), "{err}");
    assert!(err.to_string().contains(expected), "{err}");
}
#[test]
fn duplicate_manifest_is_rejected_before_zip_deduplication() {
    let bytes = raw_zip(&[MANIFEST_NAME, MANIFEST_NAME], false);
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .unwrap()
            .len(),
        1
    );
    rejection(&bytes, "duplicate container entry");
}
#[test]
fn duplicate_blob_is_rejected_before_zip_deduplication() {
    let blob = format!("blobs/{}", "a".repeat(64));
    let bytes = raw_zip(&[MANIFEST_NAME, &blob, &blob], false);
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .unwrap()
            .len(),
        2
    );
    rejection(&bytes, "duplicate container entry");
}
#[test]
fn zip64_raw_entry_count_is_checked_before_duplicate_names() {
    let bytes = raw_zip(&vec![MANIFEST_NAME; MAX_CONTAINER_ENTRIES + 1], true);
    rejection(&bytes, "65537 entries exceeds the 65536 limit");
}
#[test]
fn actual_entries_must_match_footer_count() {
    let mut bytes = raw_zip(&[MANIFEST_NAME, MANIFEST_NAME], false);
    let end = bytes.len() - 22;
    bytes[end + 8..end + 12].copy_from_slice(&[1, 0, 1, 0]);
    rejection(&bytes, "duplicate container entry");
    let mut bytes = raw_zip(&[MANIFEST_NAME], false);
    let end = bytes.len() - 22;
    bytes[end + 8..end + 12].copy_from_slice(&[2, 0, 2, 0]);
    rejection(&bytes, "entry count does not match footer");
}
#[test]
fn valid_zip64_footer_reads_the_manifest() {
    let bytes = raw_zip(&[MANIFEST_NAME], true);
    assert_eq!(open(&bytes).unwrap().raw_manifest_bytes(), b"{}");
}
#[test]
fn central_directory_budget_is_checked_before_parsing_entries() {
    let mut bytes = raw_zip(&[MANIFEST_NAME], false);
    let end = bytes.len() - 22;
    bytes[end + 12..end + 16].copy_from_slice(&(16u32 * 1024 * 1024 + 1).to_le_bytes());
    rejection(&bytes, "central-directory bytes exceeds");
}
#[test]
fn alternate_footer_in_payload_does_not_affect_valid_pack() {
    let mut bytes = Vec::new();
    footer(&mut bytes, u16::MAX, u32::MAX, 0);
    bytes.extend(raw_zip(&[MANIFEST_NAME], false));
    let end = bytes.len() - 22;
    let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize + 22;
    bytes[end + 16..end + 20].copy_from_slice(&(start as u32).to_le_bytes());
    bytes[start + 42..start + 46].copy_from_slice(&22u32.to_le_bytes());
    assert_eq!(open(&bytes).unwrap().raw_manifest_bytes(), b"{}");
}
#[test]
fn footer_magic_at_initial_scan_window_in_legal_extra_is_allowed() {
    let mut bytes = raw_zip(&[MANIFEST_NAME], false);
    let end = bytes.len() - 22;
    let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    bytes.truncate(end);
    // Unknown TLVs are legal. Place EOCD magic exactly where MagicFinder
    // initially seeks to read its 1024-byte window, before selecting a footer.
    bytes[start + 30..start + 32].copy_from_slice(&1028u16.to_le_bytes());
    let mut extra = Vec::new();
    put16(&mut extra, 0xCAFE);
    put16(&mut extra, 1024);
    let mut value = [0; 1024];
    value[22..26].copy_from_slice(b"PK\x05\x06");
    extra.extend(value);
    bytes.extend(extra);
    let size = bytes.len() - start;
    assert_eq!(MANIFEST_NAME.len(), 17);
    assert_eq!(size, 1091);
    footer(&mut bytes, 1, size as u32, start as u32);
    assert_eq!(bytes.len() - start, 1113);
    assert_eq!(bytes.len() - 1024, start + 89);
    assert_eq!(&bytes[bytes.len() - 1024..][..4], b"PK\x05\x06");
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(open(&bytes).unwrap().raw_manifest_bytes(), b"{}");
}
#[test]
fn malformed_directory_cannot_fall_back_to_unchecked_footer() {
    let mut bytes = raw_zip(&[MANIFEST_NAME], false);
    let end = bytes.len() - 22;
    let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let alternate_directory = bytes[start..end].to_vec();
    bytes.truncate(end);
    // The TLV is bounded but contains a truncated ZIP64 size. An otherwise
    // valid directory inside its comment must not rescue the malformed one.
    bytes[start + 20..start + 24].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[start + 30..start + 32].copy_from_slice(&5u16.to_le_bytes());
    bytes[start + 32..start + 34]
        .copy_from_slice(&((alternate_directory.len() + 22) as u16).to_le_bytes());
    bytes.extend([1, 0, 1, 0, 0]);
    let alternate_start = bytes.len() as u32;
    bytes.extend(alternate_directory);
    footer(&mut bytes, 1, u32::MAX - 1, alternate_start);
    let size = (bytes.len() - start) as u32;
    footer(&mut bytes, 1, size, start as u32);
    // Check our rejection regardless of whether zip itself attempts recovery;
    // newer releases reject the truncated ZIP64 field without falling back.
    rejection(&bytes, "not a readable ZIP");
}

#[test]
fn maximum_zip32_entry_count_does_not_require_zip64_locator() {
    let blobs: Vec<String> = (0..u16::MAX as usize - 1)
        .map(|i| format!("blobs/{i:064x}"))
        .collect();
    let mut names = vec![MANIFEST_NAME];
    names.extend(blobs.iter().map(String::as_str));
    let bytes = raw_zip(&names, false);
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .unwrap()
            .len(),
        u16::MAX as usize
    );
    assert_eq!(open(&bytes).unwrap().raw_manifest_bytes(), b"{}");
}

#[test]
fn actual_raw_count_is_bounded_even_when_zip64_footer_underreports() {
    let blobs: Vec<String> = (0..MAX_CONTAINER_ENTRIES)
        .map(|i| format!("blobs/{i:064x}"))
        .collect();
    let mut names = vec![MANIFEST_NAME];
    names.extend(blobs.iter().map(String::as_str));
    let mut bytes = raw_zip(&names, true);
    let zip64 = bytes.len() - 22 - 20 - 56;
    bytes[zip64 + 24..zip64 + 32].copy_from_slice(&1u64.to_le_bytes());
    bytes[zip64 + 32..zip64 + 40].copy_from_slice(&1u64.to_le_bytes());
    rejection(&bytes, "65537 entries exceeds the 65536 limit");
    // Exactly the ceiling is valid, including a real ZIP64 footer.
    let bytes = raw_zip(&names[..MAX_CONTAINER_ENTRIES], true);
    assert_eq!(open(&bytes).unwrap().raw_manifest_bytes(), b"{}");
}

#[test]
fn unicode_path_extra_cannot_hide_a_duplicate_name() {
    let blob = format!("blobs/{}", "a".repeat(64));
    let mut bytes = raw_zip(&[MANIFEST_NAME, &blob], false);
    let end = bytes.len() - 22;
    let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let second = start + 46 + MANIFEST_NAME.len();
    bytes.truncate(end);
    let mut extra = Vec::new();
    put16(&mut extra, 0x7075);
    put16(&mut extra, (5 + MANIFEST_NAME.len()) as u16);
    extra.push(1);
    put32(&mut extra, crc32(blob.as_bytes()));
    extra.extend(MANIFEST_NAME.as_bytes());
    bytes[second + 30..second + 32].copy_from_slice(&(extra.len() as u16).to_le_bytes());
    bytes.extend(extra);
    let size = (bytes.len() - start) as u32;
    footer(&mut bytes, 2, size, start as u32);
    assert_eq!(
        zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .unwrap()
            .len(),
        1
    );
    rejection(&bytes, "Unicode Path extra field changes");
}
