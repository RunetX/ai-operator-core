//! Подставная «1С» для тестов и отладочного сервера `mcp-transport-dev`: принимает события так же,
//! как модуль ядра, и отвечает методами `Shared` из своих потоков. Код компоненты при этом тот же.
//!
//! Инструменты:
//! - `echo` — возвращает свои аргументы;
//! - `slow {seconds, steps}` — шлёт прогресс `steps` раз за `seconds` секунд, затем отвечает;
//! - `fail` — ошибка JSON-RPC, как внутренняя ошибка ядра (`INTERNAL: …`);
//! - `hang` — не отвечает: для отмены и таймаута.
//!
//! Если инструменты взяты из снимка (`build/mcp-full-*.json`), любой из них отвечает как `echo`.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::{EventSink, Shared, EVENT_CANCELLED, EVENT_TOOL_CALL};

pub struct ChannelSink(pub mpsc::Sender<(String, String)>);

impl EventSink for ChannelSink {
    fn emit(&self, event: &'static str, data: String) {
        let _ = self.0.send((event.to_string(), data));
    }
}

/// Журнал событий, которые получила подставная 1С: проверки отмены и таймаута смотрят в него.
#[derive(Default, Clone)]
pub struct Events(Arc<Mutex<Vec<(String, String)>>>);

impl Events {
    pub fn all(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().clone()
    }

    pub fn cancelled(&self) -> Vec<Value> {
        self.all()
            .into_iter()
            .filter(|(event, _)| event == EVENT_CANCELLED)
            .filter_map(|(_, data)| serde_json::from_str(&data).ok())
            .collect()
    }
}

pub fn default_tools() -> String {
    json!([
        {"name": "echo", "description": "Возвращает аргументы.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
        {"name": "slow", "description": "Долгий вызов с прогрессом.", "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}, "steps": {"type": "integer"}}}},
        {"name": "fail", "description": "Внутренняя ошибка.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "hang", "description": "Не отвечает.", "inputSchema": {"type": "object", "properties": {}}},
    ])
    .to_string()
}

/// Создаёт общее состояние и поток подставной 1С. Сведения о сервере и инструменты задаются отдельно.
pub fn start() -> (Arc<Shared>, Events) {
    let (sender, receiver) = mpsc::channel::<(String, String)>();
    let shared = Shared::new(Arc::new(ChannelSink(sender)));
    let events = Events::default();
    let worker_shared = shared.clone();
    let worker_events = events.clone();
    std::thread::spawn(move || {
        for (event, data) in receiver {
            worker_events.0.lock().unwrap().push((event.clone(), data.clone()));
            if event != EVENT_TOOL_CALL {
                continue;
            }
            let Some(call_id) = serde_json::from_str::<Value>(&data)
                .ok()
                .and_then(|value| value["callId"].as_str().and_then(|id| id.parse::<u64>().ok()))
            else {
                continue;
            };
            let shared = worker_shared.clone();
            std::thread::spawn(move || answer(&shared, call_id));
        }
    });
    (shared, events)
}

fn answer(shared: &Shared, call_id: u64) {
    let Some(call) = shared.take_call(call_id).and_then(|text| serde_json::from_str::<Value>(&text).ok()) else {
        return;
    };
    let arguments = call["arguments"].clone();
    match call["name"].as_str().unwrap_or_default() {
        "slow" => {
            let seconds = arguments["seconds"].as_f64().unwrap_or(1.0);
            let steps = arguments["steps"].as_u64().unwrap_or(3).max(1);
            for step in 1..=steps {
                std::thread::sleep(Duration::from_secs_f64(seconds / steps as f64));
                shared.progress(call_id, step as f64, Some(steps as f64), Some(&format!("шаг {step}")));
            }
            shared.respond_text(call_id, &json!({"done": true, "steps": steps}).to_string());
        }
        "fail" => {
            shared.respond_error(call_id, -32000, "INTERNAL: проверочная ошибка");
        }
        "hang" => {}
        name => {
            shared.respond_text(call_id, &json!({"tool": name, "arguments": arguments}).to_string());
        }
    }
}
