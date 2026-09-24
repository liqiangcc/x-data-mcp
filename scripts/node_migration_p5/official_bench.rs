// NM-110 P5 oracle — official materialize/zip-inspect surfaces, rlib-
// linked bench binary (NOT part of the crate; compiled on demand):
//   rustc --edition 2024 -O official_bench.rs \
//     --extern x_data_mcp=<rlib> --extern zip=<rlib> \
//     --extern chrono=<rlib> -L target/release/deps -o <out>
//
// Each subcommand times one surface (warmup + N iters) and prints ONE
// JSONL stats line — the same record shape official_bench.mjs emits.
//
// Surfaces:
//   zip_index <zip>          ZipArchive::new (EOCD+CD parse; the exact
//                            call private ArchiveIndex::open wraps)
//   zip_member_read <zip> <member>   by_name + read_to_end + sha256 —
//                            the private member_bytes composition
//   gbbq_decode <zip>        member read + gbbq::parse + decode_events +
//                            sha256 — the private decoded_gbbq body
//   official_inspect <root> <manifest>
//   materialize_warm <root> <manifest> <symbol>   warm session
//   materialize_cold <root> <manifest> <symbol>   fresh session/iter
use std::io::Read;
use std::time::Instant;

fn stats_line(surface: &str, samples: &mut Vec<u64>, input_bytes: u64,
              extra: &str) -> String {
    samples.sort_unstable();
    let n = samples.len();
    let mean = samples.iter().sum::<u64>() / n.max(1) as u64;
    let p50 = samples[n * 50 / 100];
    let p95 = samples[n * 95 / 100];
    let max = *samples.last().unwrap();
    format!(
        "{{\"surface\":{surface:?},\"impl\":\"rust\",\"iters\":{n},\"p50_ns\":{p50},\"p95_ns\":{p95},\"max_ns\":{max},\"mean_ns\":{mean},\"input_bytes\":{input_bytes},{extra}}}"
    )
}

fn timed<R, F: FnMut() -> R>(iters: usize, warmup: usize,
                             mut f: F) -> Vec<u64> {
    for _ in 0..warmup { std::hint::black_box(f()); }
    let mut s = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        std::hint::black_box(f());
        s.push(t.elapsed().as_nanos() as u64);
    }
    s
}

fn vmhwm() -> u64 {
    std::fs::read_to_string("/proc/self/status").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM"))
            .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap()))
        .unwrap_or(0)
}

fn iters() -> (usize, usize) {
    let it = std::env::var("BENCH_ITERS").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(200);
    let wu = std::env::var("BENCH_WARMUP").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(20);
    (it, wu)
}

fn main() {
    let (it, wu) = iters();
    let mut args = std::env::args().skip(1);
    let surface = args.next().expect("surface");
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T00:00:00Z")
        .unwrap().with_timezone(&chrono::Utc);
    match surface.as_str() {
        "zip_index" => {
            let path = args.next().expect("zip");
            let nbytes = std::fs::metadata(&path).unwrap().len();
            let mut s = timed(it, wu, || {
                zip::ZipArchive::new(
                    std::fs::File::open(&path).unwrap()).unwrap()
            });
            println!("{}", stats_line(&surface, &mut s, nbytes,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        "zip_member_read" => {
            let path = args.next().expect("zip");
            let member = args.next().expect("member");
            // member_bytes composition: candidate name lookup + full
            // read (Crc32Reader enforces the checksum) + sha256.
            let mut nbytes = 0u64;
            let mut s = timed(it, wu, || {
                let mut za = zip::ZipArchive::new(
                    std::fs::File::open(&path).unwrap()).unwrap();
                let mut f = za.by_name(&member).unwrap();
                let mut bytes = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut bytes).unwrap();
                nbytes = bytes.len() as u64;
                x_data_mcp::hash::sha256(&bytes)
            });
            println!("{}", stats_line(&surface, &mut s, nbytes,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        "gbbq_decode" => {
            let path = args.next().expect("gbbq zip");
            let mut nbytes = 0u64;
            let mut s = timed(it, wu, || {
                // decoded_gbbq body verbatim: member read, parse,
                // decode_events, by_security fold, digest — minus the
                // OnceCell (each iter pays the decode, which is what we
                // measure).
                let mut za = zip::ZipArchive::new(
                    std::fs::File::open(&path).unwrap()).unwrap();
                let mut f = za.by_name("gbbq").unwrap();
                let mut bytes = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut bytes).unwrap();
                nbytes = bytes.len() as u64;
                let raw = x_data_mcp::gbbq::parse(&bytes).unwrap();
                let events = x_data_mcp::gbbq::decode_events(&raw).unwrap();
                let mut by_security: std::collections::HashMap<
                    (u8, String), (usize, usize)> =
                    std::collections::HashMap::new();
                for event in &events {
                    let e = by_security
                        .entry((event.market, event.code.clone()))
                        .or_default();
                    if event.category == 1 { e.0 += 1 } else { e.1 += 1 }
                }
                x_data_mcp::hash::sha256(&bytes);
                events.len()
            });
            println!("{}", stats_line(&surface, &mut s, nbytes,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        "official_inspect" => {
            let root = args.next().expect("root");
            let manifest = std::fs::read(args.next().expect("manifest"))
                .unwrap();
            let nbytes = manifest.len() as u64;
            let mut s = timed(it, wu, || {
                x_data_mcp::official_market::TdxMarketSource::inspect(
                    &root, &manifest, now).unwrap()
            });
            println!("{}", stats_line(&surface, &mut s, nbytes,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        "materialize_warm" | "materialize_cold" => {
            let root = args.next().expect("root");
            let manifest = std::fs::read(args.next().expect("manifest"))
                .unwrap();
            let sym = args.next().expect("symbol");
            let source = x_data_mcp::official_market::TdxMarketSource::inspect(
                &root, &manifest, now).unwrap();
            let mut nbytes = 0u64;
            if surface == "materialize_warm" {
                let session = source.materialize_session();
                nbytes = session.materialize_daily(&sym).unwrap()
                    .bars.len() as u64;
                let mut s = timed(it, wu, ||
                    session.materialize_daily(&sym).unwrap().bars.len());
                println!("{}", stats_line(&surface, &mut s, nbytes,
                    &format!("\"peak_rss_kb\":{}", vmhwm())));
            } else {
                let mut s = timed(it, wu, || {
                    let b = source.materialize_daily(&sym).unwrap();
                    nbytes = b.bars.len() as u64;
                    b.bars.len()
                });
                println!("{}", stats_line(&surface, &mut s, nbytes,
                    &format!("\"peak_rss_kb\":{}", vmhwm())));
            }
        }
        other => panic!("unknown surface {other}"),
    }
}
