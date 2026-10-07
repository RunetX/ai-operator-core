//! Проверки сервера по HTTP без 1С: вместо неё подставные обработчики (`fake`). Клиент — голый
//! `TcpStream`: тестам нужен точный контроль над заголовками, а reqwest требует gcc.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_operator_mcp::server::{self, Options, Server, Shared};
use ai_operator_mcp::{fake, protocol};
use serde_json::{json, Value};

const TOKEN: &str = "test-token-0123456789abcdef";

struct Stand {
    server: Server,
    shared: Arc<Shared>,
    events: fake::Events,
}

impl Stand {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(configure: impl FnOnce(&mut Options)) -> Self {
        let (shared, events) = fake::start();
        shared.set_info(&json!({"name": "test", "version": "1", "instructions": "Инструкции."}).to_string()).unwrap();
        shared.set_tools(&fake::default_tools()).unwrap();
        let mut options = Options::new(0, TOKEN.into());
        configure(&mut options);
        let server = server::start(shared.clone(), options, "test").unwrap();
        Self { server, shared, events }
    }

    fn port(&self) -> u16 {
        self.server.port
    }

    fn post(&self, body: &Value, extra: &[(&str, &str)]) -> Reply {
        let text = body.to_string();
        let mut headers = vec![
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ];
        let auth = format!("Bearer {TOKEN}");
        headers.push(("Authorization", &auth));
        for (name, value) in extra {
            headers.retain(|(known, _)| !known.eq_ignore_ascii_case(name));
            if !value.is_empty() {
                headers.push((name, value));
            }
        }
        send(self.port(), "POST", "/mcp", &headers, text.as_bytes())
    }

    /// Открывает сессию и возвращает её идентификатор.
    fn session(&self) -> String {
        let reply = self.post(&initialize("2025-06-18"), &[]);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let session = reply.header("mcp-session-id").expect("сессия").to_string();
        let ack = self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &[("Mcp-Session-Id", &session)]);
        assert_eq!(ack.status, 202);
        session
    }

    fn call(&self, session: &str, id: u64, name: &str, arguments: Value, progress: bool) -> Reply {
        let mut params = json!({"name": name, "arguments": arguments});
        if progress {
            params["_meta"] = json!({"progressToken": format!("p{id}")});
        }
        self.post(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}), &[("Mcp-Session-Id", session)])
    }

    /// Ждёт, пока подставная 1С получит `count` событий TOOL_CALL, и возвращает номер последнего вызова.
    fn wait_tool_call(&self, count: usize) -> u64 {
        let started = Instant::now();
        loop {
            let calls: Vec<u64> = self
                .events
                .all()
                .into_iter()
                .filter(|(event, _)| event == server::EVENT_TOOL_CALL)
                .map(|(_, data)| serde_json::from_str::<Value>(&data).unwrap()["callId"].as_str().unwrap().parse().unwrap())
                .collect();
            if calls.len() >= count {
                return *calls.last().unwrap();
            }
            assert!(started.elapsed() < Duration::from_secs(5), "нет события TOOL_CALL");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_cancelled(&self) -> Value {
        let started = Instant::now();
        loop {
            if let Some(event) = self.events.cancelled().pop() {
                return event;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "нет события CANCELLED");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn initialize(version: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}})
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    /// Сообщения JSON-RPC из тела SSE или JSON.
    fn messages(&self) -> Vec<Value> {
        if self.header("content-type").is_some_and(|value| value.starts_with("text/event-stream")) {
            self.body
                .split("\n\n")
                .filter_map(|block| {
                    let data: Vec<&str> = block.lines().filter_map(|line| line.strip_prefix("data: ")).collect();
                    (!data.is_empty()).then(|| serde_json::from_str(&data.join("\n")).unwrap())
                })
                .collect()
        } else if self.body.trim().is_empty() {
            Vec::new()
        } else {
            vec![serde_json::from_str(&self.body).unwrap()]
        }
    }

    fn last(&self) -> Value {
        self.messages().pop().unwrap_or_else(|| panic!("нет сообщений: {} {}", self.status, self.body))
    }
}

fn send(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    send_raw(port, method, path, headers, body, Duration::from_secs(10))
}

fn send_raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8], timeout: Duration) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("host")) {
        request.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    stream.write_all(request.as_bytes()).unwrap();
    // Сервер вправе ответить 413 до того, как дочитает тело: ошибка записи тут не провал теста.
    let _ = stream.write_all(body);
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    parse(&raw)
}

fn parse(raw: &[u8]) -> Reply {
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n").expect("нет заголовков");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_string())))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(name, value)| name == "transfer-encoding" && value.contains("chunked")) {
        body = dechunk(&body);
    }
    Reply { status, headers, body: String::from_utf8(body).unwrap() }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    while let Some(end) = data.windows(2).position(|window| window == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&data[..end]).trim(), 16).unwrap_or(0);
        if size == 0 || data.len() < end + 2 + size {
            break;
        }
        result.extend_from_slice(&data[end + 2..end + 2 + size]);
        data = &data[(end + 4 + size).min(data.len())..];
    }
    result
}

#[test]
fn request_without_token_or_with_wrong_token_is_rejected() {
    let stand = Stand::new();
    for auth in ["", "Bearer wrong-token-0123456789abcd", "Basic dGVzdDp0ZXN0"] {
        let reply = stand.post(&initialize("2025-06-18"), &[("Authorization", auth)]);
        assert_eq!(reply.status, 401, "{auth}");
        assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    }
    assert!(stand.events.all().is_empty(), "запрос без токена дошёл до 1С");
}

#[test]
fn foreign_origin_and_host_are_rejected() {
    let stand = Stand::new();
    assert_eq!(stand.post(&initialize("2025-06-18"), &[("Origin", "https://evil.example")]).status, 403);
    assert_eq!(stand.post(&initialize("2025-06-18"), &[("Host", "evil.example:9874")]).status, 403);
    assert_eq!(stand.post(&initialize("2025-06-18"), &[("Origin", "http://localhost:3000")]).status, 200);
    let allowed = Stand::with(|options| options.origins = vec!["https://app.example".into()]);
    assert_eq!(allowed.post(&initialize("2025-06-18"), &[("Origin", "https://app.example")]).status, 200);
}

#[test]
fn listens_only_on_loopback() {
    let stand = Stand::new();
    let addresses: Vec<_> = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| socket.connect("192.0.2.1:9").map(|_| socket))
        .and_then(|socket| socket.local_addr())
        .into_iter()
        .map(|address| address.ip())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect();
    for ip in addresses {
        assert!(TcpStream::connect_timeout(&(ip, stand.port()).into(), Duration::from_millis(500)).is_err(), "слушает {ip}");
    }
}

#[test]
fn large_body_is_rejected() {
    let stand = Stand::with(|options| options.body_limit = 1024);
    let big = json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": {"x": "a".repeat(4096)}});
    assert_eq!(stand.post(&big, &[]).status, 413);
}

#[test]
fn version_negotiation_and_header() {
    let stand = Stand::new();
    for (requested, expected) in [("2025-03-26", "2025-03-26"), ("2025-11-25", "2025-06-18"), ("1999-01-01", "2025-06-18")] {
        let reply = stand.post(&initialize(requested), &[]);
        assert_eq!(reply.last()["result"]["protocolVersion"], expected, "{requested}");
    }
    let session = stand.session();
    let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
    assert_eq!(stand.post(&ping, &[("Mcp-Session-Id", &session), ("MCP-Protocol-Version", "1999-01-01")]).status, 400);
    assert_eq!(stand.post(&ping, &[("Mcp-Session-Id", &session), ("MCP-Protocol-Version", "")]).status, 200);
}

#[test]
fn initialize_returns_instructions_and_tools_capability() {
    let stand = Stand::new();
    let result = stand.post(&initialize("2025-06-18"), &[]).last()["result"].clone();
    assert_eq!(result["instructions"], "Инструкции.");
    assert_eq!(result["serverInfo"]["name"], "test");
    assert_eq!(result["capabilities"], json!({"tools": {"listChanged": false}}));
}

#[test]
fn sessions_are_required_and_can_be_closed() {
    let stand = Stand::new();
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    assert_eq!(stand.post(&list, &[]).status, 400);
    assert_eq!(stand.post(&list, &[("Mcp-Session-Id", "unknown")]).status, 404);
    let session = stand.session();
    assert_eq!(stand.post(&list, &[("Mcp-Session-Id", &session)]).status, 200);
    let auth = format!("Bearer {TOKEN}");
    let deleted = send(stand.port(), "DELETE", "/mcp", &[("Authorization", &auth), ("Mcp-Session-Id", &session)], b"");
    assert_eq!(deleted.status, 200);
    assert_eq!(stand.post(&list, &[("Mcp-Session-Id", &session)]).status, 404);
}

#[test]
fn tools_list_is_verbatim() {
    let stand = Stand::new();
    let session = stand.session();
    let reply = stand.post(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &[("Mcp-Session-Id", &session)]);
    let expected: Value = serde_json::from_str(&fake::default_tools()).unwrap();
    assert_eq!(reply.last()["result"]["tools"], expected);
    // Байты, а не только значения: порядок ключей схемы сохраняется.
    assert!(reply.body.contains(&fake::default_tools()));
}

#[test]
fn protocol_errors() {
    let stand = Stand::new();
    let session = stand.session();
    let headers = [("Mcp-Session-Id", session.as_str())];
    let unknown = stand.post(&json!({"jsonrpc": "2.0", "id": 3, "method": "probe/unknown"}), &headers);
    assert_eq!(unknown.last()["error"]["code"], protocol::METHOD_NOT_FOUND);
    let tool = stand.call(&session, 4, "no_such_tool", json!({}), false);
    assert_eq!(tool.last()["error"], json!({"code": -32602, "message": "tool not found"}));
    let batch = stand.post(&json!([{"jsonrpc": "2.0", "id": 5, "method": "ping"}]), &headers);
    assert_eq!(batch.status, 400);
    assert_eq!(batch.last()["error"]["code"], protocol::INVALID_REQUEST);
    let get = send(stand.port(), "GET", "/mcp", &[("Authorization", &format!("Bearer {TOKEN}"))], b"");
    assert_eq!(get.status, 405);
    let accept = stand.post(&json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}), &[("Mcp-Session-Id", &session), ("Accept", "application/json")]);
    assert_eq!(accept.status, 406);
    let ping = stand.post(&json!({"jsonrpc": "2.0", "id": "s-7", "method": "ping"}), &headers);
    assert_eq!(ping.last(), json!({"jsonrpc": "2.0", "id": "s-7", "result": {}}));
}

#[test]
fn immediate_and_error_answers() {
    let stand = Stand::new();
    let session = stand.session();
    let echo = stand.call(&session, 2, "echo", json!({"text": "привет"}), false);
    let text = echo.last()["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["arguments"]["text"], "привет");
    let fail = stand.call(&session, 3, "fail", json!({}), false);
    assert_eq!(fail.last()["error"], json!({"code": -32000, "message": "INTERNAL: проверочная ошибка"}));
}

#[test]
fn deferred_answer_arrives_when_1c_responds() {
    let stand = Stand::new();
    let session = stand.session();
    let port = stand.port();
    let session_for_client = session.clone();
    let client = std::thread::spawn(move || {
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "hang", "arguments": {}}}).to_string();
        send(port, "POST", "/mcp", &[("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"),
            ("Authorization", &auth), ("Mcp-Session-Id", &session_for_client)], body.as_bytes())
    });
    let call_id = stand.wait_tool_call(1);
    let data: Value = serde_json::from_str(&stand.shared.take_call(call_id).unwrap()).unwrap();
    assert_eq!(data, json!({"kind": "tool", "name": "hang", "arguments": {}, "progressToken": null}));
    std::thread::sleep(Duration::from_millis(200));
    assert!(stand.shared.respond_text(call_id, "готово"));
    let reply = client.join().unwrap();
    assert_eq!(reply.last(), json!({"jsonrpc": "2.0", "id": 9, "result": {"content": [{"type": "text", "text": "готово"}]}}));
    // Повторный и поздний ответ не роняют компоненту, а возвращают Ложь.
    assert!(!stand.shared.respond_text(call_id, "ещё раз"));
    assert!(!stand.shared.respond_error(call_id, -32000, "поздно"));
    assert!(stand.shared.take_call(call_id).is_none());
}

#[test]
fn progress_is_streamed_before_answer() {
    let stand = Stand::new();
    let session = stand.session();
    let reply = stand.call(&session, 5, "slow", json!({"seconds": 0.3, "steps": 3}), true);
    let messages = reply.messages();
    let progress: Vec<&Value> = messages.iter().filter(|m| m["method"] == "notifications/progress").collect();
    assert_eq!(progress.len(), 3, "{}", reply.body);
    assert_eq!(progress[0]["params"], json!({"progressToken": "p5", "progress": 1, "total": 3, "message": "шаг 1"}));
    assert_eq!(messages.last().unwrap()["id"], 5);
    // Без progressToken прогресс не шлётся.
    let quiet = stand.call(&session, 6, "slow", json!({"seconds": 0.1, "steps": 2}), false);
    assert_eq!(quiet.messages().len(), 1);
}

#[test]
fn progress_must_increase() {
    let stand = Stand::new();
    let session = stand.session();
    let port = stand.port();
    let session_for_client = session.clone();
    let client = std::thread::spawn(move || {
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "hang", "arguments": {}, "_meta": {"progressToken": 42}}}).to_string();
        send(port, "POST", "/mcp", &[("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"),
            ("Authorization", &auth), ("Mcp-Session-Id", &session_for_client)], body.as_bytes())
    });
    let call_id = stand.wait_tool_call(1);
    assert!(stand.shared.progress(call_id, 1.0, None, None));
    assert!(!stand.shared.progress(call_id, 1.0, None, None));
    assert!(!stand.shared.progress(call_id, 0.5, None, None));
    assert!(stand.shared.progress(call_id, 2.0, Some(2.0), Some("всё")));
    assert!(stand.shared.respond_text(call_id, "ok"));
    let messages = client.join().unwrap().messages();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["params"]["progressToken"], 42);
}

#[test]
fn keepalive_comments_while_waiting() {
    let stand = Stand::with(|options| options.keepalive = Duration::from_millis(100));
    let session = stand.session();
    let reply = stand.call(&session, 2, "slow", json!({"seconds": 0.5, "steps": 1}), false);
    assert!(reply.body.matches(": keepalive").count() >= 3, "{}", reply.body);
    assert_eq!(reply.last()["id"], 2);
}

#[test]
fn client_cancellation_reaches_1c_and_ends_stream() {
    let stand = Stand::new();
    let session = stand.session();
    let port = stand.port();
    let session_for_client = session.clone();
    let client = std::thread::spawn(move || {
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {"name": "hang", "arguments": {}}}).to_string();
        send(port, "POST", "/mcp", &[("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"),
            ("Authorization", &auth), ("Mcp-Session-Id", &session_for_client)], body.as_bytes())
    });
    let call_id = stand.wait_tool_call(1);
    // Отмена из другой сессии с тем же id не трогает вызов.
    let other = stand.session();
    let cancel = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 11, "reason": "user"}});
    assert_eq!(stand.post(&cancel, &[("Mcp-Session-Id", &other)]).status, 202);
    std::thread::sleep(Duration::from_millis(100));
    assert!(stand.events.cancelled().is_empty());
    assert_eq!(stand.post(&cancel, &[("Mcp-Session-Id", &session)]).status, 202);
    assert_eq!(stand.wait_cancelled(), json!({"callId": call_id.to_string(), "reason": "client"}));
    let reply = client.join().unwrap();
    assert!(reply.messages().is_empty(), "после отмены ответа нет: {}", reply.body);
    assert!(!stand.shared.respond_text(call_id, "поздно"));
}

#[test]
fn timeout_answers_error_and_tells_1c() {
    let stand = Stand::with(|options| options.call_timeout = Duration::from_millis(300));
    let session = stand.session();
    let reply = stand.call(&session, 12, "hang", json!({}), false);
    assert_eq!(reply.last()["error"]["code"], protocol::REQUEST_TIMEOUT);
    let call_id = stand.wait_tool_call(1);
    assert_eq!(stand.wait_cancelled(), json!({"callId": call_id.to_string(), "reason": "timeout"}));
    assert!(!stand.shared.respond_text(call_id, "поздно"));
}

#[test]
fn parallel_calls_get_their_own_answers() {
    let stand = Stand::new();
    let session = stand.session();
    let port = stand.port();
    let clients: Vec<_> = (0..20)
        .map(|index| {
            let session = session.clone();
            std::thread::spawn(move || {
                let auth = format!("Bearer {TOKEN}");
                let body = json!({"jsonrpc": "2.0", "id": 100 + index, "method": "tools/call",
                    "params": {"name": "echo", "arguments": {"text": format!("n{index}")}}}).to_string();
                let reply = send(port, "POST", "/mcp", &[("Content-Type", "application/json"),
                    ("Accept", "application/json, text/event-stream"), ("Authorization", &auth), ("Mcp-Session-Id", &session)], body.as_bytes());
                (index, reply.last())
            })
        })
        .collect();
    for client in clients {
        let (index, message) = client.join().unwrap();
        assert_eq!(message["id"], 100 + index);
        let text: Value = serde_json::from_str(message["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["arguments"]["text"], format!("n{index}"));
    }
}

#[test]
fn stop_frees_port_and_fails_pending_calls() {
    let mut stand = Stand::new();
    let session = stand.session();
    let port = stand.port();
    let session_for_client = session.clone();
    let client = std::thread::spawn(move || {
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"jsonrpc": "2.0", "id": 13, "method": "tools/call", "params": {"name": "hang", "arguments": {}}}).to_string();
        send(port, "POST", "/mcp", &[("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"),
            ("Authorization", &auth), ("Mcp-Session-Id", &session_for_client)], body.as_bytes())
    });
    stand.wait_tool_call(1);
    let started = Instant::now();
    stand.server.stop();
    assert!(started.elapsed() < Duration::from_secs(2), "остановка {:?}", started.elapsed());
    assert_eq!(client.join().unwrap().last()["error"]["code"], -32000);
    assert!(!stand.shared.is_running());
    // Порт свободен: новый сервер встаёт на него же, многократно.
    for _ in 0..20 {
        let mut options = Options::new(port, TOKEN.into());
        options.keepalive = Duration::from_secs(1);
        let mut again = server::start(stand.shared.clone(), options, "test").expect("порт освободился");
        again.stop();
    }
}

#[test]
fn busy_port_is_reported() {
    let stand = Stand::new();
    let (shared, _) = fake::start();
    let error = server::start(shared, Options::new(stand.port(), TOKEN.into()), "test").err().expect("порт занят");
    assert_eq!(error.code, "PORT_BUSY");
}
