//! Отладочный сервер: та же библиотека, что в компоненте, но вместо 1С — подставные обработчики
//! (`fake`). На нём гоняются `tests/e2e/mcp_client.py`, `transport_probe.py` и клиенты MCP без 1С.
//!
//!     mcp-transport-dev [--port 9876] [--snapshot build/mcp-full-acc.json] [--timeout 900] [--keepalive 20]
//!
//! С `--snapshot` сервер отдаёт instructions и инструменты из снимка: так проверяется, что компонента
//! передаёт модели ровно то, что прислало ядро (`mcp_snapshot.py --compare`).

use std::time::Duration;

use ai_operator_mcp::{fake, server, token};
use serde_json::{json, Value};

fn main() {
    let mut port = 9876u16;
    let mut snapshot: Option<String> = None;
    let mut timeout = 900u64;
    let mut keepalive = 20u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage(&format!("нет значения для {arg}")));
        match arg.as_str() {
            "--port" => port = value().parse().unwrap_or_else(|_| usage("порт")),
            "--snapshot" => snapshot = Some(value()),
            "--timeout" => timeout = value().parse().unwrap_or_else(|_| usage("таймаут")),
            "--keepalive" => keepalive = value().parse().unwrap_or_else(|_| usage("keepalive")),
            _ => usage(&format!("неизвестный ключ {arg}")),
        }
    }

    let (shared, _events) = fake::start();
    let (instructions, tools) = match &snapshot {
        Some(path) => {
            let text = std::fs::read_to_string(path).unwrap_or_else(|error| usage(&format!("{path}: {error}")));
            let value: Value = serde_json::from_str(&text).unwrap_or_else(|error| usage(&format!("{path}: {error}")));
            (value["instructions"].as_str().unwrap_or_default().to_string(), value["tools"].to_string())
        }
        None => ("Отладочный сервер mcp-transport-dev.".to_string(), fake::default_tools()),
    };
    shared
        .set_info(
            &json!({
                "name": "1c-ai-operator",
                "version": env!("CARGO_PKG_VERSION"),
                "title": "ИИ-оператор 1С (отладка)",
                "description": "mcp-transport-dev",
                "instructions": instructions,
            })
            .to_string(),
        )
        .expect("сведения о сервере");
    let count = shared.set_tools(&tools).unwrap_or_else(|error| usage(&error));

    let (token, source) = token::load().unwrap_or_else(|error| usage(&error.to_string()));
    let mut options = server::Options::new(port, token);
    options.call_timeout = Duration::from_secs(timeout);
    options.keepalive = Duration::from_secs(keepalive);
    let server = server::start(shared, options, source.as_str())
        .unwrap_or_else(|error| usage(&format!("{}: {}", error.code, error.message)));
    println!("mcp-transport-dev: http://127.0.0.1:{}/mcp, инструментов {count}, токен: {}", server.port, source.as_str());
    loop {
        std::thread::park();
    }
}

fn usage(message: &str) -> ! {
    eprintln!("mcp-transport-dev: {message}");
    eprintln!("ключи: --port N --snapshot файл.json --timeout сек --keepalive сек");
    std::process::exit(2);
}
