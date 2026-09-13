use x_data_mcp::dbf::{DbfError, parse};

fn fixture() -> Vec<u8> {
    let fields = [("GPDM", b'C', 6u8, 0u8), ("GXRQ", b'N', 8u8, 0u8)];
    let header_length = 32 + fields.len() * 32 + 1;
    let record_length = 1 + 6 + 8;
    let mut bytes = vec![0u8; header_length];
    bytes[0] = 0x03;
    bytes[1..4].copy_from_slice(&[126, 9, 13]);
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    bytes[8..10].copy_from_slice(&(header_length as u16).to_le_bytes());
    bytes[10..12].copy_from_slice(&(record_length as u16).to_le_bytes());
    bytes[29] = 0x7a;
    for (index, (name, kind, length, decimals)) in fields.into_iter().enumerate() {
        let start = 32 + index * 32;
        bytes[start..start + name.len()].copy_from_slice(name.as_bytes());
        bytes[start + 11] = kind;
        bytes[start + 16] = length;
        bytes[start + 17] = decimals;
    }
    bytes[header_length - 1] = 0x0d;
    bytes.extend_from_slice(b" 00000120260815");
    bytes.extend_from_slice(b"*600000 20250717");
    bytes.push(0x1a);
    bytes
}

#[test]
fn preserves_header_fields_deletion_and_raw_field_bytes() {
    let parsed = parse(&fixture()).unwrap();
    assert_eq!(parsed.version, 0x03);
    assert_eq!(parsed.record_count, 2);
    assert_eq!(parsed.code_page, 0x7a);
    assert_eq!(parsed.fields[0].name, "GPDM");
    assert!(!parsed.records[0].deleted);
    assert!(parsed.records[1].deleted);
    assert_eq!(
        parsed.records[0].field_bytes(&parsed.fields, 0),
        Some(b"000001".as_slice())
    );
    assert_eq!(
        parsed.records[0].field_bytes(&parsed.fields, 1),
        Some(b"20260815".as_slice())
    );
    assert_eq!(
        parsed.records[1].field_bytes(&parsed.fields, 0),
        Some(b"600000".as_slice())
    );
}

#[test]
fn rejects_truncated_or_malformed_containers_without_guessing() {
    assert!(matches!(
        parse(&[0; 31]),
        Err(DbfError::HeaderTruncated(31))
    ));
    let mut bytes = fixture();
    bytes.truncate(bytes.len() - 2);
    assert!(matches!(parse(&bytes), Err(DbfError::BodyTruncated { .. })));
    let mut bytes = fixture();
    bytes[0] = 0xff;
    assert!(matches!(
        parse(&bytes),
        Err(DbfError::UnsupportedVersion(0xff))
    ));
    let mut bytes = fixture();
    bytes[29] = 0x7a;
    // A code page byte is retained as metadata, never used to reinterpret text.
    assert_eq!(parse(&bytes).unwrap().code_page, 0x7a);
}
