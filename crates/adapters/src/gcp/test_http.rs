//! Minimal single-response HTTP server for exercising GCP sinks without network access.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

/// Local endpoint that answers the first request with a fixed status and body.
pub(crate) struct OneShotServer {
    /// Base URL (`http://127.0.0.1:<port>`) to use as the sink's API endpoint.
    pub base_url: String,
    handle: JoinHandle<String>,
}

impl OneShotServer {
    /// Starts a server that replies once with `status` (e.g. `"500 Internal Server Error"`) and `body`.
    pub fn start(status: &str, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test port");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept test connection");
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("read header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).expect("read body");
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .expect("write response");
            request_line
        });
        Self { base_url, handle }
    }

    /// Waits for the request and returns its request line (e.g. `POST /path HTTP/1.1`).
    pub fn request_line(self) -> String {
        self.handle.join().expect("test server thread")
    }
}

/// Local endpoint that answers successive requests with the given `(status, body)` in order,
/// one connection each, and records every request line.
pub(crate) struct ScriptedServer {
    /// Base URL (`http://127.0.0.1:<port>`) to use as the sink's API endpoint.
    pub base_url: String,
    handle: JoinHandle<Vec<String>>,
}

impl ScriptedServer {
    /// Starts a server that replies to the n-th request with `responses[n]`.
    pub fn start(responses: &[(&str, &str)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test port");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
        let responses: Vec<String> = responses
            .iter()
            .map(|(status, body)| {
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            })
            .collect();
        let handle = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().expect("accept test connection");
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                reader
                    .read_line(&mut request_line)
                    .expect("read request line");
                let mut content_length = 0usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).expect("read header");
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).expect("read body");
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
                lines.push(request_line);
            }
            lines
        });
        Self { base_url, handle }
    }

    /// Waits until every scripted response was served and returns the request lines.
    pub fn request_lines(self) -> Vec<String> {
        self.handle.join().expect("test server thread")
    }
}
