use x_data_mcp::catalog::{
    CatalogRepository, CatalogService, CatalogVersion, IndustryInfo, IndustryScheme, RefreshStatus,
    SecurityCatalogSource,
};
use x_data_mcp::catalog_tdx::{compose, ex_code, incon, scope, tdxhy};

fn gb18030(text: &str) -> Vec<u8> {
    encoding_rs::GB18030.encode(text).0.into_owned()
}

// ---------- tdxhy.cfg ----------

#[test]
fn tdxhy_parses_anchored_rows_and_preserves_leading_zeroes() {
    let bytes = b"0|000001|T1001|||X500102\r\n1|600004|T090302|||X6102\r\n2|920001|T05|||\r\n";
    let rows = tdxhy::parse(bytes).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].code, "000001");
    assert_eq!(rows[0].market, 0);
    assert_eq!(rows[0].t_code.as_deref(), Some("T1001"));
    assert_eq!(rows[0].x_code.as_deref(), Some("X500102"));
    assert_eq!(rows[2].market, 2);
    assert_eq!(rows[2].x_code, None);
}

#[test]
fn tdxhy_rejects_every_malformed_shape_without_skipping() {
    let good = b"0|000001|T1001|||X500102\r\n";
    for (bytes, marker) in [
        (
            &b"0|000001|T1001|||X500102\n0|000002|T1001|||X500102\n"[..],
            "expected 6",
        ), // bare LF
        (&b"\r\n"[..], "is empty"), // interior empty line
        (&b"0|000001|T1001||\r\n"[..], "expected 6"),
        (&b"3|000001|T1001|||X500102\r\n"[..], "not a single digit"),
        (&b"0|00001|T1001|||X500102\r\n"[..], "exactly six digits"), // five digits
        (&b"0|000001a|T1001|||X500102\r\n"[..], "exactly six digits"), // loses a leading zero check
        (&b"0|000001|T1A1|||X500102\r\n"[..], "not a T code"),       // non-digit
        (&b"0|000001|T1001234|||X500102\r\n"[..], "not a T code"),   // unanchored length
        (&b"0|000001|T1001|||X50010\r\n"[..], "not an X code"),      // unanchored length
        (&b"0|000001|T1001|||Y500102\r\n"[..], "not an X code"),
        (&b"0|000001|T1001|x||X500102\r\n"[..], "must be empty"), // f3 non-empty
        (&b"0|000001|T1001|||y|X500102\r\n"[..], "expected 6"),   // seven fields
    ] {
        let err = tdxhy::parse(bytes).unwrap_err().to_string();
        assert!(err.contains(marker), "{err} should mention {marker}");
    }
    // A second bad line is still reported even when line 1 is valid.
    let mut mixed = good.to_vec();
    mixed.extend_from_slice(b"9|000001|T1001|||X500102\r\n");
    assert!(
        tdxhy::parse(&mixed)
            .unwrap_err()
            .to_string()
            .contains("line 2")
    );
    // Non-ASCII bytes reject the whole member.
    let mut non_ascii = good.to_vec();
    non_ascii.extend_from_slice(&[0xc6, 0xbd]);
    assert_eq!(tdxhy::parse(&non_ascii), Err(tdxhy::TdxhyError::NotAscii));
}

// ---------- infoharbor_ex.code ----------

#[test]
fn ex_code_decodes_gb18030_and_ignores_keywords() {
    let mut bytes = gb18030("000001|平安银行|平安保险,谢永林\r\n");
    // Keywords containing a literal '|' stay on the same row.
    bytes.extend_from_slice(&gb18030("600004|白云机场|a|b\r\n"));
    let rows = ex_code::parse(&bytes).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "平安银行");
    assert_eq!(ex_code::name_map(&rows)["600004"], "白云机场");
}

#[test]
fn ex_code_fails_closed_on_bad_input() {
    assert_eq!(
        ex_code::parse(&[0xff, 0xfe, 0xfd]),
        Err(ex_code::ExCodeError::Decode)
    );
    for (text, marker) in [
        ("000001|\r\n", "expected at least 3"),
        ("00001|名|kw\r\n", "exactly six digits"),
        ("000001||kw\r\n", "empty name"),
        (
            "000001|名|kw\r\n000001|名二|kw\r\n",
            "appears more than once",
        ),
        ("000001|名|kw\r\n\r\n000002|名|kw\r\n", "is empty"),
    ] {
        let err = ex_code::parse(&gb18030(text)).unwrap_err().to_string();
        assert!(err.contains(marker), "{err} should mention {marker}");
    }
}

// ---------- incon.dat ----------

#[test]
fn incon_parses_sections_entries_and_levels() {
    let bytes = gb18030(
        "#TDXRSHY\r\nX42|软件信息\r\nX4202|软件服务\r\nX420202|细分领域\r\n#ZB_GPXT\r\n1|1|MACD金叉\r\n",
    );
    let sections = incon::parse(&bytes).unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].name, "TDXRSHY");
    assert_eq!(sections[0].entries.len(), 3);
    assert_eq!(sections[1].entries[0].name, "1");
    assert_eq!(sections[1].entries[0].extra, vec!["MACD金叉"]);
    let map = incon::section_map(&sections, "TDXRSHY").unwrap();
    assert!(map.contains(&("X4202", "软件服务")));
    assert_eq!(incon::code_level("X4202"), Some(2));
    assert_eq!(incon::code_level("X420202"), Some(3));
    assert_eq!(incon::code_level("X42"), Some(1));
    assert_eq!(incon::code_level("X420"), None);
    assert_eq!(incon::code_level("4202"), None);
    assert_eq!(incon::research_level2("X420202").as_deref(), Some("X4202"));
    assert_eq!(incon::research_level2("X4202").as_deref(), Some("X4202"));
    assert_eq!(incon::research_level2("X42"), None);
}

#[test]
fn incon_fails_closed_on_bad_input() {
    for (text, marker) in [
        ("X4202|软件服务\r\n", "before the first section header"),
        ("#S\r\nX4202|a\r\nX4202|b\r\n", "repeats code"),
        ("#S\r\nX4202\r\n", "expected at least 2"),
        ("#S\r\n|name\r\n", "non-ASCII or empty code"),
        ("#S\r\nX1|\r\n", "empty name"),
    ] {
        let err = incon::parse(&gb18030(text)).unwrap_err().to_string();
        assert!(err.contains(marker), "{err} should mention {marker}");
    }
}

// ---------- scope rule ----------

#[test]
fn scope_partition_is_disjoint_exhaustive_and_reconciles() {
    let rows = tdxhy::parse(
        b"0|000001|T1001|||X500102\r\n\
           1|600004|T090302|||X6102\r\n\
           2|920001|T05|||\r\n\
           0|200002||||\r\n\
           1|900901||||\r\n\
           0|002710||||\r\n",
    )
    .unwrap();
    let outcome = scope::apply(&rows);
    let counts = outcome.reconciliation(rows.len());
    assert_eq!(counts.scope, 2);
    assert_eq!(counts.bj_market, 1);
    assert_eq!(counts.b_share, 2);
    assert_eq!(counts.anomalous, 1);
    assert_eq!(
        counts.total,
        counts.scope + counts.bj_market + counts.b_share + counts.anomalous
    );
    assert_eq!(outcome.excluded[3].class, scope::Exclusion::Anomalous);
    assert_eq!(outcome.excluded[3].code, "002710");
}

// ---------- composition layer (S4–S6) ----------

fn sections() -> Vec<incon::InconSection> {
    incon::parse(&gb18030(
        "#TDXNHY\r\nT1001|金融\r\n#TDXRSHY\r\nX50|信息技术\r\nX5001|软件服务\r\nX500102|细分领域\r\n",
    ))
    .unwrap()
}

fn compose_members<'a>(
    tdxhy_bytes: &'a [u8],
    ex_bytes: &'a [u8],
    sections: &'a [incon::InconSection],
) -> compose::ComposeOutcome {
    let rows = tdxhy::parse(tdxhy_bytes).unwrap();
    let outcome = scope::apply(&rows);
    let names = ex_code::parse(ex_bytes).unwrap();
    compose::compose(
        &outcome.members,
        &ex_code::name_map(&names),
        sections,
        "official-file:deadbeefcafe0001",
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    )
    .unwrap()
}

#[test]
fn compose_assembles_assigned_records_with_scheme_label() {
    let outcome = compose_members(
        b"0|000001|T1001|||X500102\r\n1|600004|T1001|||X5001\r\n",
        &gb18030("000001|平安银行|kw\r\n600004|白云机场|kw\r\n"),
        &sections(),
    );
    assert!(outcome.nameless.is_empty());
    assert!(outcome.missing_industry.is_empty());
    assert!(outcome.t_code_residuals.is_empty());
    assert!(outcome.t_dictionary_checked);
    let snapshot = outcome.snapshot.unwrap();
    assert_eq!(snapshot.records.len(), 2);
    let first = &snapshot.records[0];
    assert_eq!(first.symbol, "000001");
    assert_eq!(first.name, "平安银行");
    assert_eq!(first.source_security_id, "tdxhy:0:000001");
    assert_eq!(first.source, "official-file:deadbeefcafe0001");
    assert_eq!(
        first.industry,
        IndustryInfo::Assigned {
            code: "X5001".into(), // leaf X500102 resolved to its L2 ancestor
            name: "软件服务".into(),
            scheme: Some(IndustryScheme::TdxResearchIndustry),
        }
    );
    let second = &snapshot.records[1];
    assert_eq!(second.exchange, x_data_mcp::catalog::Exchange::Shanghai);
    assert_eq!(
        second.industry,
        IndustryInfo::Assigned {
            code: "X5001".into(), // L2 code resolves to itself
            name: "软件服务".into(),
            scheme: Some(IndustryScheme::TdxResearchIndustry),
        }
    );
}

#[test]
fn compose_rejects_candidate_when_any_member_is_nameless() {
    let outcome = compose_members(
        b"0|000001|T1001|||X500102\r\n0|000099|T1001|||X500102\r\n0|000100|T1001|||X500102\r\n",
        &gb18030("000001|平安银行|kw\r\n"),
        &sections(),
    );
    // P1 default reject_candidate: the whole candidate is withheld and every
    // nameless member is enumerated — no guessing, no partial publication.
    assert!(outcome.snapshot.is_none());
    assert_eq!(
        outcome.nameless,
        vec![(0u8, "000099".to_string()), (0u8, "000100".to_string())]
    );
}

#[test]
fn compose_keeps_members_with_unresolvable_industry_as_missing() {
    let outcome = compose_members(
        // 000002: L1 x_code has no L2 ancestor; 000003: L2 absent from TDXRSHY.
        b"0|000002|T1001|||X42\r\n0|000003|T1001|||X9901\r\n",
        &gb18030("000002|万科|kw\r\n000003|测试|kw\r\n"),
        &sections(),
    );
    let snapshot = outcome.snapshot.unwrap();
    assert_eq!(outcome.missing_industry.len(), 2);
    for record in &snapshot.records {
        match &record.industry {
            IndustryInfo::Missing { reason } => {
                assert!(reason.starts_with("tdx_research:"), "{reason}")
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }
}

#[test]
fn compose_reports_t_code_residuals_without_binding_names() {
    let outcome = compose_members(
        b"1|688688|T00|||X500102\r\n", // T00 absent from the fixture TDXNHY
        &gb18030("688688|测试科创|kw\r\n"),
        &sections(),
    );
    let snapshot = outcome.snapshot.unwrap();
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(
        outcome.t_code_residuals,
        vec![compose::TCodeResidual {
            member: (1, "688688".into()),
            t_code: "T00".into(),
        }]
    );
}

#[test]
fn composed_snapshot_passes_existing_refresh_validation() {
    struct Repo(std::sync::Mutex<Option<CatalogVersion>>);
    impl CatalogRepository for Repo {
        fn latest(&self) -> Result<Option<CatalogVersion>, std::io::Error> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn publish_atomic(&self, version: &CatalogVersion) -> Result<(), std::io::Error> {
            *self.0.lock().unwrap() = Some(version.clone());
            Ok(())
        }
    }
    struct Src(Option<x_data_mcp::catalog::CatalogSnapshot>);
    impl SecurityCatalogSource for Src {
        fn fetch(
            &self,
        ) -> Result<x_data_mcp::catalog::CatalogSnapshot, x_data_mcp::catalog::CatalogSourceError>
        {
            Ok(self.0.clone().unwrap())
        }
    }
    let outcome = compose_members(
        b"0|000001|T1001|||X500102\r\n1|600004|T1001|||X5001\r\n",
        &gb18030("000001|平安银行|kw\r\n600004|白云机场|kw\r\n"),
        &sections(),
    );
    let service = CatalogService::new(Src(outcome.snapshot), Repo(std::sync::Mutex::new(None)));
    let result = service.refresh().unwrap();
    assert_eq!(result.status, RefreshStatus::Published);
}

// ---------- retained gen-2 integration (skipped when inputs are absent) ----------

const RETAINED: &str = "/home/box/retained-inputs";

fn member(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Vec<u8> {
    use std::io::Read;
    let mut bytes = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn gen2_retained_members_match_the_frozen_counts() {
    let base = std::path::Path::new(RETAINED).join("official-base.zip");
    let gbbq = std::path::Path::new(RETAINED).join("official-gbbq.zip");
    if !base.exists() || !gbbq.exists() {
        eprintln!("skipping: {RETAINED} not present on this host");
        return;
    }
    let mut base_zip = zip::ZipArchive::new(std::fs::File::open(&base).unwrap()).unwrap();
    let tdxhy_rows = tdxhy::parse(&member(&mut base_zip, "tdxhy.cfg")).unwrap();
    let outcome = scope::apply(&tdxhy_rows);
    let counts = outcome.reconciliation(tdxhy_rows.len());
    assert_eq!(counts.total, 5659);
    assert_eq!(counts.scope, 5230);
    assert_eq!(counts.bj_market, 348);
    assert_eq!(counts.b_share, 78);
    assert_eq!(counts.anomalous, 3);
    let anomalous: Vec<&str> = outcome
        .excluded
        .iter()
        .filter(|row| row.class == scope::Exclusion::Anomalous)
        .map(|row| row.code.as_str())
        .collect();
    assert_eq!(anomalous, ["002710", "002720", "201872"]);

    let names = ex_code::parse(&member(&mut base_zip, "infoharbor_ex.code")).unwrap();
    let name_map = ex_code::name_map(&names);
    // Contract v1: every S member's level-2 X code resolves, and the six
    // nameless members are exactly the anchored set.
    let mut gbbq_zip = zip::ZipArchive::new(std::fs::File::open(&gbbq).unwrap()).unwrap();
    let zhb = member(&mut gbbq_zip, "zhb.zip");
    let mut zhb_zip = zip::ZipArchive::new(std::io::Cursor::new(zhb)).unwrap();
    let mut incon_bytes = Vec::new();
    use std::io::Read;
    zhb_zip
        .by_name("incon.dat")
        .unwrap()
        .read_to_end(&mut incon_bytes)
        .unwrap();
    let sections = incon::parse(&incon_bytes).unwrap();
    assert_eq!(incon::section_map(&sections, "TDXNHY").unwrap().len(), 145);
    assert_eq!(incon::section_map(&sections, "TDXRSHY").unwrap().len(), 474);
    let dictionary: std::collections::BTreeMap<_, _> = incon::section_map(&sections, "TDXRSHY")
        .unwrap()
        .into_iter()
        .collect();
    let mut nameless = Vec::new();
    for member in &outcome.members {
        let l2 = incon::research_level2(&member.x_code)
            .unwrap_or_else(|| panic!("{} has no L2 ancestor", member.x_code));
        assert!(
            dictionary.contains_key(l2.as_str()),
            "{l2} missing from TDXRSHY"
        );
        if !name_map.contains_key(member.code.as_str()) {
            nameless.push(member.code.clone());
        }
    }
    assert_eq!(
        nameless,
        ["001235", "300728", "601206", "603302", "603361", "688688"]
    );

    // Composition layer on the real snapshot: P1 default reject_candidate
    // must fire with exactly the anchored nameless set, the T00 residual is
    // reported verbatim, and no member lands in Missing.
    let composed = compose::compose(
        &outcome.members,
        &name_map,
        &sections,
        "official-file:dcdf6b4989892667",
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    )
    .unwrap();
    assert!(composed.snapshot.is_none());
    assert_eq!(composed.nameless.len(), 6);
    assert_eq!(
        composed
            .nameless
            .iter()
            .map(|(_, code)| code.as_str())
            .collect::<Vec<_>>(),
        ["001235", "300728", "601206", "603302", "603361", "688688"]
    );
    assert!(composed.missing_industry.is_empty());
    assert!(composed.t_dictionary_checked);
    assert_eq!(
        composed.t_code_residuals,
        vec![compose::TCodeResidual {
            member: (1, "688688".into()),
            t_code: "T00".into(),
        }]
    );
}
