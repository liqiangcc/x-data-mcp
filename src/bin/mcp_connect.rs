//! Transport-only stdio proxy. EOF/disconnect never invokes task cancellation.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("mcp_connect requires Linux owner-only IPC".into())
}
#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{self, Write},
        net::Shutdown,
    };
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: mcp_connect <private-socket>".into());
    }
    let mut socket = x_data_mcp::local_runtime::connect(std::path::Path::new(&args[0]))?;
    let mut response = socket.try_clone()?;
    let input = std::thread::spawn(move || -> io::Result<()> {
        let copied = io::copy(&mut io::stdin().lock(), &mut socket);
        let _ = socket.shutdown(Shutdown::Write);
        copied.map(|_| ())
    });
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut buffer = [0; 8192];
    loop {
        use std::io::Read;
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buffer[..n])?;
        stdout.flush()?;
    }
    // A dead service must produce EOF even if the MCP client keeps stdin open.
    // The process exits without waiting for a thread blocked on client input.
    let _ = response.shutdown(Shutdown::Both);
    if input.is_finished() {
        input.join().map_err(|_| "proxy input thread failed")??;
    }
    Ok(())
}
