//! Файловый лог компоненты: `%LOCALAPPDATA%\AiOperator\mcp-transport.log`.
//!
//! Пишем только служебное: запуск, остановка, метод запроса, код ответа, длительность, ошибки.
//! Аргументы и результаты инструментов в лог не попадают: в них персональные данные.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SIZE: u64 = 1024 * 1024;

static LOCK: Mutex<()> = Mutex::new(());

pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AI_OPERATOR_MCP_LOG") {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("AiOperator").join("mcp-transport.log"))
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

fn write(level: &str, message: &str) {
    let Some(path) = path() else { return };
    let Ok(_guard) = LOCK.lock() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Лог не растёт бесконечно: при превышении размера остаётся одна предыдущая копия.
    if std::fs::metadata(&path).map(|meta| meta.len() > MAX_SIZE).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {level} {message}", timestamp());
    }
}

/// Время UTC в виде `2026-10-07T12:34:56Z` без внешних крейтов.
pub fn timestamp() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Дата по числу дней от 1970-01-01 (алгоритм Г. Хиннанта).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_733), (2026, 10, 7));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
