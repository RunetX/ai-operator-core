//! Сервер MCP Streamable HTTP на `127.0.0.1`.
//!
//! Компонента живёт в процессе клиента 1С. Сервер работает в своём потоке с однопоточным рантаймом tokio.
//! Вызов инструмента уходит в 1С внешним событием `TOOL_CALL` с одним номером вызова: данные 1С забирает
//! сама (`Shared::take_call`), поэтому потерянное событие не теряет аргументы. Ответ, прогресс и ошибку
//! 1С передаёт методами `Shared` из своего потока; они только кладут сообщение в канал и сразу возвращаются.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener as StdListener};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body::{Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::log;
use crate::protocol::{self, ServerInfo, Tools};

pub const EVENT_SOURCE: &str = "AiOperatorMcp";
pub const EVENT_TOOL_CALL: &str = "TOOL_CALL";
pub const EVENT_CANCELLED: &str = "CANCELLED";

const MAX_SESSIONS: usize = 64;
const MCP_PATH: &str = "/mcp";

/// Куда сервер отдаёт события для 1С. В компоненте — поток, вызывающий `ExternalEvent`, в тестах — заглушка.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &'static str, data: String);
}

#[derive(Debug, Clone)]
pub struct Options {
    pub port: u16,
    /// Источники (Origin) сверх localhost, которым разрешены запросы браузера.
    pub origins: Vec<String>,
    pub call_timeout: Duration,
    pub keepalive: Duration,
    pub body_limit: usize,
    pub token: String,
}

impl Options {
    pub fn new(port: u16, token: String) -> Self {
        Self {
            port,
            origins: Vec::new(),
            call_timeout: Duration::from_secs(900),
            keepalive: Duration::from_secs(20),
            body_limit: 4 * 1024 * 1024,
            token,
        }
    }
}

enum Outgoing {
    Message(String),
    Final(String),
    Close,
}

struct Call {
    session: String,
    request_id: Value,
    name: String,
    arguments: Value,
    progress_token: Option<Value>,
    last_progress: Option<f64>,
    tx: mpsc::UnboundedSender<Outgoing>,
}

struct Session {
    last_used: Instant,
}

#[derive(Default)]
struct State {
    info: ServerInfo,
    tools: Tools,
    sessions: HashMap<String, Session>,
    calls: HashMap<u64, Call>,
    next_call: u64,
    running: Option<(u16, &'static str)>,
}

/// Общее состояние компоненты: сведения о сервере, инструменты, сессии и вызовы, которые ждут ответа 1С.
pub struct Shared {
    state: Mutex<State>,
    sink: Arc<dyn EventSink>,
}

impl Shared {
    pub fn new(sink: Arc<dyn EventSink>) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State::default()), sink })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Паника в другом потоке не должна делать компоненту неработоспособной.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_info(&self, text: &str) -> Result<(), String> {
        let info = ServerInfo::parse(text)?;
        self.state().info = info;
        Ok(())
    }

    pub fn set_tools(&self, text: &str) -> Result<usize, String> {
        let tools = Tools::parse(text)?;
        let count = tools.names.len();
        self.state().tools = tools;
        Ok(count)
    }

    /// Данные вызова для 1С: `{kind, name, arguments, progressToken}`. Пусто, если вызова уже нет.
    pub fn take_call(&self, call_id: u64) -> Option<String> {
        let state = self.state();
        let call = state.calls.get(&call_id)?;
        Some(
            json!({
                "kind": "tool",
                "name": call.name,
                "arguments": call.arguments,
                "progressToken": call.progress_token,
            })
            .to_string(),
        )
    }

    /// Успешный ответ инструмента. Ложь — вызова уже нет: отменён клиентом, истёк или уже завершён.
    pub fn respond_text(&self, call_id: u64, text: &str) -> bool {
        self.finish(call_id, |id| protocol::tool_text_result(id, text))
    }

    /// Ответ ошибкой JSON-RPC (сбои, которые модель исправить не может).
    pub fn respond_error(&self, call_id: u64, code: i64, message: &str) -> bool {
        self.finish(call_id, |id| protocol::error(id, code, message))
    }

    fn finish(&self, call_id: u64, message: impl FnOnce(&Value) -> String) -> bool {
        let Some(call) = self.state().calls.remove(&call_id) else {
            return false;
        };
        call.tx.send(Outgoing::Final(message(&call.request_id))).is_ok()
    }

    /// Уведомление о прогрессе. Шлётся, только если клиент передал `progressToken`; значения строго растут.
    pub fn progress(&self, call_id: u64, progress: f64, total: Option<f64>, message: Option<&str>) -> bool {
        let mut state = self.state();
        let Some(call) = state.calls.get_mut(&call_id) else {
            return false;
        };
        let Some(token) = call.progress_token.clone() else {
            return false;
        };
        if !progress.is_finite() || call.last_progress.is_some_and(|last| progress <= last) {
            return false;
        }
        call.last_progress = Some(progress);
        let text = protocol::progress_notification(&token, progress, total.filter(|value| value.is_finite()), message);
        call.tx.send(Outgoing::Message(text)).is_ok()
    }

    pub fn status(&self) -> String {
        let state = self.state();
        json!({
            "running": state.running.is_some(),
            "port": state.running.map(|(port, _)| port),
            "tokenSource": state.running.map(|(_, source)| source),
            "sessions": state.sessions.len(),
            "calls": state.calls.len(),
            "tools": state.tools.names.len(),
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string()
    }

    pub fn is_running(&self) -> bool {
        self.state().running.is_some()
    }

    /// Вызов истёк по таймауту сервера: 1С получает событие отмены, клиент — ошибку.
    fn expire(&self, call_id: u64) -> bool {
        let removed = self.state().calls.remove(&call_id);
        if removed.is_some() {
            log::warn(format!("вызов {call_id}: таймаут"));
            self.sink.emit(EVENT_CANCELLED, json!({"callId": call_id.to_string(), "reason": "timeout"}).to_string());
        }
        removed.is_some()
    }

    fn cancel_request(&self, session: &str, request_id: &Value) {
        let removed = {
            let mut state = self.state();
            let found = state
                .calls
                .iter()
                .find(|(_, call)| call.session == session && &call.request_id == request_id)
                .map(|(id, _)| *id);
            found.and_then(|id| state.calls.remove(&id).map(|call| (id, call)))
        };
        if let Some((call_id, call)) = removed {
            let _ = call.tx.send(Outgoing::Close);
            log::info(format!("вызов {call_id}: отменён клиентом"));
            self.sink.emit(EVENT_CANCELLED, json!({"callId": call_id.to_string(), "reason": "client"}).to_string());
        }
    }

    fn open_session(&self) -> String {
        let id = session_id();
        let mut state = self.state();
        if state.sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) =
                state.sessions.iter().min_by_key(|(_, session)| session.last_used).map(|(id, _)| id.clone())
            {
                state.sessions.remove(&oldest);
            }
        }
        state.sessions.insert(id.clone(), Session { last_used: Instant::now() });
        id
    }

    fn touch_session(&self, id: &str) -> bool {
        match self.state().sessions.get_mut(id) {
            Some(session) => {
                session.last_used = Instant::now();
                true
            }
            None => false,
        }
    }

    fn close_session(&self, id: &str) -> bool {
        self.state().sessions.remove(id).is_some()
    }

    /// Остановка сервера: ждущие вызовы завершаются ошибкой, сессии забываются.
    fn reset(&self) {
        let calls: Vec<Call> = {
            let mut state = self.state();
            state.running = None;
            state.sessions.clear();
            state.calls.drain().map(|(_, call)| call).collect()
        };
        for call in calls {
            let _ = call.tx.send(Outgoing::Final(protocol::error(
                &call.request_id,
                -32000,
                "INTERNAL: сервер MCP остановлен",
            )));
        }
    }
}

fn session_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Запасной путь: сессия — не секрет (секрет — токен), но должна быть уникальной.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        bytes.copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug)]
pub struct StartError {
    pub code: &'static str,
    pub message: String,
}

/// Работающий сервер. `stop` (или `Drop`) останавливает его и освобождает порт.
pub struct Server {
    shared: Arc<Shared>,
    shutdown: Option<oneshot::Sender<()>>,
    finished: std::sync::mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

pub fn start(shared: Arc<Shared>, options: Options, token_source: &'static str) -> Result<Server, StartError> {
    if shared.is_running() {
        return Err(StartError { code: "ALREADY_RUNNING", message: "сервер уже запущен".into() });
    }
    let listener = StdListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, options.port)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse || error.raw_os_error() == Some(10048) {
            StartError { code: "PORT_BUSY", message: format!("порт {} занят", options.port) }
        } else {
            StartError { code: "BIND_FAILED", message: format!("порт {}: {error}", options.port) }
        }
    })?;
    let port = listener.local_addr().map(|address| address.port()).unwrap_or(options.port);
    listener
        .set_nonblocking(true)
        .map_err(|error| StartError { code: "BIND_FAILED", message: error.to_string() })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| StartError { code: "RUNTIME", message: error.to_string() })?;

    let options = Arc::new(Options { port, ..options });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let thread_shared = shared.clone();
    let thread = std::thread::Builder::new()
        .name("mcp-transport".into())
        .spawn(move || {
            runtime.block_on(accept_loop(thread_shared, options, listener, shutdown_rx));
            runtime.shutdown_timeout(Duration::from_millis(500));
            let _ = finished_tx.send(());
        })
        .map_err(|error| StartError { code: "RUNTIME", message: error.to_string() })?;

    shared.state().running = Some((port, token_source));
    log::info(format!("сервер запущен: 127.0.0.1:{port}, токен: {token_source}"));
    Ok(Server { shared, shutdown: Some(shutdown_tx), finished: finished_rx, thread: Some(thread), port })
}

impl Server {
    pub fn stop(&mut self) {
        let Some(shutdown) = self.shutdown.take() else { return };
        self.shared.reset();
        let _ = shutdown.send(());
        // Поток сервера не должен держать 1С: ждём не дольше 3 секунд, затем отпускаем его.
        if self.finished.recv_timeout(Duration::from_secs(3)).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            log::warn("сервер не остановился за 3 с");
        }
        log::info(format!("сервер остановлен: порт {}", self.port));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(
    shared: Arc<Shared>,
    options: Arc<Options>,
    listener: StdListener,
    mut shutdown: oneshot::Receiver<()>,
) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn(format!("сервер: {error}"));
            return;
        }
    };
    // Остановка мягкая: соединения дописывают начатые ответы (ошибки ждущих вызовов), простаивающие
    // закрываются сразу. Счётчик соединений показывает, когда можно гасить рантайм.
    let (closing_tx, closing_rx) = tokio::sync::watch::channel(false);
    let open = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    // Без этого Nagle и отложенный ACK дают на loopback задержки около 200 мс.
                    let _ = stream.set_nodelay(true);
                    let shared = shared.clone();
                    let options = options.clone();
                    let mut closing = closing_rx.clone();
                    let guard = ConnectionGuard::new(open.clone());
                    tokio::spawn(async move {
                        let _guard = guard;
                        let service = hyper::service::service_fn(move |request| {
                            let shared = shared.clone();
                            let options = options.clone();
                            async move { Ok::<_, Infallible>(handle(shared, options, request).await) }
                        });
                        let connection = hyper::server::conn::http1::Builder::new()
                            .keep_alive(true)
                            .serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = connection.as_mut() => return,
                            _ = closing.wait_for(|closing| *closing) => connection.as_mut().graceful_shutdown(),
                        }
                        let _ = connection.await;
                    });
                }
                Err(error) => {
                    log::warn(format!("приём соединения: {error}"));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
    drop(listener);
    let _ = closing_tx.send(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while open.load(std::sync::atomic::Ordering::SeqCst) > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

struct ConnectionGuard(Arc<std::sync::atomic::AtomicUsize>);

impl ConnectionGuard {
    fn new(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Тело ответа: целиком или потоком SSE.
pub enum Body {
    Full(Option<Bytes>),
    Stream(mpsc::UnboundedReceiver<Bytes>),
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            Body::Full(data) => Poll::Ready(data.take().map(|data| Ok(Frame::data(data)))),
            Body::Stream(receiver) => receiver.poll_recv(cx).map(|item| item.map(|data| Ok(Frame::data(data)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self, Body::Full(None))
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Body::Full(Some(data)) => SizeHint::with_exact(data.len() as u64),
            Body::Full(None) => SizeHint::with_exact(0),
            Body::Stream(_) => SizeHint::default(),
        }
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from_static(text.as_bytes()))));
    *response.status_mut() = status;
    response.headers_mut().insert("content-type", HeaderValue::from_static("text/plain; charset=utf-8"));
    response
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::Full(None));
    *response.status_mut() = status;
    response
}

fn json_error(status: StatusCode, id: &Value, code: i64, message: &str) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from(protocol::error(id, code, message)))));
    *response.status_mut() = status;
    response.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
    response
}

/// Ответ одним событием SSE: так отвечала и прежняя компонента, клиенты MCP обязаны принимать оба вида.
fn sse_once(message: String) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from(protocol::sse_event(&message)))));
    set_sse_headers(response.headers_mut());
    response
}

fn set_sse_headers(headers: &mut HeaderMap) {
    headers.insert("content-type", HeaderValue::from_static("text/event-stream"));
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
}

async fn handle(shared: Arc<Shared>, options: Arc<Options>, request: Request<Incoming>) -> Response<Body> {
    let started = Instant::now();
    let http_method = request.method().clone();
    let (response, label) = route(&shared, &options, request).await;
    let elapsed = started.elapsed().as_millis();
    let status = response.status().as_u16();
    if status >= 400 {
        log::warn(format!("{http_method} {label} {status} {elapsed} мс"));
    } else if label != "notification" {
        log::info(format!("{http_method} {label} {status} {elapsed} мс"));
    }
    response
}

async fn route(shared: &Arc<Shared>, options: &Arc<Options>, request: Request<Incoming>) -> (Response<Body>, String) {
    if request.uri().path() != MCP_PATH {
        return (plain(StatusCode::NOT_FOUND, "Not Found"), "path".into());
    }
    let headers = request.headers();
    if !host_allowed(headers, options.port) {
        return (plain(StatusCode::FORBIDDEN, "Forbidden: Host is not allowed"), "host".into());
    }
    if !origin_allowed(headers, &options.origins) {
        return (plain(StatusCode::FORBIDDEN, "Forbidden: Origin is not allowed"), "origin".into());
    }
    if !authorized(headers, &options.token) {
        let mut response = plain(StatusCode::UNAUTHORIZED, "Unauthorized");
        response.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer"));
        return (response, "auth".into());
    }
    match *request.method() {
        Method::POST => post(shared, options, request).await,
        Method::DELETE => {
            let closed = header(request.headers(), "mcp-session-id").is_some_and(|id| shared.close_session(id));
            let status = if closed { StatusCode::OK } else { StatusCode::NOT_FOUND };
            (empty(status), "delete".into())
        }
        _ => {
            // Поток GET от сервера к клиенту отложен: 405 разрешён спецификацией.
            let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
            response.headers_mut().insert("allow", HeaderValue::from_static("POST, DELETE"));
            (response, "method".into())
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn host_allowed(headers: &HeaderMap, port: u16) -> bool {
    // Проверка Host защищает от DNS rebinding: браузер со страницы evil.example пришлёт Host: evil.example:порт.
    let Some(host) = header(headers, "host") else { return true };
    let host = host.to_ascii_lowercase();
    let port_suffix = format!(":{port}");
    let name = host.strip_suffix(&port_suffix).unwrap_or(&host);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn origin_allowed(headers: &HeaderMap, extra: &[String]) -> bool {
    let Some(origin) = header(headers, "origin") else { return true };
    if extra.iter().any(|allowed| allowed == "*" || allowed.eq_ignore_ascii_case(origin)) {
        return true;
    }
    let origin = origin.to_ascii_lowercase();
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    let host = if rest.starts_with('[') {
        rest.split_inclusive(']').next().unwrap_or(rest)
    } else {
        rest.split(':').next().unwrap_or(rest)
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    use subtle::ConstantTimeEq;
    let Some(value) = header(headers, "authorization") else { return false };
    let Some((scheme, presented)) = value.split_once(' ') else { return false };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    let presented = presented.trim().as_bytes();
    presented.len() == token.len() && bool::from(presented.ct_eq(token.as_bytes()))
}

fn accepts_both(headers: &HeaderMap) -> bool {
    let accept = header(headers, "accept").unwrap_or("").to_ascii_lowercase();
    let any = accept.contains("*/*");
    (any || accept.contains("application/json")) && (any || accept.contains("text/event-stream"))
}

async fn post(shared: &Arc<Shared>, options: &Arc<Options>, request: Request<Incoming>) -> (Response<Body>, String) {
    let headers = request.headers();
    let content_type = header(headers, "content-type").unwrap_or("").to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return (
            plain(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type: Content-Type must be application/json"),
            "content-type".into(),
        );
    }
    if !accepts_both(headers) {
        return (
            plain(StatusCode::NOT_ACCEPTABLE, "Not Acceptable: Client must accept both application/json and text/event-stream"),
            "accept".into(),
        );
    }
    if let Some(version) = header(headers, "mcp-protocol-version") {
        if !protocol::is_supported(version) {
            return (plain(StatusCode::BAD_REQUEST, "Bad Request: Unsupported MCP-Protocol-Version"), "version".into());
        }
    }
    let session_header = header(headers, "mcp-session-id").map(str::to_string);
    let declared = header(headers, "content-length").and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > options.body_limit) {
        return (plain(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large"), "body".into());
    }
    let body = match http_body_util::Limited::new(request.into_body(), options.body_limit).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return (plain(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large"), "body".into()),
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return (json_error(StatusCode::BAD_REQUEST, &Value::Null, protocol::PARSE_ERROR, "Parse error"), "parse".into()),
    };
    if message.is_array() {
        return (
            json_error(StatusCode::BAD_REQUEST, &Value::Null, protocol::INVALID_REQUEST, "Batch requests are not supported"),
            "batch".into(),
        );
    }
    let id = message.get("id").cloned();
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return (
            json_error(StatusCode::BAD_REQUEST, id.as_ref().unwrap_or(&Value::Null), protocol::INVALID_REQUEST, "Invalid Request"),
            "invalid".into(),
        );
    }
    let method = message.get("method").and_then(Value::as_str).map(str::to_string);
    let Some(method) = method else {
        // Ответ клиента на запрос сервера: таких запросов сервер не шлёт.
        return (empty(StatusCode::ACCEPTED), "response".into());
    };
    if method == "initialize" && id.is_some() {
        let requested = message.pointer("/params/protocolVersion").and_then(Value::as_str);
        let version = protocol::negotiate(requested);
        let session = shared.open_session();
        let text = {
            let state = shared.state();
            protocol::initialize_result(id.as_ref().unwrap(), version, &state.info)
        };
        let mut response = sse_once(text);
        if let Ok(value) = HeaderValue::from_str(&session) {
            response.headers_mut().insert("mcp-session-id", value);
        }
        return (response, format!("initialize {version}"));
    }

    let Some(session) = session_header else {
        return (plain(StatusCode::BAD_REQUEST, "Bad Request: Mcp-Session-Id header is required"), method);
    };
    if !shared.touch_session(&session) {
        return (plain(StatusCode::NOT_FOUND, "Not Found: Session not found"), method);
    }
    let Some(id) = id else {
        if method == "notifications/cancelled" {
            if let Some(request_id) = message.pointer("/params/requestId") {
                shared.cancel_request(&session, request_id);
            }
        }
        return (empty(StatusCode::ACCEPTED), "notification".into());
    };
    match method.as_str() {
        "ping" => (sse_once(protocol::result(&id, json!({}))), method),
        "tools/list" => {
            let text = protocol::tools_list_result(&id, &shared.state().tools);
            (sse_once(text), method)
        }
        "tools/call" => tools_call(shared, options, session, id, &message),
        _ => (sse_once(protocol::error(&id, protocol::METHOD_NOT_FOUND, "Method not found")), method),
    }
}

fn tools_call(
    shared: &Arc<Shared>,
    options: &Arc<Options>,
    session: String,
    id: Value,
    message: &Value,
) -> (Response<Body>, String) {
    let params = message.get("params");
    let Some(name) = params.and_then(|params| params.get("name")).and_then(Value::as_str) else {
        return (
            sse_once(protocol::error(&id, protocol::INVALID_PARAMS, "Invalid params: name is required")),
            "tools/call".into(),
        );
    };
    let label = format!("tools/call {name}");
    let arguments = params.and_then(|params| params.get("arguments")).cloned().unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return (
            sse_once(protocol::error(&id, protocol::INVALID_PARAMS, "Invalid params: arguments must be an object")),
            label,
        );
    }
    let progress_token = params
        .and_then(|params| params.pointer("/_meta/progressToken"))
        .filter(|token| token.is_string() || token.is_number())
        .cloned();
    let (tx, rx) = mpsc::unbounded_channel();
    let call_id = {
        let mut state = shared.state();
        // Прежняя компонента отвечала на неизвестный инструмент так же: модель видит этот текст.
        if !state.tools.contains(name) {
            return (sse_once(protocol::error(&id, protocol::INVALID_PARAMS, "tool not found")), label);
        }
        state.next_call += 1;
        let call_id = state.next_call;
        state.calls.insert(
            call_id,
            Call {
                session,
                request_id: id.clone(),
                name: name.to_string(),
                arguments,
                progress_token,
                last_progress: None,
                tx,
            },
        );
        call_id
    };
    shared.sink.emit(EVENT_TOOL_CALL, json!({"callId": call_id.to_string()}).to_string());

    let (body_tx, body_rx) = mpsc::unbounded_channel();
    tokio::spawn(stream_call(shared.clone(), call_id, id, rx, body_tx, options.call_timeout, options.keepalive));
    let mut response = Response::new(Body::Stream(body_rx));
    set_sse_headers(response.headers_mut());
    (response, label)
}

/// Поток SSE одного вызова: прогресс, keepalive-комментарии (иначе клиенты на Node рвут соединение через
/// 300 с без данных), затем ответ. Разрыв соединения клиентом — не отмена (спецификация 2025-06-18):
/// вызов ждёт ответа 1С, ответ просто некуда отдать.
async fn stream_call(
    shared: Arc<Shared>,
    call_id: u64,
    request_id: Value,
    mut rx: mpsc::UnboundedReceiver<Outgoing>,
    body: mpsc::UnboundedSender<Bytes>,
    timeout: Duration,
    keepalive: Duration,
) {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut expired = false;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + keepalive, keepalive);
    loop {
        tokio::select! {
            message = rx.recv() => match message {
                Some(Outgoing::Message(text)) => { let _ = body.send(Bytes::from(protocol::sse_event(&text))); }
                Some(Outgoing::Final(text)) => { let _ = body.send(Bytes::from(protocol::sse_event(&text))); break; }
                Some(Outgoing::Close) | None => break,
            },
            _ = tick.tick() => { let _ = body.send(Bytes::from_static(b": keepalive\n\n")); }
            _ = &mut deadline, if !expired => {
                expired = true;
                if shared.expire(call_id) {
                    let text = protocol::error(&request_id, protocol::REQUEST_TIMEOUT, "Request timed out");
                    let _ = body.send(Bytes::from(protocol::sse_event(&text)));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn bearer_must_match_exactly() {
        let token = "secret-token-0123456789";
        assert!(authorized(&headers(&[("authorization", "Bearer secret-token-0123456789")]), token));
        assert!(authorized(&headers(&[("authorization", "bearer secret-token-0123456789")]), token));
        assert!(!authorized(&headers(&[("authorization", "Bearer secret-token-012345678")]), token));
        assert!(!authorized(&headers(&[("authorization", "Bearer secret-token-0123456789x")]), token));
        assert!(!authorized(&headers(&[("authorization", "Basic secret-token-0123456789")]), token));
        assert!(!authorized(&headers(&[]), token));
    }

    #[test]
    fn origin_rules() {
        assert!(origin_allowed(&headers(&[]), &[]));
        assert!(origin_allowed(&headers(&[("origin", "http://localhost")]), &[]));
        assert!(origin_allowed(&headers(&[("origin", "http://127.0.0.1:3000")]), &[]));
        assert!(origin_allowed(&headers(&[("origin", "http://[::1]:3000")]), &[]));
        assert!(!origin_allowed(&headers(&[("origin", "https://evil.example")]), &[]));
        assert!(!origin_allowed(&headers(&[("origin", "http://localhost.evil.example")]), &[]));
        assert!(!origin_allowed(&headers(&[("origin", "null")]), &[]));
        assert!(origin_allowed(&headers(&[("origin", "https://app.example")]), &["https://app.example".into()]));
    }

    #[test]
    fn host_rules() {
        assert!(host_allowed(&headers(&[("host", "127.0.0.1:9874")]), 9874));
        assert!(host_allowed(&headers(&[("host", "localhost:9874")]), 9874));
        assert!(host_allowed(&headers(&[]), 9874));
        assert!(!host_allowed(&headers(&[("host", "evil.example:9874")]), 9874));
        assert!(!host_allowed(&headers(&[("host", "127.0.0.1.evil.example")]), 9874));
    }
}
