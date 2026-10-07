//! Внешняя компонента Native API: объект `AddIn.<Имя>.Transport` для 1С.
//!
//! Все методы возвращают управление сразу; в ядре они вызываются через `Ждать …Асинх`. Внешние события
//! отдаёт один поток-насос: он не держит блокировок во время `ExternalEvent` и повторяет событие, если
//! очередь 1С переполнена. Паника в коде компоненты не выходит за границу FFI: каждый вход обёрнут
//! в `catch_unwind`, иначе процесс 1С упал бы целиком.

use std::ffi::{c_int, c_long, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use addin1c::{name, AttachType, CStr1C, CString1C, Connection, ParamValue, RawAddin, Variant};
use serde_json::json;

use crate::server::{self, EventSink, Options, Server, Shared};
use crate::{log, token};

const EVENT_BUFFER_DEPTH: c_long = 1000;
const EVENT_RETRIES: u32 = 500;
const EVENT_RETRY_PAUSE: Duration = Duration::from_millis(20);

enum PumpMessage {
    Event(&'static str, String),
    Stop,
}

struct PumpSink(mpsc::Sender<PumpMessage>);

impl EventSink for PumpSink {
    fn emit(&self, event: &'static str, data: String) {
        let _ = self.0.send(PumpMessage::Event(event, data));
    }
}

struct Pump {
    sender: mpsc::Sender<PumpMessage>,
    stopping: Arc<AtomicBool>,
    finished: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Pump {
    fn start(connection: &'static Connection, receiver: mpsc::Receiver<PumpMessage>, sender: mpsc::Sender<PumpMessage>) -> Self {
        let stopping = Arc::new(AtomicBool::new(false));
        let (finished_tx, finished) = mpsc::channel();
        let flag = stopping.clone();
        let thread = std::thread::Builder::new()
            .name("mcp-events".into())
            .spawn(move || {
                pump_loop(connection, receiver, &flag);
                let _ = finished_tx.send(());
            })
            .ok();
        Self { sender, stopping, finished, thread }
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.sender.send(PumpMessage::Stop);
        if self.finished.recv_timeout(Duration::from_secs(2)).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            log::warn("поток событий не остановился за 2 с");
        }
    }
}

fn pump_loop(connection: &'static Connection, receiver: mpsc::Receiver<PumpMessage>, stopping: &AtomicBool) {
    let source = CString1C::new(server::EVENT_SOURCE);
    while let Ok(message) = receiver.recv() {
        let PumpMessage::Event(event, data) = message else { break };
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let name = CString1C::new(event);
        let data = CString1C::new(&data);
        let mut attempts = 0;
        while !connection.external_event(&source, &name, &data) {
            attempts += 1;
            if attempts >= EVENT_RETRIES || stopping.load(Ordering::SeqCst) {
                log::warn(format!("событие {event} не принято 1С: очередь событий полна"));
                break;
            }
            std::thread::sleep(EVENT_RETRY_PAUSE);
        }
    }
}

struct MethodDef {
    en: &'static CStr1C,
    ru: &'static CStr1C,
    params: usize,
}

const START: usize = 0;
const STOP: usize = 1;
const SET_INFO: usize = 2;
const SET_TOOLS: usize = 3;
const TAKE_CALL: usize = 4;
const SEND_RESULT: usize = 5;
const SEND_ERROR: usize = 6;
const NOTIFY_PROGRESS: usize = 7;
const STATE: usize = 8;

const METHODS: &[MethodDef] = &[
    MethodDef { en: name!("Start"), ru: name!("Запустить"), params: 3 },
    MethodDef { en: name!("Stop"), ru: name!("Остановить"), params: 0 },
    MethodDef { en: name!("SetServerInfo"), ru: name!("УстановитьИнформациюОСервере"), params: 1 },
    MethodDef { en: name!("SetTools"), ru: name!("УстановитьИнструменты"), params: 1 },
    MethodDef { en: name!("TakeCall"), ru: name!("ВзятьВызов"), params: 1 },
    MethodDef { en: name!("SendResult"), ru: name!("ОтправитьОтвет"), params: 2 },
    MethodDef { en: name!("SendError"), ru: name!("ОтправитьОшибку"), params: 3 },
    MethodDef { en: name!("NotifyProgress"), ru: name!("УведомитьОПрогрессе"), params: 4 },
    MethodDef { en: name!("State"), ru: name!("Состояние"), params: 0 },
];

const PROP_VERSION: usize = 0;
const PROPS: &[(&CStr1C, &CStr1C)] = &[(name!("Version"), name!("Версия"))];

enum Ret {
    Bool(bool),
    Str(String),
}

pub struct McpAddin {
    shared: Arc<Shared>,
    server: Option<Server>,
    pump_sender: mpsc::Sender<PumpMessage>,
    pump_receiver: Option<mpsc::Receiver<PumpMessage>>,
    pump: Option<Pump>,
}

impl McpAddin {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let shared = Shared::new(Arc::new(PumpSink(sender.clone())));
        Self { shared, server: None, pump_sender: sender, pump_receiver: Some(receiver), pump: None }
    }

    fn invoke(&mut self, method: usize, params: &[Variant]) -> Option<Ret> {
        let param = |index: usize| params.get(index).map(Variant::get).unwrap_or(ParamValue::Empty);
        match method {
            START => Some(Ret::Str(self.start(param(0), param(1), param(2)))),
            STOP => Some(Ret::Bool(self.stop())),
            SET_INFO => Some(Ret::Str(match text(&param(0)) {
                Some(value) => outcome(self.shared.set_info(&value).map(|_| json!({}))),
                None => outcome(Err("ожидается строка JSON".into())),
            })),
            SET_TOOLS => Some(Ret::Str(match text(&param(0)) {
                Some(value) => outcome(self.shared.set_tools(&value).map(|count| json!({ "count": count }))),
                None => outcome(Err("ожидается строка JSON".into())),
            })),
            TAKE_CALL => {
                let data = call_id(&param(0)).and_then(|id| self.shared.take_call(id));
                Some(Ret::Str(data.unwrap_or_default()))
            }
            SEND_RESULT => {
                let sent = match (call_id(&param(0)), text(&param(1))) {
                    (Some(id), Some(value)) => self.shared.respond_text(id, &value),
                    _ => false,
                };
                Some(Ret::Bool(sent))
            }
            SEND_ERROR => {
                let sent = match (call_id(&param(0)), number(&param(1)), text(&param(2))) {
                    (Some(id), Some(code), Some(message)) => self.shared.respond_error(id, code as i64, &message),
                    _ => false,
                };
                Some(Ret::Bool(sent))
            }
            NOTIFY_PROGRESS => {
                let sent = match (call_id(&param(0)), number(&param(1))) {
                    (Some(id), Some(progress)) => {
                        self.shared.progress(id, progress, number(&param(2)), text(&param(3)).as_deref())
                    }
                    _ => false,
                };
                Some(Ret::Bool(sent))
            }
            STATE => Some(Ret::Str(self.shared.status())),
            _ => None,
        }
    }

    fn start(&mut self, port: ParamValue, origins: ParamValue, timeout: ParamValue) -> String {
        let failure = |code: &str, message: String| json!({"ok": false, "code": code, "error": message}).to_string();
        let Some(port) = number(&port).filter(|port| port.fract() == 0.0 && (1.0..=65535.0).contains(port)) else {
            return failure("INVALID_ARGUMENT", "порт должен быть числом от 1 до 65535".into());
        };
        let origins = match parse_origins(&text(&origins).unwrap_or_default()) {
            Ok(origins) => origins,
            Err(message) => return failure("INVALID_ARGUMENT", message),
        };
        if self.server.is_some() {
            return failure("ALREADY_RUNNING", "сервер уже запущен".into());
        }
        let (token, source) = match token::load() {
            Ok(found) => found,
            Err(error) => {
                let code = match error {
                    token::TokenError::Missing => "NO_TOKEN",
                    token::TokenError::Unreadable(_) => "TOKEN_UNREADABLE",
                    token::TokenError::TooShort => "TOKEN_TOO_SHORT",
                };
                log::warn(format!("запуск: {error}"));
                return failure(code, error.to_string());
            }
        };
        let mut options = Options::new(port as u16, token);
        options.origins = origins;
        if let Some(seconds) = number(&timeout).filter(|seconds| *seconds >= 1.0) {
            options.call_timeout = Duration::from_secs(seconds as u64);
        }
        match server::start(self.shared.clone(), options, source.as_str()) {
            Ok(server) => {
                let port = server.port;
                self.server = Some(server);
                json!({"ok": true, "port": port, "tokenSource": source.as_str()}).to_string()
            }
            Err(error) => {
                log::warn(format!("запуск: {}: {}", error.code, error.message));
                failure(error.code, error.message)
            }
        }
    }

    fn stop(&mut self) -> bool {
        match self.server.take() {
            Some(mut server) => {
                server.stop();
                true
            }
            None => false,
        }
    }
}

impl Default for McpAddin {
    fn default() -> Self {
        Self::new()
    }
}

fn outcome(result: Result<serde_json::Value, String>) -> String {
    match result {
        Ok(mut value) => {
            value["ok"] = json!(true);
            value.to_string()
        }
        Err(message) => json!({"ok": false, "code": "INVALID_ARGUMENT", "error": message}).to_string(),
    }
}

fn text(value: &ParamValue) -> Option<String> {
    match value {
        ParamValue::Str(chars) => Some(String::from_utf16_lossy(chars)),
        ParamValue::I32(number) => Some(number.to_string()),
        ParamValue::F64(number) => Some(number.to_string()),
        _ => None,
    }
}

fn number(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::I32(number) => Some(f64::from(*number)),
        ParamValue::F64(number) => Some(*number),
        ParamValue::Str(chars) => String::from_utf16_lossy(chars).trim().parse().ok(),
        _ => None,
    }
}

fn call_id(value: &ParamValue) -> Option<u64> {
    number(value).filter(|id| *id >= 1.0 && id.fract() == 0.0).map(|id| id as u64)
}

/// Origins: пусто — только localhost; JSON-массив строк или список через запятую.
fn parse_origins(text: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        return serde_json::from_str::<Vec<String>>(text).map_err(|error| format!("origins: {error}"));
    }
    Ok(text.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_string).collect())
}

fn same_name(left: &CStr1C, right: &CStr1C) -> bool {
    let trim = |name: &CStr1C| String::from_utf16_lossy(name.strip_suffix(&[0]).unwrap_or(name)).to_lowercase();
    trim(left) == trim(right)
}

fn guarded<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(fallback)
}

static PANIC_HOOK: Once = Once::new();

impl RawAddin for McpAddin {
    fn init(&mut self, interface: &'static Connection) -> bool {
        guarded(false, || {
            PANIC_HOOK.call_once(|| {
                std::panic::set_hook(Box::new(|info| log::warn(format!("паника: {info}"))));
            });
            interface.set_event_buffer_depth(EVENT_BUFFER_DEPTH);
            if let Some(receiver) = self.pump_receiver.take() {
                self.pump = Some(Pump::start(interface, receiver, self.pump_sender.clone()));
            }
            true
        })
    }

    fn done(&mut self) {
        guarded((), || {
            self.stop();
            if let Some(mut pump) = self.pump.take() {
                pump.stop();
            }
        })
    }

    fn register_extension_as(&mut self) -> &CStr1C {
        name!("Transport")
    }

    fn get_n_props(&mut self) -> usize {
        PROPS.len()
    }

    fn find_prop(&mut self, name: &CStr1C) -> Option<usize> {
        PROPS.iter().position(|(en, ru)| same_name(en, name) || same_name(ru, name))
    }

    fn get_prop_name(&mut self, num: usize, alias: usize) -> Option<&'static CStr1C> {
        PROPS.get(num).map(|(en, ru)| if alias == 0 { *en } else { *ru })
    }

    fn get_prop_val(&mut self, num: usize, val: &mut Variant) -> bool {
        guarded(false, || num == PROP_VERSION && val.set_str1c(env!("CARGO_PKG_VERSION")).is_ok())
    }

    fn is_prop_readable(&mut self, num: usize) -> bool {
        num < PROPS.len()
    }

    fn get_n_methods(&mut self) -> usize {
        METHODS.len()
    }

    fn find_method(&mut self, name: &CStr1C) -> Option<usize> {
        METHODS.iter().position(|method| same_name(method.en, name) || same_name(method.ru, name))
    }

    fn get_method_name(&mut self, num: usize, alias: usize) -> Option<&'static CStr1C> {
        METHODS.get(num).map(|method| if alias == 0 { method.en } else { method.ru })
    }

    fn get_n_params(&mut self, num: usize) -> usize {
        METHODS.get(num).map(|method| method.params).unwrap_or(0)
    }

    fn get_param_def_value(&mut self, method_num: usize, param_num: usize, mut value: Variant) -> bool {
        // Необязательны только «Всего» и «Сообщение» у прогресса.
        if method_num == NOTIFY_PROGRESS && param_num >= 2 {
            value.set_empty();
            return true;
        }
        false
    }

    fn has_ret_val(&mut self, method_num: usize) -> bool {
        method_num < METHODS.len()
    }

    fn call_as_proc(&mut self, method_num: usize, params: &mut [Variant]) -> bool {
        guarded(false, || self.invoke(method_num, params).is_some())
    }

    fn call_as_func(&mut self, method_num: usize, params: &mut [Variant], val: &mut Variant) -> bool {
        guarded(false, || match self.invoke(method_num, params) {
            Some(Ret::Bool(value)) => {
                val.set_bool(value);
                true
            }
            Some(Ret::Str(value)) => val.set_str1c(value).is_ok(),
            None => false,
        })
    }
}

static CLASS_NAMES: &[u16] = &addin1c::utf16_null!("Transport");

/// # Safety
/// Вызывает платформа 1С: `component` — указатель, куда записать созданный объект.
#[no_mangle]
pub unsafe extern "C" fn GetClassObject(_name: *const u16, component: *mut *mut c_void) -> c_long {
    if component.is_null() {
        return 0;
    }
    guarded(0, || unsafe { addin1c::create_component(component, McpAddin::new()) })
}

/// # Safety
/// Вызывает платформа 1С с объектом, созданным `GetClassObject`.
#[no_mangle]
pub unsafe extern "C" fn DestroyObject(component: *mut *mut c_void) -> c_long {
    if component.is_null() || unsafe { (*component).is_null() } {
        return -1;
    }
    guarded(-1, || unsafe { addin1c::destroy_component(component) })
}

#[no_mangle]
pub extern "C" fn GetClassNames() -> *const u16 {
    CLASS_NAMES.as_ptr()
}

#[no_mangle]
pub extern "C" fn SetPlatformCapabilities(_capabilities: c_int) -> c_int {
    3
}

#[no_mangle]
pub extern "C" fn GetAttachType() -> AttachType {
    AttachType::Any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_formats() {
        assert_eq!(parse_origins("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_origins(r#"["https://a.example","https://b.example"]"#).unwrap(), ["https://a.example", "https://b.example"]);
        assert_eq!(parse_origins("https://a.example, https://b.example").unwrap(), ["https://a.example", "https://b.example"]);
        assert!(parse_origins("[1]").is_err());
    }

    #[test]
    fn method_names_match_both_languages_any_case() {
        let mut addin = McpAddin::new();
        assert_eq!(addin.find_method(name!("ВзятьВызов")), Some(TAKE_CALL));
        assert_eq!(addin.find_method(name!("взятьвызов")), Some(TAKE_CALL));
        assert_eq!(addin.find_method(name!("TakeCall")), Some(TAKE_CALL));
        assert_eq!(addin.find_method(name!("НетТакого")), None);
        assert_eq!(addin.find_prop(name!("Версия")), Some(PROP_VERSION));
    }
}
