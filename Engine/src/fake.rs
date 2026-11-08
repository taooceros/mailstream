//! A stand-in for the providers in tests: an HTTP server on the loopback interface, on a thread
//! of its own, answering each request with a handler's reply and keeping what it got.
use std::sync::{Arc, Mutex};

use percent_encoding::percent_decode_str;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
pub struct FakeRequest {
    pub method: String,
    /// Decoded, without the query.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl FakeRequest {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(value: serde_json::Value) -> Self {
        Self::status(200, value)
    }

    pub fn status(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

pub struct FakeServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    pub requests: Arc<Mutex<Vec<FakeRequest>>>,
}

impl FakeServer {
    pub fn start(handler: impl Fn(&FakeRequest) -> Reply + Send + Sync + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (handler, kept) = (Arc::new(handler), requests.clone());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                while let Ok((mut connection, _)) = listener.accept().await {
                    let (handler, kept) = (handler.clone(), kept.clone());
                    tokio::spawn(async move {
                        let Some(request) = read_request(&mut connection).await else { return };
                        kept.lock().unwrap().push(request.clone());
                        let reply = handler(&request);
                        let mut head = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
                        for (name, value) in &reply.headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let _ = connection.write_all(head.as_bytes()).await;
                        let _ = connection.write_all(&reply.body).await;
                        let _ = connection.shutdown().await;
                    });
                }
            });
        });
        Self { base, requests }
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

async fn read_request(connection: &mut tokio::net::TcpStream) -> Option<FakeRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = connection.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..count]);
        let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_string(), value.trim().to_string())))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < end + 4 + length {
            let count = connection.read(&mut chunk).await.ok()?;
            if count == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..count]);
        }
        let body = buffer[end + 4..].to_vec();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let decode = |text: &str| percent_decode_str(text).decode_utf8_lossy().into_owned();
        let query = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(name), decode(value))
            })
            .collect();
        return Some(FakeRequest { method, path: decode(path), query, headers, body });
    }
}
