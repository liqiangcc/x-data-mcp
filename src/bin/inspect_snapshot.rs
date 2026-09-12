use std::{fs::File, io::Read, path::Path};
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: inspect_snapshot <manifest.json> <trusted-input-directory>".into());
    }
    let mut bytes = Vec::new();
    File::open(&args[0])?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let result = x_data_mcp::snapshot::inspect(&bytes, Path::new(&args[1]), chrono::Utc::now())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
