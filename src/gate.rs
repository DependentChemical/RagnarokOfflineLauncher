use crate::logging::Logger;
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Polls a TCP port until it accepts a connection - used as the readiness gate
/// that replaces "sleep and hope" (the reason the servers used `start cmd /k`).
pub fn wait_for_port(
    log: &Logger,
    host: &str,
    port: u16,
    timeout_secs: u64,
    what: &str,
) -> bool {
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            log.error(format!("{what}: invalid address {host}:{port} ({e})"));
            return false;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last_note = Instant::now();
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if last_note.elapsed() >= Duration::from_secs(10) {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            log.warn(format!(
                "{what}: waiting for {host}:{port} ({left}s left)"
            ));
            last_note = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn port_is_open(host: &str, port: u16) -> bool {
    match format!("{host}:{port}").parse::<SocketAddr>() {
        Ok(addr) => TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok(),
        Err(_) => false,
    }
}

/// Prints the final boot summary through the logger (the single console
/// writer) so it also lands in the log file.
pub fn print_ports(log: &Logger, host: &str, ports: &[(String, u16)]) {
    for (name, port) in ports {
        if port_is_open(host, *port) {
            log.ok(format!("{name:<22} {host}:{port}  listening"));
        } else {
            log.warn(format!("{name:<22} {host}:{port}  NOT listening"));
        }
    }
}