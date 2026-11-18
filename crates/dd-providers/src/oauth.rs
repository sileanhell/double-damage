//! Общий OAuth-помощник: PKCE (S256), loopback callback-сервер, открытие браузера.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener as StdListener;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::types::AuthCallback;

const CALLBACK_HTML: &str = "<!doctype html><html><body style=\"font-family:sans-serif;display:flex;\
justify-content:center;align-items:center;height:90vh\"><div><h2>Авторизация завершена</h2>\
<p>Эту вкладку можно закрыть и вернуться в приложение.</p></div></body></html>";

/// URL-энкодинг одного параметра (unreserved RFC 3986 не кодируются).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Склеить query из пар ключ-значение (значения энкодятся).
pub fn query_string(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn random_b64(len: usize) -> String {
    let mut buf = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Пара PKCE: (code_verifier, code_challenge S256).
pub fn pkce_pair() -> (String, String) {
    let verifier = random_b64(48); // 64 url-safe символа, в диапазоне 43..128
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Случайный state для OAuth.
pub fn random_state() -> String {
    random_b64(24)
}

/// Открыть URL в системном браузере (без внешних зависимостей).
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let res = std::process::Command::new("xdg-open").arg(url).spawn();
    res.map(|_| ())
        .map_err(|e| Error::OAuth(format!("cannot open browser: {e}")))
}

/// Одноразовый HTTP-сервер на 127.0.0.1 для ловли OAuth-redirect'а.
/// Создаётся синхронно (std listener), ожидание — через tokio.
pub struct CallbackServer {
    listener: StdListener,
    /// Порт, на котором слушаем.
    pub port: u16,
    /// Путь, который ожидаем (`/callback`).
    pub path: String,
}

impl CallbackServer {
    /// Бинд на случайном свободном порту 127.0.0.1.
    pub fn bind(path: &str) -> Result<Self> {
        let listener = StdListener::bind(("127.0.0.1", 0))
            .map_err(|e| Error::OAuth(format!("cannot bind callback listener: {e}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| Error::OAuth(format!("listener nonblocking: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::OAuth(e.to_string()))?
            .port();
        Ok(Self { listener, port, path: path.to_string() })
    }

    /// URL redirect_uri для этого сервера.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path)
    }

    /// Ждёт один входящий GET, отдаёт HTML-страницу «можно закрыть» и
    /// возвращает параметры `code`/`state` из query-строки.
    pub async fn wait(self, timeout: Duration) -> Result<AuthCallback> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let listener = self.listener;
        let path = self.path.clone();

        std::thread::spawn(move || {
            let _ = wait_one(&listener, &path, timeout, tx);
        });

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::OAuth("callback listener died".into())),
            Err(_) => Err(Error::OAuth("oauth callback timed out".into())),
        }
    }
}

fn wait_one(
    listener: &StdListener,
    expected_path: &str,
    timeout: Duration,
    tx: tokio::sync::oneshot::Sender<Result<AuthCallback>>,
) -> Result<()> {
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::OAuth(e.to_string()))?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let result = handle_conn(stream, expected_path);
                let _ = tx.send(result);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    let _ = tx.send(Err(Error::OAuth("oauth callback timed out".into())));
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = tx.send(Err(Error::OAuth(format!("accept: {e}"))));
                return Ok(());
            }
        }
    }
}

fn handle_conn(stream: std::net::TcpStream, expected_path: &str) -> Result<AuthCallback> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| Error::OAuth(e.to_string()))?,
    );
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| Error::OAuth(format!("read request: {e}")))?;

    // Дочитываем заголовки (не нужны, но браузер ждёт корректный HTTP).
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let (req_path, query) = target.split_once('?').unwrap_or((&target, ""));

    let mut out = std::io::BufWriter::new(reader.into_inner());
    if req_path != expected_path {
        write_http(&mut out, 404, "not found");
        return Err(Error::OAuth(format!("unexpected callback path {req_path}")));
    }
    write_http(&mut out, 200, CALLBACK_HTML);

    let params: HashMap<String, String> = parse_query(query);

    if let Some(err) = params.get("error") {
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_default();
        return Err(Error::OAuth(format!("{err}: {desc}")));
    }

    Ok(AuthCallback {
        code: params.get("code").cloned().unwrap_or_default(),
        state: params.get("state").cloned().unwrap_or_default(),
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => out.push(b),
                    Err(_) => out.extend_from_slice(&bytes[i..i + 3]),
                }
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn write_http(out: &mut impl Write, status: u16, body: &str) {
    let status_line = match status {
        200 => "200 OK",
        404 => "404 Not Found",
        _ => "200 OK",
    };
    let _ = write!(
        out,
        "HTTP/1.1 {status_line}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = out.flush();
}
