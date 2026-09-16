//! HUFF/CDIC ("huffdic") decompression for Kindle PalmDB containers.
//!
//! `kindlegen -c2`, and most Amazon-published AZW3/KF8 files, compress text
//! records with a static Huffman code over a shared phrase dictionary instead
//! of the PalmDOC LZ77 used by compression type 2. The PalmDOC header spells
//! that choice `0x4448` (`17480`), which is why some tools print it as "DH".
//!
//! `ebook-rs` refuses those files outright, so the adapter decodes the text
//! itself and hands the parser an equivalent, uncompressed container. Nothing
//! in this module leaves `src/formats/`: it produces plain bytes for the
//! project's own model, never a third-party type.
//!
//! The model lives in two record kinds that sit right after the text records
//! and are located through the MOBI header (`huff_rec_index` at MOBI offset
//! `0x60`, `huff_rec_count` at `0x64`):
//!
//! * one `HUFF` record with the code tables — a 256-entry table indexed by the
//!   top 8 bits of the code window, plus per-code-length `mincode`/`maxcode`
//!   bounds for codes too long to resolve from those 8 bits;
//! * `huff_rec_count - 1` `CDIC` records holding the phrase dictionary. A
//!   phrase is usually stored literally, but may itself be a compressed
//!   bitstream (the `0x8000` flag clear), in which case it expands through the
//!   same decoder and is memoized.
//!
//! Codes are read most-significant-bit first out of a 32-bit window carried in
//! a 64-bit accumulator. A code's dictionary index is `maxcode - code` for its
//! length, so symbols run *backwards* through each length's code range; the
//! tables are stored pre-shifted (`(maxcode + 1) << (32 - codelen)`) so the
//! decoder never normalizes at run time. Those bounds do not fit in 32 bits at
//! short code lengths, hence the `u64` arithmetic throughout.
//!
//! Cross-checked against KindleUnpack's `HuffcdicReader`, calibre's
//! `ebooks/mobi/huffcdic.py` and the `kindling-mobi` crate, which agree.

use anyhow::{Result, bail};

/// PalmDOC header compression value that selects HUFF/CDIC.
pub(crate) const COMPRESSION_HUFFCDIC: u16 = 17_480;

/// Longest chain of compressed phrases that may expand into each other. Real
/// files nest one level deep at most; the cap keeps a corrupt dictionary from
/// recursing away.
const MAX_PHRASE_DEPTH: u32 = 8;

/// Ceiling on the bytes one text record may produce. A record decodes to
/// `text_record_size` (4096, or 8192/16384 for unusually large books), so this
/// is orders of magnitude of headroom; it only stops a decompression bomb.
const MAX_RECORD_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// The compressed body of a text record: everything before its trailing data
/// regions.
///
/// `extra_record_flags` names those regions (bits 15..1 each add one, sized by
/// a big-endian varint at its own end; bit 0 adds the multibyte overlap byte,
/// which comes off last). They sit outside the compressed stream whatever the
/// compression is, so they are removed *before* decoding rather than after —
/// otherwise their bits decode into stray symbols.
pub(crate) fn text_body_len(record: &[u8], extra_record_flags: u32) -> usize {
    let mut end = record.len();
    for bit in (1..16).rev() {
        if extra_record_flags & (1 << bit) == 0 || end == 0 {
            continue;
        }
        // Most significant byte first, and the high-bit marker sits on the
        // *first* byte: `81 20` is 160, not 4097.
        let window = &record[end.saturating_sub(4)..end];
        let mut size = 0usize;
        for byte in window {
            if byte & 0x80 != 0 {
                size = 0;
            }
            size = (size << 7) | usize::from(byte & 0x7F);
        }
        end -= size.min(end);
    }
    if extra_record_flags & 1 != 0 && end > 0 {
        end -= (usize::from(record[end - 1] & 3) + 1).min(end);
    }
    end
}

/// One entry of the 256-entry `HUFF` lookup table, keyed by the top 8 bits of
/// the code window.
#[derive(Clone, Copy)]
struct CodeEntry {
    /// For a terminal entry the code's length; otherwise a lower bound to start
    /// the code-length walk from.
    codelen: u8,
    /// Set when those 8 bits already identify the code, so no walk is needed.
    terminal: bool,
    /// Pre-shifted upper bound, only meaningful when `terminal` is set.
    maxcode: u64,
}

/// A phrase dictionary slot. Compressed slots expand on first use and are then
/// replaced by their expansion.
enum Phrase {
    Plain(Vec<u8>),
    Packed(Vec<u8>),
    /// Currently expanding: seeing this again means the file references itself.
    Expanding,
}

/// A loaded huffdic model: the code tables plus the phrase dictionary.
pub(crate) struct Huffcdic {
    table: Vec<CodeEntry>,
    mincode: [u64; 33],
    maxcode: [u64; 33],
    phrases: Vec<Phrase>,
}

/// Summarizes rather than dumps: the code tables are 289 numbers nobody reads.
impl std::fmt::Debug for Huffcdic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Huffcdic")
            .field("phrases", &self.phrases.len())
            .finish_non_exhaustive()
    }
}

impl Huffcdic {
    /// Parses a `HUFF` record followed by its `CDIC` records.
    pub(crate) fn load(huff: &[u8], cdics: &[&[u8]]) -> Result<Self> {
        if huff.len() < 24 || &huff[..4] != b"HUFF" {
            bail!(
                "Kindle HUFF record does not start with a HUFF signature (found {:?})",
                String::from_utf8_lossy(&huff[..huff.len().min(4)])
            );
        }
        let off1 = read_u32(huff, 8).unwrap_or(0) as usize;
        let off2 = read_u32(huff, 12).unwrap_or(0) as usize;
        if off1 + 256 * 4 > huff.len() {
            bail!(
                "Kindle HUFF record is truncated: the 256-entry table at offset {off1} needs \
                 {} bytes but the record is {}",
                256 * 4,
                huff.len()
            );
        }
        if off2 + 64 * 4 > huff.len() {
            bail!(
                "Kindle HUFF record is truncated: the code-length table at offset {off2} needs \
                 256 bytes but the record is {}",
                huff.len()
            );
        }

        let mut table = Vec::with_capacity(256);
        for index in 0..256 {
            let value = read_u32(huff, off1 + index * 4).unwrap_or(0);
            let codelen = (value & 0x1F) as u8;
            let terminal = value & 0x80 != 0;
            if codelen == 0 {
                bail!("Kindle HUFF code table entry {index} declares a zero code length");
            }
            if codelen <= 8 && !terminal {
                // A code of 8 bits or fewer is fully determined by the 8 bits
                // that index this table, so it must say so. Without the flag the
                // code-length walk would have no bound to converge on.
                bail!(
                    "Kindle HUFF code table entry {index} has code length {codelen} but is not \
                     marked terminal"
                );
            }
            // Stored pre-shifted; widen before shifting because the result
            // overruns 32 bits for short codes.
            let maxcode = ((u64::from(value >> 8) + 1) << (32 - u32::from(codelen))) - 1;
            table.push(CodeEntry {
                codelen,
                terminal,
                maxcode,
            });
        }

        let mut mincode = [0_u64; 33];
        let mut maxcode = [0_u64; 33];
        mincode[0] = 0;
        maxcode[0] = (1_u64 << 32) - 1;
        for len in 1..=32_usize {
            let shift = 32 - len as u32;
            let lo = u64::from(read_u32(huff, off2 + (len - 1) * 8).unwrap_or(0));
            let hi = u64::from(read_u32(huff, off2 + (len - 1) * 8 + 4).unwrap_or(0));
            mincode[len] = lo << shift;
            maxcode[len] = ((hi + 1) << shift) - 1;
        }

        let mut model = Huffcdic {
            table,
            mincode,
            maxcode,
            phrases: Vec::new(),
        };
        for (position, cdic) in cdics.iter().enumerate() {
            model.push_cdic(cdic, position)?;
        }
        if model.phrases.is_empty() {
            bail!("Kindle HUFF record has no CDIC phrase dictionary behind it");
        }
        Ok(model)
    }

    /// How many phrases the dictionary holds.
    pub(crate) fn phrase_count(&self) -> usize {
        self.phrases.len()
    }

    /// Appends the phrases held by one `CDIC` record.
    fn push_cdic(&mut self, cdic: &[u8], position: usize) -> Result<()> {
        if cdic.len() < 16 || &cdic[..4] != b"CDIC" {
            bail!(
                "Kindle CDIC record {position} does not start with a CDIC signature (found {:?})",
                String::from_utf8_lossy(&cdic[..cdic.len().min(4)])
            );
        }
        let declared = read_u32(cdic, 8).unwrap_or(0) as usize;
        let bits = read_u32(cdic, 12).unwrap_or(0);
        if bits > 16 {
            bail!("Kindle CDIC record {position} declares an implausible {bits} index bits");
        }
        // Each record holds up to `1 << bits` phrases and the last one is short.
        let already = self.phrases.len();
        let remaining = declared.saturating_sub(already);
        let count = remaining.min(1_usize << bits);
        // The offset table starts at 0x10 and its entries are relative to it.
        if 16 + count * 2 > cdic.len() {
            bail!(
                "Kindle CDIC record {position} is truncated: an offset table for {count} phrases \
                 needs {} bytes but the record is {}",
                16 + count * 2,
                cdic.len()
            );
        }
        for index in 0..count {
            let offset = read_u16(cdic, 16 + index * 2).unwrap_or(0) as usize;
            let length_at = 16 + offset;
            let tagged = read_u16(cdic, length_at).unwrap_or(0) as usize;
            let start = length_at + 2;
            let end = start + (tagged & 0x7FFF);
            if end > cdic.len() {
                bail!(
                    "Kindle CDIC record {position} phrase {index} runs to offset {end} but the \
                     record is {}",
                    cdic.len()
                );
            }
            let bytes = cdic[start..end].to_vec();
            self.phrases.push(if tagged & 0x8000 != 0 {
                Phrase::Plain(bytes)
            } else {
                Phrase::Packed(bytes)
            });
        }
        Ok(())
    }

    /// Decodes one text record that already had its trailing data removed.
    pub(crate) fn decompress(&mut self, record: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(record.len().saturating_mul(4));
        self.decode_into(record, &mut out, 0)?;
        Ok(out)
    }

    fn decode_into(&mut self, data: &[u8], out: &mut Vec<u8>, depth: u32) -> Result<()> {
        // The reader always keeps a full 64-bit window in hand, so pad past the
        // end. The padding bits are never emitted because `bitsleft` runs out
        // first.
        let mut buf = Vec::with_capacity(data.len() + 12);
        buf.extend_from_slice(data);
        buf.resize(data.len() + 12, 0);

        let mut bitsleft = data.len() as i64 * 8;
        let mut pos = 0_usize;
        let mut window = u64::from_be_bytes(buf[0..8].try_into().expect("8-byte window"));
        // Bits of `window` still unread, counted down from the top 32.
        let mut available: i32 = 32;

        loop {
            if available <= 0 {
                pos += 4;
                if pos + 8 > buf.len() {
                    buf.resize(pos + 8, 0);
                }
                window = u64::from_be_bytes(buf[pos..pos + 8].try_into().expect("8-byte window"));
                available += 32;
            }
            let code = (window >> available) & 0xFFFF_FFFF;

            let entry = self.table[(code >> 24) as usize];
            let mut codelen = usize::from(entry.codelen);
            let mut maxcode = entry.maxcode;
            if !entry.terminal {
                while codelen <= 32 && code < self.mincode[codelen] {
                    codelen += 1;
                }
                if codelen > 32 {
                    bail!("Kindle HUFF code {code:#010x} resolves to more than 32 bits");
                }
                maxcode = self.maxcode[codelen];
            }

            available -= codelen as i32;
            bitsleft -= codelen as i64;
            if bitsleft < 0 {
                break;
            }

            let span = maxcode.checked_sub(code).ok_or_else(|| {
                anyhow::anyhow!(
                    "Kindle HUFF code {code:#010x} is above the maximum for its {codelen}-bit length"
                )
            })?;
            let index = (span >> (32 - codelen as u32)) as usize;
            self.append_phrase(index, out, depth)?;
            if out.len() > MAX_RECORD_OUTPUT_BYTES {
                bail!(
                    "Kindle HUFF record decompressed past the {} byte record ceiling",
                    MAX_RECORD_OUTPUT_BYTES
                );
            }
        }
        Ok(())
    }

    /// Appends phrase `index`, expanding and memoizing it when it is stored
    /// compressed.
    fn append_phrase(&mut self, index: usize, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        // Replace the slot before recursing so no borrow is held across the
        // call and a self-reference becomes visible as `Expanding`.
        let phrase_count = self.phrases.len();
        let packed = match self.phrases.get_mut(index) {
            None => bail!(
                "Kindle HUFF code resolved to phrase {index} but the dictionary holds \
                 {phrase_count}"
            ),
            Some(Phrase::Expanding) => {
                bail!("Kindle HUFF phrase {index} expands to itself")
            }
            Some(Phrase::Plain(bytes)) => {
                out.extend_from_slice(bytes);
                return Ok(());
            }
            Some(slot) => match std::mem::replace(slot, Phrase::Expanding) {
                Phrase::Packed(bytes) => bytes,
                _ => unreachable!("the slot was just matched as Packed"),
            },
        };

        if depth >= MAX_PHRASE_DEPTH {
            self.phrases[index] = Phrase::Packed(packed);
            bail!("Kindle HUFF phrase {index} nests deeper than {MAX_PHRASE_DEPTH} levels");
        }

        let mut expanded = Vec::new();
        match self.decode_into(&packed, &mut expanded, depth + 1) {
            Ok(()) => {
                out.extend_from_slice(&expanded);
                self.phrases[index] = Phrase::Plain(expanded);
                Ok(())
            }
            Err(error) => {
                // Restore the slot so a later record reports the same failure
                // rather than a spurious cycle.
                self.phrases[index] = Phrase::Packed(packed);
                Err(error)
            }
        }
    }
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
    ]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
        *data.get(offset + 2)?,
        *data.get(offset + 3)?,
    ]))
}

/// A model whose every `dict1` entry is one terminal 8-bit code, so the top 8
/// bits of the window *are* the code: byte `0xFF` resolves phrase 0, `0xFE`
/// phrase 1, and so on downwards.
#[cfg(test)]
pub(crate) fn tiny_huff() -> Vec<u8> {
    let mut huff = Vec::from(*b"HUFF\x00\x00\x00\x18");
    huff.extend_from_slice(&24_u32.to_be_bytes());
    huff.extend_from_slice(&(24_u32 + 1024).to_be_bytes());
    huff.resize(24, 0);
    let entry: u32 = (255 << 8) | 0x80 | 8;
    for _ in 0..256 {
        huff.extend_from_slice(&entry.to_be_bytes());
    }
    // dict2 is never consulted while every entry is terminal.
    huff.resize(24 + 1024 + 256, 0);
    huff
}

/// One `CDIC` holding `phrases`, each `(bytes, stored_literally)`.
#[cfg(test)]
pub(crate) fn tiny_cdic(phrases: &[(&[u8], bool)]) -> Vec<u8> {
    let count = phrases.len();
    let mut offsets = Vec::with_capacity(count);
    let mut body: Vec<u8> = Vec::new();
    for (data, literal) in phrases {
        offsets.push((2 * count + body.len()) as u16);
        let tagged = data.len() as u16 | if *literal { 0x8000 } else { 0 };
        body.extend_from_slice(&tagged.to_be_bytes());
        body.extend_from_slice(data);
    }
    let mut record = Vec::from(*b"CDIC\x00\x00\x00\x10");
    record.extend_from_slice(&(count as u32).to_be_bytes());
    record.extend_from_slice(&8_u32.to_be_bytes());
    for offset in offsets {
        record.extend_from_slice(&offset.to_be_bytes());
    }
    record.extend_from_slice(&body);
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_model(phrases: &[(&[u8], bool)]) -> Huffcdic {
        let huff = tiny_huff();
        let cdic = tiny_cdic(phrases);
        Huffcdic::load(&huff, &[&cdic]).expect("the tiny model loads")
    }

    #[test]
    fn decodes_plain_phrases() {
        let mut model = tiny_model(&[(b"hello ", true), (b"world", true)]);
        assert_eq!(model.decompress(&[0xFE, 0xFF]).unwrap(), b"worldhello ");
        assert_eq!(model.phrase_count(), 2);
    }

    #[test]
    fn expands_a_packed_phrase_and_memoizes_it() {
        let mut model = tiny_model(&[(&[0xFE], false), (b"x", true)]);
        assert_eq!(model.decompress(&[0xFF]).unwrap(), b"x");
        // The slot is now plain, so the same code resolves without recursion.
        assert_eq!(model.decompress(&[0xFF]).unwrap(), b"x");
    }

    #[test]
    fn a_self_referential_phrase_is_reported_not_recursed() {
        let mut model = tiny_model(&[(&[0xFF], false), (b"x", true)]);
        assert_eq!(model.decompress(&[0xFE]).unwrap(), b"x");
        let error = model.decompress(&[0xFF]).unwrap_err();
        assert!(error.to_string().contains("expands to itself"), "{error}");
        // The slot is restored, so a later record gets the same answer.
        let error = model.decompress(&[0xFF]).unwrap_err();
        assert!(error.to_string().contains("expands to itself"), "{error}");
    }

    #[test]
    fn a_phrase_index_past_the_dictionary_is_an_error() {
        let mut model = tiny_model(&[(b"a", true), (b"b", true)]);
        let error = model.decompress(&[0x00]).unwrap_err();
        assert!(error.to_string().contains("dictionary holds 2"), "{error}");
    }

    #[test]
    fn an_empty_record_decodes_to_nothing() {
        let mut model = tiny_model(&[(b"a", true), (b"b", true)]);
        assert!(model.decompress(&[]).unwrap().is_empty());
    }

    #[test]
    fn rejects_a_foreign_huff_signature() {
        let error = Huffcdic::load(b"nope", &[]).unwrap_err();
        assert!(error.to_string().contains("HUFF signature"), "{error}");
    }

    #[test]
    fn rejects_a_zero_code_length() {
        let mut huff = Vec::from(*b"HUFF\x00\x00\x00\x18");
        huff.extend_from_slice(&24_u32.to_be_bytes());
        huff.extend_from_slice(&(24_u32 + 1024).to_be_bytes());
        huff.resize(24 + 1024 + 256, 0);
        let cdic = tiny_cdic(&[(b"a", true)]);
        let error = Huffcdic::load(&huff, &[&cdic]).unwrap_err();
        assert!(error.to_string().contains("zero code length"), "{error}");
    }

    #[test]
    fn rejects_a_short_code_that_is_not_marked_terminal() {
        let mut huff = Vec::from(*b"HUFF\x00\x00\x00\x18");
        huff.extend_from_slice(&24_u32.to_be_bytes());
        huff.extend_from_slice(&(24_u32 + 1024).to_be_bytes());
        huff.resize(24, 0);
        for _ in 0..256 {
            huff.extend_from_slice(&4_u32.to_be_bytes());
        }
        huff.resize(24 + 1024 + 256, 0);
        let cdic = tiny_cdic(&[(b"a", true)]);
        let error = Huffcdic::load(&huff, &[&cdic]).unwrap_err();
        assert!(error.to_string().contains("not marked terminal"), "{error}");
    }

    #[test]
    fn rejects_a_huff_record_without_a_cdic() {
        let error = Huffcdic::load(&tiny_huff(), &[]).unwrap_err();
        assert!(error.to_string().contains("no CDIC"), "{error}");
    }

    #[test]
    fn garbage_records_never_panic() {
        let mut model = tiny_model(&[(b"a", true), (b"b", true)]);
        let mut seed = 0x1234_5678_u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        for case in [
            vec![0_u8; 64],
            vec![0xFF_u8; 64],
            (0..512).map(|_| random()).collect::<Vec<u8>>(),
        ] {
            let _ = model.decompress(&case);
        }
        for bad in [vec![0_u8; 4096], vec![0xFF_u8; 4096]] {
            let _ = Huffcdic::load(&bad, &[&bad]);
        }
    }

    #[test]
    fn trailing_data_strips_the_flag_region_then_the_multibyte_byte() {
        // extra_record_flags = 3: one trailing region plus the multibyte byte.
        // The region's size is a big-endian varint with the marker on the first
        // byte, so `81 20` is 160 and not 4097.
        let mut record = vec![b'x'; 200];
        record[198] = 0x81;
        record[199] = 0x20;
        record[39] = 0xAA; // (0xAA & 3) + 1 = 3 bytes
        assert_eq!(text_body_len(&record, 3), 200 - 160 - 3);
        // Bit 0 alone strips only the multibyte byte, sized from the last byte.
        assert_eq!(text_body_len(&record, 1), 200 - 1);
        assert_eq!(text_body_len(&record, 0), 200);
    }
}
