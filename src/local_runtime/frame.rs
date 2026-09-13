//! Bound incomplete frames, not the lifetime of an idle MCP session.
use std::{
    io::{self, BufRead},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub(super) fn read_frame<R: BufRead>(
    input: &mut R,
    stopping: &AtomicBool,
    limit: usize,
    incomplete_budget: Duration,
) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    let mut started: Option<Instant> = None;
    loop {
        if stopping.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if started.is_some_and(|at| at.elapsed() >= incomplete_budget) {
            return Err(io::Error::other("incomplete IPC frame deadline expired"));
        }
        let bytes = match input.fill_buf() {
            Ok(bytes) => bytes,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        if stopping.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if started.is_some_and(|at| at.elapsed() >= incomplete_budget) {
            return Err(io::Error::other("incomplete IPC frame deadline expired"));
        }
        if bytes.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other("EOF inside IPC frame"))
            };
        }
        let count = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        if frame.len().checked_add(count).is_none_or(|n| n > limit) {
            return Err(io::Error::other("IPC frame exceeds limit"));
        }
        let complete = bytes[count - 1] == b'\n';
        started.get_or_insert_with(Instant::now);
        frame.extend_from_slice(&bytes[..count]);
        input.consume(count);
        if complete {
            return Ok(Some(frame));
        }
    }
}
