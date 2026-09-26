//! `tessera-api healthcheck` — a self-probe for container health checks (#26).
//!
//! The production image is distroless (`gcr.io/distroless/static-debian12`):
//! it has no shell and no `curl`, so the Docker `HEALTHCHECK` and the ECS
//! container health check run the API binary itself in this mode. It sends a
//! single `GET /health` to the local server and exits 0 only on `200 OK`
//! (the route answers 503 while the indexed snapshot is stale), matching the
//! `curl --fail` behaviour it replaces. Constant time and memory: one
//! connection, one status line read.

use std::{
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

/// Well inside the 3 s Docker `HEALTHCHECK --timeout`, so a hung server is
/// reported as unhealthy by this probe rather than killed by Docker.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Probe the server on `PORT` (default 8080) and return the process exit code.
pub fn run() -> i32 {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    if probe(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), PROBE_TIMEOUT) {
        0
    } else {
        1
    }
}

/// True when `GET /health` on `addr` answers with status 200.
fn probe(addr: SocketAddr, timeout: Duration) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
        || stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .is_err()
    {
        return false;
    }

    let mut status_line = String::new();
    if BufReader::new(stream).read_line(&mut status_line).is_err() {
        return false;
    }
    // "HTTP/1.1 200 OK" — the second token is the status code.
    status_line.split_whitespace().nth(1) == Some("200")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, thread};

    /// Serve one connection with `response` and return the server's address.
    fn serve_once(response: &'static str) -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = [0u8; 256];
            let _ = conn.read(&mut request);
            conn.write_all(response.as_bytes()).unwrap();
        });
        addr
    }

    #[test]
    fn healthy_on_200() {
        let addr = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        assert!(probe(addr, PROBE_TIMEOUT));
    }

    #[test]
    fn unhealthy_on_503_stale_snapshot() {
        let addr = serve_once("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
        assert!(!probe(addr, PROBE_TIMEOUT));
    }

    #[test]
    fn unhealthy_when_nothing_listens() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!probe(addr, PROBE_TIMEOUT));
    }

    #[test]
    fn unhealthy_when_server_hangs() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = thread::spawn(move || {
            let (_conn, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(2));
        });
        assert!(!probe(addr, Duration::from_millis(200)));
    }
}
