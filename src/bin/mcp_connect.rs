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
    let output = std::thread::spawn(move || -> io::Result<()> {
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
        Ok(())
    });
    let copied = io::copy(&mut io::stdin().lock(), &mut socket);
    let _ = socket.shutdown(Shutdown::Write);
    let response = output.join().map_err(|_| "proxy output thread failed")?;
    copied?;
    response?;
    Ok(())
}
