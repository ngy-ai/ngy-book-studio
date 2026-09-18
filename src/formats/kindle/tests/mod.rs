//! Shared fixtures for the Kindle importer tests.

mod parse_test;

use super::*;

/// Lays out a PalmDB container: 78-byte header, record offset table, then
/// the records back to back.
fn build_palmdb(records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 78];
    bytes[0..8].copy_from_slice(b"TestBook");
    bytes[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
    let mut offset = 78 + records.len() * 8;
    for record in records {
        bytes.extend_from_slice(&(offset as u32).to_be_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        offset += record.len();
    }
    for record in records {
        bytes.extend_from_slice(record);
    }
    bytes
}

fn split_records(bytes: &[u8]) -> Vec<Vec<u8>> {
    let count = usize::from(u16::from_be_bytes([bytes[76], bytes[77]]));
    let starts = (0..count)
        .map(|index| read_u32(bytes, 78 + index * 8).unwrap() as usize)
        .collect::<Vec<_>>();
    (0..count)
        .map(|index| {
            let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
            bytes[starts[index]..end].to_vec()
        })
        .collect()
}

/// A HUFF/CDIC container whose single text record decodes to `ABCDAB`.
///
/// The trailing records stand in for the resource region: they must keep
/// their exact indices, because `first_image_index` and the `recindex=`
/// rewriting name them by number.
fn huffcdic_container() -> Vec<u8> {
    let text = b"ABCDAB";
    let mut record0 = vec![0_u8; 280];
    record0[0..2].copy_from_slice(&kindle_huff::COMPRESSION_HUFFCDIC.to_be_bytes());
    record0[4..8].copy_from_slice(&(text.len() as u32).to_be_bytes());
    record0[8..10].copy_from_slice(&1_u16.to_be_bytes());
    record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
    record0[16..20].copy_from_slice(b"MOBI");
    record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
    record0[112..116].copy_from_slice(&2_u32.to_be_bytes());
    record0[116..120].copy_from_slice(&2_u32.to_be_bytes());
    build_palmdb(&[
        record0,
        // `tiny_huff` maps 0xFF to phrase 0 and 0xFE to phrase 1.
        vec![0xFF, 0xFE, 0xFF],
        kindle_huff::tiny_huff(),
        kindle_huff::tiny_cdic(&[(b"AB", true), (b"CD", true)]),
        b"\x89PNG\r\n\x1a\nresource".to_vec(),
        b"last record".to_vec(),
    ])
}

#[test]
fn huffcdic_text_is_repacked_without_moving_record_indices() {
    let original = huffcdic_container();
    let prepared = prepare_kindle_source(&original).expect("the container is repacked");
    let rewritten = match prepared {
        Cow::Owned(bytes) => bytes,
        Cow::Borrowed(_) => panic!("HUFF/CDIC text must be rewritten"),
    };

    let records = split_records(&rewritten);
    assert_eq!(records.len(), 6, "the record count must not change");
    assert_eq!(
        u16::from_be_bytes([records[0][0], records[0][1]]),
        1,
        "the repacked container must declare uncompressed text records"
    );
    assert_eq!(
        records[0][4..8],
        [0_u8, 0, 0, 6],
        "the decoded text length is preserved"
    );
    assert_eq!(
        records[1].as_slice(),
        b"ABCDAB".as_slice(),
        "the text record must hold the decoded text"
    );

    let original_records = split_records(&original);
    for index in 2..6 {
        assert_eq!(
            records[index], original_records[index],
            "record {index} must stay at its original index unchanged"
        );
    }
}

#[test]
fn a_palmdoc_container_is_handed_over_untouched() {
    let mut record0 = vec![0_u8; 280];
    record0[0..2].copy_from_slice(&2_u16.to_be_bytes());
    let original = build_palmdb(&[record0, vec![0x01, 0x02], b"resource".to_vec()]);
    match prepare_kindle_source(&original).expect("PalmDOC needs no rewrite") {
        Cow::Borrowed(bytes) => assert_eq!(bytes, original.as_slice()),
        Cow::Owned(_) => panic!("PalmDOC text must not be rewritten"),
    }
}

/// Record 0 of a PalmDOC container whose `extra_record_flags` says the text
/// records carry both a trailing data region and the multibyte overlap.
fn record0_with_trailing_regions(compression: u16, text_record_count: u16) -> Vec<u8> {
    let mut record0 = vec![0_u8; 280];
    record0[0..2].copy_from_slice(&compression.to_be_bytes());
    record0[8..10].copy_from_slice(&text_record_count.to_be_bytes());
    record0[10..12].copy_from_slice(&4_096_u16.to_be_bytes());
    record0[16..20].copy_from_slice(b"MOBI");
    record0[20..24].copy_from_slice(&264_u32.to_be_bytes());
    record0[EXTRA_RECORD_FLAGS_FIELD..EXTRA_RECORD_FLAGS_FIELD + 4]
        .copy_from_slice(&3_u32.to_be_bytes());
    record0
}

/// Appends what `extra_record_flags == 3` describes: two multibyte overlap
/// bytes whose last byte holds `1` (so the stripper removes 2) and a
/// five-byte region whose trailing size varint reads 5.
fn with_trailing_region(mut body: Vec<u8>) -> Vec<u8> {
    body.extend_from_slice(&[0xAA, 0x01]);
    body.extend_from_slice(&[0xFF, 0x00, 0x00, 0x00, 0x05]);
    body
}

#[test]
fn palmdoc_text_records_lose_their_trailing_data_regions() {
    let original = build_palmdb(&[
        record0_with_trailing_regions(1, 2),
        with_trailing_region("作者简介".as_bytes().to_vec()),
        with_trailing_region("前言".as_bytes().to_vec()),
        b"\x89PNG\r\n\x1a\nresource".to_vec(),
    ]);
    let rewritten = match prepare_kindle_source(&original).expect("the container is repacked") {
        Cow::Owned(bytes) => bytes,
        Cow::Borrowed(_) => panic!("text records carrying trailing data must be rewritten"),
    };

    let records = split_records(&rewritten);
    assert_eq!(records.len(), 4, "the record count must not change");
    assert_eq!(
        records[1].as_slice(),
        "作者简介".as_bytes(),
        "the trailing region is not text and must be gone"
    );
    assert_eq!(records[2].as_slice(), "前言".as_bytes());
    assert_eq!(
        records[3].as_slice(),
        b"\x89PNG\r\n\x1a\nresource",
        "records after the text must stay at their index unchanged"
    );
    let joined = [records[1].as_slice(), records[2].as_slice()].concat();
    assert_eq!(
        std::str::from_utf8(&joined).expect("the joined text must be valid UTF-8"),
        "作者简介前言",
        "ebook-rs must no longer fall back to WINDOWS-1252"
    );
}

#[test]
fn trimming_trailing_regions_keeps_the_text_records_compressed() {
    let original = build_palmdb(&[
        record0_with_trailing_regions(2, 1),
        with_trailing_region(vec![0x41, 0x42]),
        b"resource".to_vec(),
    ]);
    let rewritten = match prepare_kindle_source(&original).expect("the container is repacked") {
        Cow::Owned(bytes) => bytes,
        Cow::Borrowed(_) => panic!("trailing regions must be trimmed"),
    };

    let records = split_records(&rewritten);
    assert_eq!(
        u16::from_be_bytes([records[0][0], records[0][1]]),
        2,
        "ebook-rs still owns the PalmDOC decoding"
    );
    assert_eq!(records[1].as_slice(), &[0x41, 0x42]);
    assert_eq!(records[2].as_slice(), b"resource");
}

#[test]
fn a_palmdoc_container_without_trailing_regions_is_handed_over_untouched() {
    let mut record0 = record0_with_trailing_regions(2, 1);
    record0[EXTRA_RECORD_FLAGS_FIELD..EXTRA_RECORD_FLAGS_FIELD + 4]
        .copy_from_slice(&0_u32.to_be_bytes());
    let original = build_palmdb(&[record0, vec![0x41, 0x42], b"resource".to_vec()]);
    match prepare_kindle_source(&original).expect("PalmDOC needs no rewrite") {
        Cow::Borrowed(bytes) => assert_eq!(bytes, original.as_slice()),
        Cow::Owned(_) => panic!("a container with no trailing regions must not be rewritten"),
    }
}
