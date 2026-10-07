//! Токен доступа к серверу MCP.
//!
//! Источники по порядку: переменная `WEB_TRANSPORT_MCP_TOKEN`, файл из `WEB_TRANSPORT_MCP_TOKEN_FILE`,
//! файл `%LOCALAPPDATA%\WebTransport\mcp-token` (его выпускает `tools/new-mcp-token.ps1`). Имена и путь
//! прежние: на них завязаны выданные токены, мост `mcp-bridge.cmd` и тесты.

use std::path::PathBuf;

pub const ENV_TOKEN: &str = "WEB_TRANSPORT_MCP_TOKEN";
pub const ENV_TOKEN_FILE: &str = "WEB_TRANSPORT_MCP_TOKEN_FILE";
pub const MIN_LENGTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    EnvFile,
    DefaultFile,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::EnvFile => "env_file",
            Source::DefaultFile => "file",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    /// Ни один источник не задан.
    Missing,
    /// Источник задан, но прочитать его нельзя.
    Unreadable(String),
    /// Токен короче MIN_LENGTH.
    TooShort,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Missing => write!(
                f,
                "токен не найден: создайте его скриптом tools/new-mcp-token.ps1 ({})",
                default_file().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            TokenError::Unreadable(detail) => write!(f, "токен не прочитан: {detail}"),
            TokenError::TooShort => write!(f, "токен короче {MIN_LENGTH} символов"),
        }
    }
}

pub fn default_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("WebTransport").join("mcp-token"))
}

/// Читает токен из первого заданного источника.
pub fn load() -> Result<(String, Source), TokenError> {
    load_from(
        std::env::var(ENV_TOKEN).ok(),
        std::env::var_os(ENV_TOKEN_FILE).map(PathBuf::from),
        default_file(),
    )
}

pub fn load_from(
    env: Option<String>,
    env_file: Option<PathBuf>,
    default: Option<PathBuf>,
) -> Result<(String, Source), TokenError> {
    if let Some(value) = env.filter(|value| !value.trim().is_empty()) {
        return checked(clean(&value), Source::Env);
    }
    if let Some(path) = env_file.filter(|path| !path.as_os_str().is_empty()) {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| TokenError::Unreadable(format!("{}: {error}", path.display())))?;
        return checked(clean(&text), Source::EnvFile);
    }
    match default {
        Some(path) if path.is_file() => {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| TokenError::Unreadable(format!("{}: {error}", path.display())))?;
            checked(clean(&text), Source::DefaultFile)
        }
        _ => Err(TokenError::Missing),
    }
}

/// Файл токена пишется в UTF-8 с BOM (Windows PowerShell 5.1): BOM и пробельные символы не часть токена.
fn clean(text: &str) -> String {
    text.trim_start_matches('\u{feff}').trim().to_string()
}

fn checked(token: String, source: Source) -> Result<(String, Source), TokenError> {
    if token.chars().count() < MIN_LENGTH {
        return Err(TokenError::TooShort);
    }
    Ok((token, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mcp-token-test-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn env_wins_over_files() {
        let file = temp_file("env-wins", b"file-token-0123456789");
        let (token, source) =
            load_from(Some("env-token-0123456789".into()), Some(file.clone()), Some(file)).unwrap();
        assert_eq!(token, "env-token-0123456789");
        assert_eq!(source, Source::Env);
    }

    #[test]
    fn env_file_before_default_file() {
        let first = temp_file("env-file", b"first-token-0123456789");
        let second = temp_file("default-file", b"second-token-0123456789");
        let (token, source) = load_from(None, Some(first), Some(second)).unwrap();
        assert_eq!(token, "first-token-0123456789");
        assert_eq!(source, Source::EnvFile);
    }

    #[test]
    fn bom_and_newline_are_stripped() {
        let file = temp_file("bom", "\u{feff}bom-token-0123456789\r\n".as_bytes());
        let (token, source) = load_from(None, None, Some(file)).unwrap();
        assert_eq!(token, "bom-token-0123456789");
        assert_eq!(source, Source::DefaultFile);
    }

    #[test]
    fn missing_and_short_tokens_are_errors() {
        assert_eq!(load_from(None, None, None), Err(TokenError::Missing));
        assert_eq!(
            load_from(None, None, Some(std::env::temp_dir().join("no-such-mcp-token"))),
            Err(TokenError::Missing)
        );
        assert_eq!(load_from(Some("short".into()), None, None), Err(TokenError::TooShort));
        assert!(matches!(
            load_from(None, Some(std::env::temp_dir().join("no-such-mcp-token")), None),
            Err(TokenError::Unreadable(_))
        ));
    }
}
