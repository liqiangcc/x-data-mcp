use x_data_mcp::gbbq::{self, Error};

fn fixture() -> Vec<u8> {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    bytes.extend((0..gbbq::RECORD_BYTES).map(|n| n as u8));
    bytes.extend((0..gbbq::RECORD_BYTES).map(|n| 255u8 - n as u8));
    bytes
}

#[test]
fn preserves_count_and_encrypted_records_without_semantic_decoding() {
    let parsed = gbbq::parse(&fixture()).unwrap();
    assert_eq!(parsed.record_count, 2);
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.records[0][0], 0);
    assert_eq!(parsed.records[0][gbbq::RECORD_BYTES - 1], 28);
    assert_eq!(parsed.records[1][0], 255);
    assert_eq!(parsed.records[1][gbbq::RECORD_BYTES - 1], 227);
}

#[test]
fn rejects_short_or_inconsistent_files_without_padding_or_truncation() {
    assert!(matches!(
        gbbq::parse(&[0; 3]),
        Err(Error::HeaderTruncated(3))
    ));
    let mut bytes = fixture();
    bytes.push(0);
    assert!(matches!(
        gbbq::parse(&bytes),
        Err(Error::LengthMismatch { .. })
    ));
    let mut bytes = fixture();
    bytes[0] = 3;
    assert!(matches!(
        gbbq::parse(&bytes),
        Err(Error::LengthMismatch { count: 3, .. })
    ));
}
