//! Подмножество MCP, которое нужно ядру ИИ-оператора: `initialize`, `ping`, `tools/list`,
//! `tools/call`, `notifications/progress`, `notifications/cancelled`. Ресурсы, промпты и задачи отложены.
//! Здесь только сборка сообщений JSON-RPC, без ввода-вывода.

use serde_json::{json, Map, Value};

/// Ревизии со схемой `initialize`, которые знает сервер. Первая — та, что отдаём неизвестным клиентам.
/// 2025-11-25 и 2026-07-28 отложены: прежняя компонента отвечала на них 2025-06-18, клиенты с этим работают.
pub const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
pub const LATEST_VERSION: &str = SUPPORTED_VERSIONS[0];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const REQUEST_TIMEOUT: i64 = -32001;

/// Версия для ответа на `initialize`: запрошенная, если мы её знаем, иначе последняя наша.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|version| SUPPORTED_VERSIONS.iter().find(|known| **known == version))
        .copied()
        .unwrap_or(LATEST_VERSION)
}

pub fn is_supported(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

/// Сведения о сервере из 1С: `name`, `title`, `version`, `description` и `instructions`.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub fields: Map<String, Value>,
    pub instructions: Option<String>,
}

impl ServerInfo {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|error| format!("сведения о сервере: {error}"))?;
        let Value::Object(mut object) = value else {
            return Err("сведения о сервере: ожидается объект JSON".into());
        };
        let instructions = match object.remove("instructions") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text),
            Some(_) => return Err("сведения о сервере: instructions должно быть строкой".into()),
        };
        if !matches!(object.get("name"), Some(Value::String(_))) {
            return Err("сведения о сервере: нет строки name".into());
        }
        // Порядок как у прежней компоненты: name, title, version, description, затем прочее.
        let mut fields = Map::new();
        for key in ["name", "title", "version", "description"] {
            if let Some(value) = object.remove(key) {
                fields.insert(key.to_string(), value);
            }
        }
        fields.extend(object);
        Ok(Self { fields, instructions })
    }
}

/// Список инструментов из 1С: массив `{name, description, inputSchema}` хранится как есть, чтобы
/// `tools/list` отдавал модели ровно то, что прислало ядро (снимок MCP, критерий «модель не заметила»).
#[derive(Debug, Clone)]
pub struct Tools {
    pub raw: Box<serde_json::value::RawValue>,
    pub names: Vec<String>,
}

impl Default for Tools {
    fn default() -> Self {
        Self { raw: serde_json::value::RawValue::from_string("[]".into()).unwrap(), names: Vec::new() }
    }
}

impl Tools {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|error| format!("инструменты: {error}"))?;
        let Value::Array(items) = value else {
            return Err("инструменты: ожидается массив JSON".into());
        };
        let mut names = Vec::with_capacity(items.len());
        for item in &items {
            let name = item.get("name").and_then(Value::as_str).ok_or("инструменты: у элемента нет строки name")?;
            if !matches!(item.get("inputSchema"), Some(Value::Object(_))) {
                return Err(format!("инструменты: у {name} нет объекта inputSchema"));
            }
            if names.iter().any(|known| known == name) {
                return Err(format!("инструменты: {name} указан дважды"));
            }
            names.push(name.to_string());
        }
        let raw = serde_json::value::RawValue::from_string(text.trim().to_string())
            .map_err(|error| format!("инструменты: {error}"))?;
        Ok(Self { raw, names })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|known| known == name)
    }
}

pub fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

pub fn initialize_result(id: &Value, version: &str, info: &ServerInfo) -> String {
    let mut result = Map::new();
    result.insert("protocolVersion".into(), Value::String(version.into()));
    result.insert("capabilities".into(), json!({"tools": {"listChanged": false}}));
    result.insert("serverInfo".into(), Value::Object(info.fields.clone()));
    if let Some(instructions) = &info.instructions {
        result.insert("instructions".into(), Value::String(instructions.clone()));
    }
    self::result(id, Value::Object(result))
}

pub fn tools_list_result(id: &Value, tools: &Tools) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{{"tools":{}}}}}"#,
        serde_json::to_string(id).unwrap_or_else(|_| "null".into()),
        tools.raw.get()
    )
}

/// Ответ инструмента: текст в одном элементе `content`.
pub fn tool_text_result(id: &Value, text: &str) -> String {
    result(id, json!({"content": [{"type": "text", "text": text}]}))
}

pub fn progress_notification(token: &Value, progress: f64, total: Option<f64>, message: Option<&str>) -> String {
    let mut params = Map::new();
    params.insert("progressToken".into(), token.clone());
    params.insert("progress".into(), number(progress));
    if let Some(total) = total {
        params.insert("total".into(), number(total));
    }
    if let Some(message) = message.filter(|text| !text.is_empty()) {
        params.insert("message".into(), Value::String(message.into()));
    }
    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}).to_string()
}

/// Целые числа — без дробной части: `5`, а не `5.0`.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

/// Одно событие SSE с сообщением JSON-RPC.
pub fn sse_event(message: &str) -> String {
    format!("event: message\ndata: {message}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_negotiation() {
        assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate(Some("2025-11-25")), LATEST_VERSION);
        assert_eq!(negotiate(Some("1999-01-01")), LATEST_VERSION);
        assert_eq!(negotiate(None), LATEST_VERSION);
    }

    #[test]
    fn tools_are_kept_verbatim() {
        let text = r#"[{"name":"b","description":"Б","inputSchema":{"type":"object","properties":{"x":{"type":"number"}}}},{"name":"a","inputSchema":{"type":"object"}}]"#;
        let tools = Tools::parse(text).unwrap();
        assert_eq!(tools.names, ["b", "a"]);
        assert_eq!(tools_list_result(&json!(7), &tools), format!(r#"{{"jsonrpc":"2.0","id":7,"result":{{"tools":{text}}}}}"#));
    }

    #[test]
    fn tools_are_validated() {
        assert!(Tools::parse("{}").is_err());
        assert!(Tools::parse(r#"[{"inputSchema":{}}]"#).is_err());
        assert!(Tools::parse(r#"[{"name":"a"}]"#).is_err());
        assert!(Tools::parse(r#"[{"name":"a","inputSchema":{}},{"name":"a","inputSchema":{}}]"#).is_err());
    }

    #[test]
    fn server_info_order_and_instructions() {
        let info = ServerInfo::parse(r#"{"description":"d","instructions":"i","version":"1","name":"n","title":"t"}"#).unwrap();
        assert_eq!(info.fields.keys().collect::<Vec<_>>(), ["name", "title", "version", "description"]);
        assert_eq!(info.instructions.as_deref(), Some("i"));
        assert!(ServerInfo::parse(r#"{"title":"t"}"#).is_err());
    }

    #[test]
    fn progress_numbers_and_optional_fields() {
        let text = progress_notification(&json!("tok"), 2.0, Some(5.0), Some("шаг"));
        assert_eq!(text, r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"tok","progress":2,"total":5,"message":"шаг"}}"#);
        let text = progress_notification(&json!(3), 0.5, None, Some(""));
        assert_eq!(text, r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":3,"progress":0.5}}"#);
    }

    #[test]
    fn string_and_number_ids_are_echoed() {
        assert_eq!(result(&json!("x"), json!({})), r#"{"jsonrpc":"2.0","id":"x","result":{}}"#);
        assert_eq!(error(&json!(1), METHOD_NOT_FOUND, "Method not found"), r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#);
    }
}
