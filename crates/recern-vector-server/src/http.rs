use crate::{
    error::{ApiError, Result},
    store::{Limits, Store},
    wire,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, Response},
};
use hmac::{Hmac, Mac};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet, time::timeout_at};
use uuid::Uuid;

pub const OPENAPI: &str = include_str!("../../../api/openapi.json");
const READ_ROUTES: &[&str] = &[
    "/v1/collections/list",
    "/v1/collections/describe",
    "/v1/records/get",
    "/v1/records/count",
    "/v1/records/page",
    "/v1/search",
    "/v1/snapshots/create",
    "/v1/snapshots/release",
];
const WRITE_ROUTES: &[&str] = &[
    "/v1/collections/create",
    "/v1/collections/drop",
    "/v1/records/upsert",
    "/v1/records/delete",
];

pub struct App {
    store: Mutex<Store>,
    key_hash: [u8; 32],
    pub limits: Limits,
    active: Arc<Semaphore>,
    admission: Arc<Semaphore>,
    pub draining: AtomicBool,
}
impl App {
    pub fn new(store: Store, key: &[u8], limits: Limits) -> Result<Arc<Self>> {
        limits.validate()?;
        if !(32..=256).contains(&key.len()) || key.iter().any(|c| !c.is_ascii_graphic()) {
            return Err(ApiError::invalid());
        }
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            key_hash: Sha256::digest(key).into(),
            active: Arc::new(Semaphore::new(limits.active)),
            admission: Arc::new(Semaphore::new(limits.active + limits.queued)),
            limits,
            draining: AtomicBool::new(false),
        }))
    }
    fn authorized(&self, request: &Request) -> bool {
        let mut headers = request.headers().get_all("authorization").iter();
        let token = headers
            .next()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if headers.next().is_some() {
            return false;
        }
        let Some(token) = token.filter(|t| (32..=256).contains(&t.len())) else {
            return false;
        };
        // Hash first, then compare fixed-size authentication tags in constant
        // time. No raw key is retained in the server's shared application state.
        let digest = Sha256::digest(token.as_bytes());
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key_hash).expect("fixed key");
        mac.update(b"recern-http-auth");
        let expected = mac.finalize().into_bytes();
        let mut candidate = Hmac::<Sha256>::new_from_slice(&digest).expect("fixed key");
        candidate.update(b"recern-http-auth");
        candidate.verify_slice(&expected).is_ok()
    }
    pub async fn drain(&self) {
        self.draining.store(true, Ordering::Release);
        // Every admitted worker owns its admission permit, including after a
        // client disconnect or timeout. Wait for committed writes to finish.
        let _ = self
            .admission
            .acquire_many((self.limits.active + self.limits.queued) as u32)
            .await;
    }
}

fn response(status: u16, body: Vec<u8>, id: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .header("cache-control", "no-store")
        .header("x-request-id", id)
        .body(Body::from(body))
        .expect("static valid headers")
}
fn error(error: ApiError, id: &str) -> Response<Body> {
    let body = serde_json::to_vec(&error.body(id)).expect("static error");
    let mut result = response(error.status, body, id);
    if error.status == 401 {
        result
            .headers_mut()
            .insert("www-authenticate", "Bearer".parse().unwrap());
    }
    if error.status == 429 {
        result
            .headers_mut()
            .insert("retry-after", "1".parse().unwrap());
    }
    result
}
fn deadline_error(write: bool, started: bool) -> ApiError {
    let e = ApiError::new(504, "Unavailable", "deadline_exceeded");
    if !write {
        e.readonly()
    } else if started {
        e.unknown()
    } else {
        e
    }
}

async fn handle(State(app): State<Arc<App>>, request: Request) -> Response<Body> {
    let id = Uuid::new_v4().to_string();
    let path = request.uri().path().to_string();
    let write = WRITE_ROUTES.contains(&path.as_str());
    let read = |e: ApiError| if write { e } else { e.readonly() };
    if path != "/health/live" && !app.authorized(&request) {
        return error(
            read(ApiError::new(401, "PermissionDenied", "unauthenticated")),
            &id,
        );
    }
    if request.uri().query().is_some() {
        return error(read(ApiError::invalid()), &id);
    }
    let expected = if [
        "/health/live",
        "/health/ready",
        "/v1/info",
        "/v1/openapi.json",
    ]
    .contains(&path.as_str())
    {
        Method::GET
    } else if write || READ_ROUTES.contains(&path.as_str()) {
        Method::POST
    } else {
        return error(read(ApiError::missing()), &id);
    };
    if request.method() != expected {
        let mut response = error(
            read(ApiError::new(405, "InvalidArgument", "method_not_allowed")),
            &id,
        );
        response
            .headers_mut()
            .insert("allow", expected.as_str().parse().unwrap());
        return response;
    }
    if expected == Method::GET
        && (request.headers().contains_key("transfer-encoding")
            || request
                .headers()
                .get("content-length")
                .is_some_and(|h| h != "0"))
    {
        return error(read(ApiError::invalid()), &id);
    }
    let mut timeout_headers = request.headers().get_all("x-recern-timeout-ms").iter();
    let budget = if let Some(value) = timeout_headers.next() {
        let value = value
            .to_str()
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u64>().ok());
        match value {
            Some(v) if v > 0 && v <= app.limits.max_timeout_ms => v,
            _ => return error(read(ApiError::invalid()), &id),
        }
    } else {
        app.limits.default_timeout_ms
    };
    if timeout_headers.next().is_some() {
        return error(read(ApiError::invalid()), &id);
    }
    let deadline = Instant::now() + Duration::from_millis(budget);
    if path == "/health/live" {
        return response(200, br#"{"status":"alive"}"#.to_vec(), &id);
    }
    if app.draining.load(Ordering::Acquire) {
        return error(read(ApiError::unavailable()), &id);
    }
    if path == "/health/ready" {
        let ready = match app.store.try_lock() {
            Ok(store) => store.ready,
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(_)) => false,
        };
        return if ready {
            response(200, br#"{"status":"ready"}"#.to_vec(), &id)
        } else {
            error(read(ApiError::unavailable()), &id)
        };
    }
    if path == "/v1/openapi.json" {
        if OPENAPI.len() > app.limits.response {
            return error(ApiError::too_large().readonly(), &id);
        }
        return response(200, OPENAPI.as_bytes().to_vec(), &id);
    }
    let body = if expected == Method::POST {
        let content = request
            .headers()
            .get_all("content-type")
            .iter()
            .collect::<Vec<_>>();
        let valid = content.len() == 1
            && content[0].to_str().ok().is_some_and(|s| {
                let parts = s.split(';').map(str::trim).collect::<Vec<_>>();
                parts[0].eq_ignore_ascii_case("application/json")
                    && (parts.len() == 1
                        || (parts.len() == 2 && parts[1].eq_ignore_ascii_case("charset=utf-8")))
            });
        if !valid || request.headers().contains_key("content-encoding") {
            return error(
                read(ApiError::new(
                    415,
                    "InvalidArgument",
                    "unsupported_media_type",
                )),
                &id,
            );
        }
        if request
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > app.limits.request as u64)
        {
            return error(
                read(ApiError::new(413, "InvalidArgument", "request_too_large")),
                &id,
            );
        }
        let bytes = match timeout_at(
            deadline.into(),
            to_bytes(request.into_body(), app.limits.request),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => {
                return error(
                    read(ApiError::new(413, "InvalidArgument", "request_too_large")),
                    &id,
                );
            }
            Err(_) => {
                return error(
                    read(ApiError::new(408, "Unavailable", "request_timeout")),
                    &id,
                );
            }
        };
        // Parsing runs in the admitted blocking worker, not a Tokio IO task.
        bytes.to_vec()
    } else {
        b"{}".to_vec()
    };
    let admission = match app.admission.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return error(read(ApiError::new(429, "Unavailable", "overloaded")), &id),
    };
    let permit = match timeout_at(deadline.into(), app.active.clone().acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => return error(deadline_error(write, false), &id),
    };
    let phase = Arc::new(AtomicU8::new(0));
    let worker_phase = phase.clone();
    let worker_app = app.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let (_permit, _admission) = (permit, admission);
        let mut store = worker_app
            .store
            .lock()
            .map_err(|_| ApiError::unavailable().unknown())?;
        if Instant::now() >= deadline
            || worker_app.draining.load(Ordering::Acquire)
            || worker_phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(deadline_error(write, false));
        }
        let value = std::str::from_utf8(&body)
            .map_err(|_| ApiError::invalid())
            .and_then(|s| wire::strict_json(s, 16))?;
        if Instant::now() >= deadline {
            return Err(deadline_error(write, false));
        }
        let result = store.dispatch(&path, value, &worker_app.limits);
        worker_phase.store(2, Ordering::Release);
        result
    });
    match timeout_at(deadline.into(), worker).await {
        Ok(Ok(Ok((status, body)))) => response(status, body, &id),
        Ok(Ok(Err(e))) => error(read(e), &id),
        Ok(Err(_)) => error(read(ApiError::unavailable().unknown()), &id),
        Err(_) => {
            let queued = phase
                .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            error(deadline_error(write, !queued), &id)
        }
    }
}

pub async fn serve(
    listener: TcpListener,
    app: Arc<App>,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    let router = Router::new().fallback(handle).with_state(app.clone());
    let connections = Arc::new(Semaphore::new(64));
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _=&mut shutdown=>break,
            result=listener.accept()=>{
                let (socket,_)=result?;
                let Ok(permit)=connections.clone().try_acquire_owned() else {drop(socket);continue;};
                let service=TowerToHyperService::new(router.clone());
                tasks.spawn(async move {
                    let _permit=permit;
                    let mut builder=hyper::server::conn::http1::Builder::new();
                    builder.keep_alive(false).max_headers(64).max_buf_size(32768).timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5));
                    // Includes a bounded response-drain period. A dropped HTTP
                    // future does not cancel a started durable blocking worker.
                    let _=tokio::time::timeout(Duration::from_secs(45),builder.serve_connection(TokioIo::new(socket),service)).await;
                });
            },
            _=tasks.join_next(), if !tasks.is_empty()=>{},
        }
    }
    app.draining.store(true, Ordering::Release);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    app.drain().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn request(
        address: std::net::SocketAddr,
        path: &str,
        body: Value,
        budget: u64,
    ) -> (u16, Value) {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let payload = serde_json::to_vec(&body).unwrap();
        let header = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nX-Recern-Timeout-Ms: {budget}\r\nContent-Length: {}\r\n\r\n",
            "x".repeat(32),
            payload.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&payload).await.unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        let split = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
        let status = std::str::from_utf8(&bytes[..split])
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (status, serde_json::from_slice(&bytes[split + 4..]).unwrap())
    }
    fn setup() -> (tempfile::TempDir, Arc<App>, Value) {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("db.rvec")).unwrap();
        let created = store
            .dispatch(
                "/v1/collections/create",
                json!({"spec":{"name":"docs","dim":2}}),
                &Limits::default(),
            )
            .unwrap()
            .1;
        let created: Value = serde_json::from_slice(&created).unwrap();
        let reference = json!({"name":"docs","identity":created["collection"]["identity"]});
        let app = App::new(
            store,
            &[b'x'; 32],
            Limits {
                active: 1,
                queued: 0,
                ..Limits::default()
            },
        )
        .unwrap();
        (temp, app, reference)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_queued_write_is_not_executed_and_capacity_is_bounded() {
        let (_temp, app, reference) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(serve(listener, app.clone(), async {
            let _ = shutdown.await;
        }));
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let held = app.clone();
        let holder = std::thread::spawn(move || {
            let _guard = held.store.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        locked_rx.await.unwrap();
        let first = tokio::spawn(request(
            address,
            "/v1/records/upsert",
            json!({"collection":reference,"records":[{"id":"late","vector":[1,0]}]}),
            100,
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while app.admission.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            request(
                address,
                "/v1/records/count",
                json!({"collection":reference}),
                1000
            )
            .await
            .0,
            429
        );
        let (status, error) = first.await.unwrap();
        assert_eq!(status, 504);
        assert_eq!(error["error"]["write_outcome"], "not_committed");
        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move || holder.join().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while app.admission.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            request(
                address,
                "/v1/records/count",
                json!({"collection":reference}),
                1000
            )
            .await
            .1["count"],
            "0"
        );
        stop.send(()).unwrap();
        running.await.unwrap().unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn started_durable_write_survives_http_timeout_and_is_drained_on_shutdown() {
        let (_temp, app, reference) = setup();
        app.store.lock().unwrap().before_save_delay = Some(Duration::from_millis(200));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(serve(listener, app.clone(), async {
            let _ = shutdown.await;
        }));
        let (status, error) = request(
            address,
            "/v1/records/upsert",
            json!({"collection":reference,"records":[{"id":"committed","vector":[1,0]}]}),
            75,
        )
        .await;
        assert_eq!(status, 504);
        assert_eq!(error["error"]["write_outcome"], "unknown");
        assert_eq!(app.admission.available_permits(), 0);
        stop.send(()).unwrap();
        running.await.unwrap().unwrap();
        assert_eq!(app.admission.available_permits(), 1);
        let response = app
            .store
            .lock()
            .unwrap()
            .dispatch(
                "/v1/records/count",
                json!({"collection":reference}),
                &Limits::default(),
            )
            .unwrap()
            .1;
        assert_eq!(
            serde_json::from_slice::<Value>(&response).unwrap()["count"],
            "1"
        );
    }
}
