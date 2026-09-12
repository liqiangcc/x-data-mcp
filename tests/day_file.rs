use x_data_mcp::day_file::{self, DayFileError};
fn record(date: u32, amount: u32) -> Vec<u8> {
    [date, 725, 730, 720, 725, amount, 0, 0xdeadbeef]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect()
}
#[test]
fn fields_and_float_bits_are_preserved_without_business_conversion() {
    let bytes = record(20250716, 0x3d4ccccd);
    let parsed = day_file::parse(&bytes).unwrap();
    assert_eq!(parsed[0].date.to_string(), "2025-07-16");
    assert_eq!(parsed[0].ohlc_raw, [725, 730, 720, 725]);
    assert_eq!(parsed[0].amount_float32_bits, 0x3d4ccccd);
    assert_eq!(parsed[0].volume_raw, 0);
    assert_eq!(parsed[0].reserved, 0xdeadbeef);
    assert_eq!(parsed, day_file::parse(&bytes).unwrap());
    assert_eq!(
        day_file::parse(&record(20250716, 0x7fc00001)).unwrap()[0].amount_float32_bits,
        0x7fc00001
    );
}
#[test]
fn truncation_invalid_dates_duplicates_and_reverse_order_reject() {
    let bytes = record(20250716, 0);
    assert_eq!(
        day_file::parse(&bytes[..31]),
        Err(DayFileError::Truncated(31))
    );
    for date in [0, 20250229, 20251301, 20250100, 100000101] {
        assert!(matches!(
            day_file::parse(&record(date, 0)),
            Err(DayFileError::InvalidDate { .. })
        ));
    }
    for date in [20250716, 20250715] {
        let mut values = bytes.clone();
        values.extend(record(date, 0));
        assert_eq!(day_file::parse(&values), Err(DayFileError::DateOrder(1)));
    }
    let mut values = bytes;
    values.extend(record(20250717, 0));
    assert_eq!(day_file::parse(&values).unwrap().len(), 2);
    assert!(day_file::parse(&[]).unwrap().is_empty());
}
#[test]
fn sha256_matches_standard_known_vector() {
    assert_eq!(
        day_file::sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
