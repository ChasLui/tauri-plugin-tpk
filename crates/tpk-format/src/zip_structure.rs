//! Bounded ZIP structure checks before zip's allocating, deduplicating parser.
//!
//! TPK uses a single disk, an empty comment and an EOF footer. Offsets are
//! absolute (no self-extracting prefix). Payload decoding stays with `zip`.
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::{is_allowed_entry_name, MAX_CONTAINER_ENTRIES};
use crate::error::{FormatError, Result};

/// Bounds names, extra fields and entry comments together, before allocation.
pub(super) const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
const EOCD: &[u8] = b"PK\x05\x06";
const EOCD64: &[u8] = b"PK\x06\x06";

fn invalid(message: impl Into<String>) -> FormatError {
    FormatError::Spec(message.into())
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn read_at<const N: usize>(file: &mut File, at: u64) -> Result<[u8; N]> {
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = [0; N];
    file.read_exact(&mut bytes)
        .map_err(|e| invalid(format!("truncated ZIP metadata: {e}")))?;
    Ok(bytes)
}

/// During construction, zip may retry older footers after a parse failure.
/// Only the checked footer may be parsed, and reads cannot leave the bounded
/// metadata region. After construction the same reader serves normal payloads.
pub(super) struct CheckedReader {
    file: File,
    start: u64,
    end: u64,
    footer: u64,
    footer64: Option<u64>,
    footer_selected: bool,
    checking: Arc<AtomicBool>,
}

pub(super) fn open(mut file: File) -> Result<zip::ZipArchive<CheckedReader>> {
    let end = file.seek(SeekFrom::End(0))?;
    // Search only the maximum legal comment window, requiring the footer to
    // end at EOF. Trailing junk and archive comments aren't TPK containers.
    let tail_start = end.saturating_sub(22 + u16::MAX as u64);
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = Vec::new();
    (&mut file).take(end - tail_start).read_to_end(&mut tail)?;
    let at = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| &tail[i..i + 4] == EOCD && i + 22 + u16_at(&tail, i + 20) as usize == tail.len())
        .ok_or_else(|| invalid("missing EOF ZIP central-directory footer"))?;
    let footer = tail_start + at as u64;
    let e = &tail[at..];
    if u16_at(e, 20) != 0 {
        return Err(invalid(
            "archive comment must be empty; it is not covered by the manifest",
        ));
    }
    if u16_at(e, 4) != 0 || u16_at(e, 6) != 0 || u16_at(e, 8) != u16_at(e, 10) {
        return Err(invalid("multi-disk ZIP is not a TPK container"));
    }
    let (mut count, mut size, mut start) = (
        u16_at(e, 10) as u64,
        u32_at(e, 12) as u64,
        u32_at(e, 16) as u64,
    );
    let mut directory_end = footer;
    let mut footer64 = None;
    let zip64_required = size == u32::MAX as u64 || start == u32::MAX as u64;
    let locator = if count == u16::MAX as u64 || zip64_required {
        let locator_at = footer
            .checked_sub(20)
            .ok_or_else(|| invalid("missing ZIP64 locator"))?;
        Some(read_at::<20>(&mut file, locator_at)?)
    } else {
        None
    };
    // Exactly 65535 entries fits ZIP32. Like zip, only interpret that count
    // as a ZIP64 sentinel when a locator is present; size/offset sentinels
    // always require ZIP64. A present but malformed locator still fails.
    if let Some(locator) = locator.filter(|l| zip64_required || &l[..4] == b"PK\x06\x07") {
        let locator_at = footer - 20;
        if &locator[..4] != b"PK\x06\x07" || u32_at(&locator, 4) != 0 || u32_at(&locator, 16) != 1 {
            return Err(invalid("invalid or multi-disk ZIP64 locator"));
        }
        let pos = u64_at(&locator, 8);
        let z = read_at::<56>(&mut file, pos)?;
        let record_len = u64_at(&z, 4)
            .checked_add(12)
            .ok_or_else(|| invalid("ZIP64 footer length overflow"))?;
        if &z[..4] != EOCD64 || record_len < 56 || pos.checked_add(record_len) != Some(locator_at) {
            return Err(invalid("invalid ZIP64 footer bounds"));
        }
        // zip also allocates the extensible data sector while finding a footer.
        if record_len > MAX_DIRECTORY_BYTES {
            return Err(invalid(
                "ZIP64 metadata exceeds central-directory byte budget",
            ));
        }
        if u32_at(&z, 16) != 0 || u32_at(&z, 20) != 0 || u64_at(&z, 24) != u64_at(&z, 32) {
            return Err(invalid("multi-disk ZIP64 is not a TPK container"));
        }
        count = u64_at(&z, 32);
        size = u64_at(&z, 40);
        start = u64_at(&z, 48);
        directory_end = pos;
        footer64 = Some(pos);
        if size
            .checked_add(record_len)
            .is_none_or(|n| n > MAX_DIRECTORY_BYTES)
        {
            return Err(invalid(
                "ZIP64 metadata exceeds central-directory byte budget",
            ));
        }
    }
    if count > MAX_CONTAINER_ENTRIES as u64 {
        return Err(invalid(format!(
            "{count} entries exceeds the {MAX_CONTAINER_ENTRIES} limit"
        )));
    }
    if size > MAX_DIRECTORY_BYTES {
        return Err(invalid(format!(
            "central-directory bytes exceeds the {MAX_DIRECTORY_BYTES} limit"
        )));
    }
    if start.checked_add(size) != Some(directory_end) {
        return Err(invalid("invalid central-directory bounds"));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut directory = (&mut file).take(size);
    let mut consumed = 0u64;
    let mut names = HashSet::new();
    let mut actual = 0usize;
    while consumed < size {
        actual += 1;
        if actual > MAX_CONTAINER_ENTRIES {
            return Err(invalid(format!(
                "{actual} entries exceeds the {MAX_CONTAINER_ENTRIES} limit"
            )));
        }
        let mut header = [0; 46];
        directory
            .read_exact(&mut header)
            .map_err(|_| invalid("truncated central-directory entry"))?;
        if &header[..4] != b"PK\x01\x02" {
            return Err(invalid("invalid central-directory entry signature"));
        }
        let name_len = u16_at(&header, 28) as usize;
        let extra_len = u16_at(&header, 30) as usize;
        let comment_len = u16_at(&header, 32) as usize;
        consumed += 46 + (name_len + extra_len + comment_len) as u64;
        if consumed > size {
            return Err(invalid("central-directory entry exceeds declared bounds"));
        }
        let mut name = vec![0; name_len];
        directory.read_exact(&mut name)?;
        let name = std::str::from_utf8(&name).map_err(|_| invalid("non-UTF-8 container entry"))?;
        if !is_allowed_entry_name(name) {
            return Err(invalid(format!("illegal container entry {name:?}")));
        }
        if !names.insert(name.to_owned()) {
            return Err(invalid(format!("duplicate container entry {name:?}")));
        }
        let mut extra = vec![0; extra_len];
        directory.read_exact(&mut extra)?;
        // zip applies Unicode Path overrides before deduplication. Require any
        // such field to preserve the ASCII whitelist name, even with bad CRC.
        let mut remaining = extra.as_slice();
        while !remaining.is_empty() {
            if remaining.len() < 4 {
                return Err(invalid("truncated ZIP extra field"));
            }
            let id = u16_at(remaining, 0);
            let len = u16_at(remaining, 2) as usize;
            if len + 4 > remaining.len() {
                return Err(invalid("ZIP extra field exceeds entry bounds"));
            }
            if id == 0x7075 && (len < 5 || &remaining[9..4 + len] != name.as_bytes()) {
                return Err(invalid(
                    "Unicode Path extra field changes container entry name",
                ));
            }
            remaining = &remaining[4 + len..];
        }
        io::copy(
            &mut (&mut directory).take(comment_len as u64),
            &mut io::sink(),
        )?;
    }
    if actual as u64 != count {
        return Err(invalid(
            "central-directory entry count does not match footer",
        ));
    }
    let checking = Arc::new(AtomicBool::new(true));
    let reader = CheckedReader {
        file,
        start,
        end,
        footer,
        footer64,
        footer_selected: false,
        checking: checking.clone(),
    };
    let archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        reader,
    )
    .map_err(|e| invalid(format!("not a readable ZIP: {e}")))?;
    checking.store(false, Ordering::Relaxed);
    Ok(archive)
}

impl Read for CheckedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if !self.checking.load(Ordering::Relaxed) {
            return self.file.read(bytes);
        }
        let pos = self.file.stream_position()?;
        if pos < self.start {
            // The footer finder reads 1024-byte windows. Hide payload bytes so
            // their magic cannot influence metadata parsing or fallback.
            let len = bytes.len().min((self.start - pos) as usize);
            bytes[..len].fill(0);
            self.file.seek(SeekFrom::Start(pos + len as u64))?;
            return Ok(len);
        }
        let len = bytes.len().min(self.end.saturating_sub(pos) as usize);
        self.file.read(&mut bytes[..len])
    }
}
impl Seek for CheckedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = self.file.seek(from)?;
        if self.checking.load(Ordering::Relaxed) {
            if pos < self.start.saturating_sub(1024) || pos > self.end {
                return Err(io::Error::other("ZIP parser left checked metadata bounds"));
            }
            // MagicFinder first seeks to a scan window, which may start on
            // footer magic inside a legal extra field or entry comment. The
            // checked EOF footer is the first complete candidate (any magic
            // within its 22 bytes is too close to EOF to parse). Arm the
            // fallback guard only after its selection; entry parsing
            // reads extras/comments sequentially and seeks between headers.
            if matches!(from, SeekFrom::Start(at) if at == self.footer) {
                self.footer_selected = true;
            }
            if self.footer_selected && pos >= self.start && pos + 4 <= self.end {
                let mut magic = [0; 4];
                self.file.read_exact(&mut magic)?;
                self.file.seek(SeekFrom::Start(pos))?;
                if (magic == EOCD && pos != self.footer)
                    || (magic == EOCD64 && Some(pos) != self.footer64)
                {
                    return Err(io::Error::other(
                        "ZIP parser attempted an unchecked alternate footer",
                    ));
                }
            }
        }
        Ok(pos)
    }
}
