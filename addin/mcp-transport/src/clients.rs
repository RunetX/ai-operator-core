//! Файлы для MCP-клиентов в `%LOCALAPPDATA%\AiOperator`: токен в их настройки не попадает.
//!
//! - `mcp-headers.cmd` — для `headersHelper` Claude Code: печатает заголовок авторизации из файла токена;
//! - `mcp-bridge-<порт>.cmd` — stdio-мост `npx mcp-remote` для Claude Desktop и LM Studio (у Claude Desktop
//!   `headersHelper` для HTTP-серверов не работает). Порт в имени: у каждой базы свой порт MCP, и мост одной
//!   базы не переписывает мост другой.
//!
//! Оба читают токен при каждом запуске, поэтому после «Создать токен» настройки клиентов менять не нужно.

use std::path::{Path, PathBuf};

pub const MCP_REMOTE: &str = "mcp-remote@0.14.3";

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("AiOperator"))
}

/// Путь для показа и отчёта: каталог профиля заменён на `%LOCALAPPDATA%`, имя пользователя Windows не видно.
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    match std::env::var("LOCALAPPDATA") {
        Ok(base) if !base.is_empty() && text.to_lowercase().starts_with(&base.to_lowercase()) => {
            format!("%LOCALAPPDATA%{}", &text[base.len()..])
        }
        _ => text,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFiles {
    pub headers_helper: PathBuf,
    pub bridge: PathBuf,
}

/// Пишет оба скрипта для порта `port` и возвращает их пути.
pub fn write(dir: &Path, port: u16) -> std::io::Result<ClientFiles> {
    std::fs::create_dir_all(dir)?;
    let files =
        ClientFiles { headers_helper: dir.join("mcp-headers.cmd"), bridge: dir.join(format!("mcp-bridge-{port}.cmd")) };
    std::fs::write(&files.headers_helper, crlf(&headers_script()))?;
    std::fs::write(&files.bridge, crlf(&bridge_script(port)))?;
    Ok(files)
}

/// Источник токена тот же, что у компоненты: файл из AI_OPERATOR_MCP_TOKEN_FILE или файл по умолчанию.
fn token_file_lines() -> &'static str {
    r#"set "TOKEN_FILE=%LOCALAPPDATA%\AiOperator\mcp-token"
if defined AI_OPERATOR_MCP_TOKEN_FILE set "TOKEN_FILE=%AI_OPERATOR_MCP_TOKEN_FILE%"
set "TOKEN=%AI_OPERATOR_MCP_TOKEN%"
if not defined TOKEN if exist "%TOKEN_FILE%" set /p TOKEN=<"%TOKEN_FILE%"
"#
}

fn headers_script() -> String {
    format!(
        r#"@echo off
rem Заголовок авторизации для Claude Code (headersHelper). Создано формой «Состояние ИИ-оператора».
setlocal
{}if not defined TOKEN (
	echo MCP token not found: %TOKEN_FILE% 1>&2
	exit /b 1
)
echo {{"Authorization": "Bearer %TOKEN%"}}
"#,
        token_file_lines()
    )
}

fn bridge_script(port: u16) -> String {
    format!(
        r#"@echo off
rem stdio-мост к MCP-серверу ИИ-оператора для Claude Desktop и LM Studio. Создано формой «Состояние ИИ-оператора».
rem Токен передаётся mcp-remote через переменную окружения: его нет ни в настройках клиента, ни в командной строке.
setlocal
{}if not defined TOKEN (
	echo MCP token not found: %TOKEN_FILE% 1>&2
	exit /b 1
)
set "AUTH_HEADER=Bearer %TOKEN%"
set "TOKEN="
npx -y {MCP_REMOTE} http://127.0.0.1:{port}/mcp --header "Authorization:${{AUTH_HEADER}}" --transport http-only
"#,
        token_file_lines()
    )
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_hides_user_profile() {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
        if base.is_empty() {
            return;
        }
        let path = PathBuf::from(&base).join("AiOperator").join("mcp-token");
        assert_eq!(display_path(&path), r"%LOCALAPPDATA%\AiOperator\mcp-token");
        assert_eq!(display_path(Path::new(r"D:\tokens\t")), r"D:\tokens\t");
    }

    #[test]
    fn scripts_are_written_with_port_and_without_token() {
        let dir = std::env::temp_dir().join(format!("mcp-clients-{}", std::process::id()));
        let files = write(&dir, 9890).unwrap();
        let headers = std::fs::read_to_string(&files.headers_helper).unwrap();
        let bridge = std::fs::read_to_string(&files.bridge).unwrap();
        assert_eq!(files.bridge.file_name().unwrap(), "mcp-bridge-9890.cmd");
        assert!(headers.contains(r#"echo {"Authorization": "Bearer %TOKEN%"}"#));
        assert!(bridge.contains("http://127.0.0.1:9890/mcp"));
        assert!(bridge.contains(r#"--header "Authorization:${AUTH_HEADER}""#));
        assert!(headers.contains("\r\n") && !headers.replace("\r\n", "").contains('\n'));
    }

    /// Скрипт для headersHelper печатает ровно JSON с токеном из файла.
    #[cfg(windows)]
    #[test]
    fn headers_script_prints_token_from_file() {
        let dir = std::env::temp_dir().join(format!("mcp-clients-run-{}", std::process::id()));
        let files = write(&dir, 9874).unwrap();
        let token_file = dir.join("token");
        std::fs::write(&token_file, "abcDEF-_0123456789xyz").unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/c", files.headers_helper.to_str().unwrap()])
            .env("AI_OPERATOR_MCP_TOKEN_FILE", &token_file)
            .env_remove("AI_OPERATOR_MCP_TOKEN")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value, serde_json::json!({"Authorization": "Bearer abcDEF-_0123456789xyz"}));

        let output = std::process::Command::new("cmd")
            .args(["/c", files.headers_helper.to_str().unwrap()])
            .env("AI_OPERATOR_MCP_TOKEN_FILE", dir.join("missing"))
            .env_remove("AI_OPERATOR_MCP_TOKEN")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
}
