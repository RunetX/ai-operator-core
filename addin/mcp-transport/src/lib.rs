//! MCP-транспорт ИИ-оператора 1С: внешняя компонента Native API с сервером MCP Streamable HTTP
//! на `127.0.0.1` и проверкой Bearer-токена (этап 3д «Развёртывание и диагностика»).
//!
//! Компонента реализует только то, что нужно ядру: `initialize`, `ping`, `tools/list`, `tools/call`
//! (сразу и отложенно), прогресс и отмену. Ресурсы, промпты и задачи MCP отложены.
//!
//! Код написан с нуля по спецификации MCP (modelcontextprotocol.io) и документации Native API.

pub mod clients;
/// Подставная 1С для `mcp-transport-dev` и тестов: только с признаком `dev`, в компоненту не входит.
#[cfg(feature = "dev")]
pub mod fake;
pub mod ffi;
pub mod log;
pub mod protocol;
pub mod server;
pub mod token;
