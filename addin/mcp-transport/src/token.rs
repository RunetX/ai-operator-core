//! Токен доступа к серверу MCP.
//!
//! Источники по порядку: переменная `AI_OPERATOR_MCP_TOKEN`, файл из `AI_OPERATOR_MCP_TOKEN_FILE`,
//! файл `%LOCALAPPDATA%\AiOperator\mcp-token` (его выпускает форма «Состояние ИИ-оператора» или
//! `tools/new-mcp-token.ps1`). Там же лежат скрипты для MCP-клиентов и лог компоненты.

use std::path::PathBuf;

pub const ENV_TOKEN: &str = "AI_OPERATOR_MCP_TOKEN";
pub const ENV_TOKEN_FILE: &str = "AI_OPERATOR_MCP_TOKEN_FILE";
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
            // Путь без имени пользователя: текст попадает в журнал регистрации и в отчёт для поддержки.
            TokenError::Missing => {
                write!(f, r"токен не найден в %LOCALAPPDATA%\AiOperator\mcp-token: создайте его в форме «Состояние ИИ-оператора»")
            }
            TokenError::Unreadable(detail) => write!(f, "токен не прочитан: {detail}"),
            TokenError::TooShort => write!(f, "токен короче {MIN_LENGTH} символов"),
        }
    }
}

pub fn default_file() -> Option<PathBuf> {
    crate::clients::data_dir().map(|dir| dir.join("mcp-token"))
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

/// Состояние токена для формы «Состояние ИИ-оператора». Значение токена наружу не отдаётся.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub exists: bool,
    pub source: Option<Source>,
    /// Файл, из которого читается токен; для переменной окружения — нет.
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

pub fn status() -> Status {
    status_from(
        std::env::var(ENV_TOKEN).ok(),
        std::env::var_os(ENV_TOKEN_FILE).map(PathBuf::from),
        default_file(),
    )
}

pub fn status_from(env: Option<String>, env_file: Option<PathBuf>, default: Option<PathBuf>) -> Status {
    let env_file = env_file.filter(|path| !path.as_os_str().is_empty());
    let path = match (&env, &env_file) {
        (Some(value), _) if !value.trim().is_empty() => None,
        (_, Some(file)) => Some(file.clone()),
        _ => default.clone(),
    };
    match load_from(env, env_file, default) {
        Ok((_, source)) => Status { exists: true, source: Some(source), path, error: None },
        Err(TokenError::Missing) => Status { exists: false, source: None, path, error: None },
        Err(error) => Status { exists: false, source: None, path, error: Some(error.to_string()) },
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    /// Файл уже есть, а перезаписывать не просили: старый токен работает у настроенных клиентов.
    Exists,
    Io(String),
}

/// Создаёт новый токен в файле: 32 случайных байта ОС в base64url, как `tools/new-mcp-token.ps1`.
pub fn create(path: &std::path::Path, overwrite: bool) -> Result<(), CreateError> {
    if path.exists() && !overwrite {
        return Err(CreateError::Exists);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| CreateError::Io(format!("случайные байты: {error}")))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| CreateError::Io(format!("{}: {error}", dir.display())))?;
    }
    std::fs::write(path, base64url(&bytes)).map_err(|error| CreateError::Io(format!("{}: {error}", path.display())))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| value | (u32::from(*byte) << (16 - 8 * index)));
        for index in 0..=chunk.len() {
            text.push(ALPHABET[(value >> (18 - 6 * index) & 63) as usize] as char);
        }
    }
    text
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
    fn base64url_matches_powershell_variant() {
        // [Convert]::ToBase64String без «=», «+» → «-», «/» → «_».
        assert_eq!(base64url(&[0xfb, 0xff, 0xfe]), "-__-");
        assert_eq!(base64url(b"ab"), "YWI");
        assert_eq!(base64url(b"a"), "YQ");
        assert_eq!(base64url(&[0u8; 32]).len(), 43);
    }

    #[test]
    fn create_does_not_overwrite_unless_asked() {
        let path = std::env::temp_dir().join(format!("mcp-token-create-{}", std::process::id())).join("mcp-token");
        let _ = std::fs::remove_file(&path);
        create(&path, false).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.len(), 43);
        assert!(first.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(create(&path, false), Err(CreateError::Exists));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        create(&path, true).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), first);
        let (token, source) = load_from(None, None, Some(path.clone())).unwrap();
        assert_eq!((token.len(), source), (43, Source::DefaultFile));
    }

    #[test]
    fn status_reports_source_and_path_without_value() {
        let file = temp_file("status", b"status-token-0123456789");
        let status = status_from(None, None, Some(file.clone()));
        assert_eq!(status, Status { exists: true, source: Some(Source::DefaultFile), path: Some(file.clone()), error: None });
        let status = status_from(Some("env-token-0123456789".into()), None, Some(file.clone()));
        assert_eq!((status.exists, status.source, status.path), (true, Some(Source::Env), None));
        let missing = std::env::temp_dir().join("no-such-mcp-token-status");
        let status = status_from(None, None, Some(missing.clone()));
        assert_eq!(status, Status { exists: false, source: None, path: Some(missing), error: None });
        let status = status_from(Some("short".into()), None, None);
        assert!(!status.exists && status.error.is_some());
    }

    #[test]
    fn default_file_is_in_ai_operator_data_dir() {
        let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
        assert_eq!(default_file(), Some(PathBuf::from(base).join("AiOperator").join("mcp-token")));
        assert!(TokenError::Missing.to_string().contains(r"%LOCALAPPDATA%\AiOperator\mcp-token"));
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
