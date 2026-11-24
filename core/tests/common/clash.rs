//! A loopback stand-in for the sing-box Clash API: what the display endpoints return, and a log
//! of who asked. Real sockets, so tests that use it run on real time.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// What one path answers.
#[derive(Clone)]
pub enum Reply {
    /// `status`, then the body at once, then the connection closes.
    Body(&'static str, String),
    /// Lines with `gap` between them; the connection stays open afterwards when `hold`.
    Lines {
        lines: Vec<String>,
        gap: Duration,
        hold: bool,
    },
    /// Accepts the request and says nothing.
    Silent,
}
#[derive(Default)]
pub struct Api {
    routes: Mutex<HashMap<String, Reply>>,
    /// Request heads in the order they arrived.
    pub requests: Mutex<Vec<String>>,
    /// Connections that are open now (a stream counts until the client leaves).
    pub open: AtomicUsize,
}
impl Api {
    pub fn route(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.to_owned(), reply);
    }
    pub fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|head| head.starts_with(&format!("GET {path} ")))
            .count()
    }
    pub fn heads(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
/// Serves `api` on a free loopback port.
pub async fn serve(api: Arc<Api>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let api = api.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 2048];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let first = text.lines().next().unwrap_or_default().to_owned();
                api.requests.lock().unwrap().push(text);
                let path = first.split_whitespace().nth(1).unwrap_or("/");
                let route = path.split('?').next().unwrap_or(path).to_owned();
                let reply = api.routes.lock().unwrap().get(&route).cloned();
                api.open.fetch_add(1, Ordering::SeqCst);
                match reply {
                    Some(Reply::Body(status, body)) => {
                        let response = format!(
                            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    }
                    Some(Reply::Lines { lines, gap, hold }) => {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n")
                            .await;
                        for line in lines {
                            if socket
                                .write_all(format!("{line}\n").as_bytes())
                                .await
                                .is_err()
                            {
                                break;
                            }
                            tokio::time::sleep(gap).await;
                        }
                        if hold {
                            // Until the client leaves: a read that ends is the client closing.
                            let mut sink = [0u8; 64];
                            loop {
                                match socket.read(&mut sink).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) => {}
                                }
                            }
                        }
                    }
                    Some(Reply::Silent) => {
                        let mut sink = [0u8; 64];
                        loop {
                            match socket.read(&mut sink).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    }
                    None => {
                        let _ = socket
                            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                            .await;
                    }
                }
                api.open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    port
}
