use crate::{
    error::{Code, Error, Result},
    model::{self, MAX_HTTP_BYTES},
};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;
use std::{net::IpAddr, path::PathBuf, time::Duration};

/// Credentials are deliberately excluded from Debug and client identities.
#[derive(Clone)]
pub struct HttpOptions {
    pub url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub ca_file: Option<PathBuf>,
}
impl HttpOptions {
    pub fn new(url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            url: url.into(),
            api_key,
            timeout: Duration::from_secs(10),
            ca_file: None,
        }
    }
}
pub(crate) struct Transport {
    client: Client,
    runtime: Option<tokio::runtime::Runtime>,
    timeout: Duration,
    pub url: String,
    pub request_limit: usize,
    pub response_limit: usize,
    pub deadline_ms: u64,
}
impl Drop for Transport {
    fn drop(&mut self) {
        // Cancelled OS DNS work must not turn destruction into an unbounded
        // caller wait. HTTP requests are never detached or retried.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
impl Transport {
    pub fn new(options: HttpOptions, recern: bool) -> Result<Self> {
        if options.url.bytes().any(|b| b <= 32 || b == 127) {
            return Err(Error::invalid());
        }
        let url = reqwest::Url::parse(&options.url).map_err(|_| Error::invalid())?;
        let host = url.host_str().ok_or_else(Error::invalid)?;
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.scheme() == "http" && !loopback && (recern || options.api_key.is_some()))
            || options.timeout.is_zero()
            || options.timeout > Duration::from_secs(300)
        {
            return Err(Error::invalid());
        }
        if recern && options.api_key.is_none() {
            return Err(Error::invalid());
        }
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert("accept", HeaderValue::from_static("application/json"));
        if let Some(key) = &options.api_key {
            let range = if recern { 32..=256 } else { 1..=8192 };
            if !range.contains(&key.len()) || !key.bytes().all(|b| (33..=126).contains(&b)) {
                return Err(Error::invalid());
            }
            let mut value = HeaderValue::from_str(&if recern {
                format!("Bearer {key}")
            } else {
                key.clone()
            })
            .map_err(|_| Error::invalid())?;
            value.set_sensitive(true);
            headers.insert(if recern { "authorization" } else { "api-key" }, value);
        }
        let mut builder = Client::builder()
            .timeout(options.timeout)
            .connect_timeout(options.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .http1_only()
            .default_headers(headers)
            .user_agent("recern-vector-rust/1");
        if let Some(path) = options.ca_file {
            let bytes = std::fs::read(path).map_err(|_| Error::invalid())?;
            let certificates =
                reqwest::Certificate::from_pem_bundle(&bytes).map_err(|_| Error::invalid())?;
            if certificates.is_empty() {
                return Err(Error::invalid());
            }
            builder = builder.tls_certs_merge(certificates);
        }
        let client = builder.build().map_err(|_| Error::unavailable())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(8)
            .enable_all()
            .build()
            .map_err(|_| Error::unavailable())?;
        Ok(Self {
            client,
            runtime: Some(runtime),
            timeout: options.timeout,
            url: options.url.trim_end_matches('/').into(),
            request_limit: MAX_HTTP_BYTES,
            response_limit: MAX_HTTP_BYTES,
            deadline_ms: options.timeout.as_millis().clamp(1, 30000) as u64,
        })
    }

    pub fn request(
        &self,
        method: &str,
        route: &str,
        body: Option<&Value>,
        recern: bool,
        write: bool,
    ) -> Result<Value> {
        self.runtime
            .as_ref()
            .expect("live transport runtime")
            .block_on(async {
                tokio::time::timeout(self.timeout, self.send(method, route, body, recern))
                    .await
                    .map_err(|_| Error::unavailable())?
            })
            .map_err(|e| if write { e.write() } else { e })
    }
    async fn send(
        &self,
        method: &str,
        route: &str,
        body: Option<&Value>,
        recern: bool,
    ) -> Result<Value> {
        let mut request = self.client.request(
            method.parse().map_err(|_| Error::invalid())?,
            format!("{}{route}", self.url),
        );
        if recern {
            request = request.header("X-Recern-Timeout-Ms", self.deadline_ms.to_string());
        }
        if let Some(body) = body {
            let bytes = model::encode(body, self.request_limit)?;
            request = request.body(bytes);
        }
        // One outer async deadline covers send AND every streamed body chunk.
        // Blocking Response::read has a per-read timeout, not a total cap.
        let mut response = request.send().await.map_err(|_| Error::unavailable())?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|n| n > self.response_limit as u64)
            || headers
                .get("content-encoding")
                .is_some_and(|v| v != "identity")
        {
            return Err(Error::corrupt());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
            if chunk.len() > self.response_limit.saturating_sub(bytes.len()) {
                return Err(Error::corrupt());
            }
            bytes.extend_from_slice(&chunk);
        }
        if recern {
            if headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or("").trim().to_lowercase())
                != Some("application/json".into())
            {
                return Err(Error::corrupt());
            }
            let id = headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(Error::corrupt)?;
            if uuid::Uuid::parse_str(id)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(id)
            {
                return Err(Error::corrupt());
            }
            let value = model::strict_json(
                std::str::from_utf8(&bytes).map_err(|_| Error::corrupt())?,
                16,
            )
            .map_err(|_| Error::corrupt())?;
            if !value.is_object() {
                return Err(Error::corrupt());
            }
            if !matches!(status, 200 | 201) {
                return Err(server_error(status, &value, id)?);
            }
            Ok(value)
        } else {
            if !matches!(status, 200 | 201) {
                return Err(Error::new(match status {
                    400 | 422 => Code::InvalidArgument,
                    401 | 403 => Code::PermissionDenied,
                    404 => Code::NotFound,
                    409 => Code::AlreadyExists,
                    _ => Code::Unavailable,
                }));
            }
            let value = model::strict_json(
                std::str::from_utf8(&bytes).map_err(|_| Error::corrupt())?,
                80,
            )
            .map_err(|_| Error::corrupt())?;
            if !value.is_object() {
                return Err(Error::corrupt());
            }
            Ok(value)
        }
    }
}
fn server_error(status: u16, value: &Value, request_id: &str) -> Result<Error> {
    let error = &value["error"];
    let code: Code = model::decode(error["code"].clone())?;
    let valid = match status {
        400 | 405 | 415 => code == Code::InvalidArgument,
        401 | 403 => code == Code::PermissionDenied,
        404 => code == Code::NotFound,
        408 | 429 | 503 | 504 => code == Code::Unavailable,
        409 => matches!(code, Code::AlreadyExists | Code::Conflict),
        413 => matches!(code, Code::InvalidArgument | Code::Unsupported),
        422 => code == Code::Unsupported,
        500 => matches!(code, Code::CorruptData | Code::Unavailable),
        _ => false,
    };
    let reason = error["reason"].as_str().ok_or_else(Error::corrupt)?;
    if !valid
        || error["request_id"].as_str() != Some(request_id)
        || reason.is_empty()
        || reason.len() > 64
        || !reason.as_bytes()[0].is_ascii_lowercase()
        || !reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        || !error["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.chars().count() <= 512)
    {
        return Err(Error::corrupt());
    }
    let outcome = match error["write_outcome"].as_str() {
        Some("not_applicable") => "not_applicable",
        Some("not_committed") => "not_committed",
        Some("unknown") => "unknown",
        _ => return Err(Error::corrupt()),
    };
    Ok(Error {
        code,
        request_id: Some(request_id.into()),
        write_outcome: outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Instant,
    };
    #[test]
    fn redirects_and_drip_bodies_are_explicit_failures_without_retries() {
        for drip in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = vec![0; 4096];
                let _ = socket.read(&mut request).unwrap();
                let request_id = "2c291bb2-e575-4dc1-84ec-58e7237b8ed1";
                if drip {
                    write!(socket,"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nX-Request-ID: {request_id}\r\nContent-Length: 128\r\n\r\n").unwrap();
                    for _ in 0..128 {
                        if socket.write_all(b" ").is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                } else {
                    write!(socket,"HTTP/1.1 302 Found\r\nLocation: /do-not-forward-key\r\nContent-Type: application/json\r\nX-Request-ID: {request_id}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                }
                drop(socket);
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(Duration::from_millis(30));
                assert!(
                    listener.accept().is_err(),
                    "transport retried or followed a redirect"
                );
            });
            let mut options =
                HttpOptions::new(url, Some("recern-rust-transport-tests-only-000000".into()));
            options.timeout = Duration::from_millis(150);
            let transport = Transport::new(options, true).unwrap();
            let started = Instant::now();
            let error = transport
                .request(
                    "POST",
                    "/v1/records/upsert",
                    Some(&serde_json::json!({})),
                    true,
                    true,
                )
                .unwrap_err();
            assert!(started.elapsed() < Duration::from_millis(800));
            assert_eq!(error.write_outcome, "unknown");
            assert_eq!(
                error.code,
                if drip {
                    Code::Unavailable
                } else {
                    Code::CorruptData
                }
            );
            worker.join().unwrap();
        }
    }
}
