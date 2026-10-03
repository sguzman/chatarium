use std::env;
use std::fmt::Display;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Off,
}

static STARTED: OnceLock<Instant> = OnceLock::new();
static LEVEL: OnceLock<Level> = OnceLock::new();

fn configured_level() -> Level {
    *LEVEL.get_or_init(|| {
        match env::var("CHATARIUM_LOG")
            .unwrap_or_else(|_| "info".to_owned())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "none" | "quiet" => Level::Off,
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" | "trace" => Level::Debug,
            _ => Level::Info,
        }
    })
}

fn elapsed_ms() -> u128 {
    STARTED.get_or_init(Instant::now).elapsed().as_millis()
}

fn write(level: Level, level_name: &str, area: &str, message: impl Display) {
    if configured_level() < level || configured_level() == Level::Off {
        return;
    }

    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("main");
    eprintln!(
        "[chatarium +{:>6}ms {level_name:<5} {area:<10} {thread_name}] {message}",
        elapsed_ms()
    );
}

pub fn init() {
    let _ = STARTED.set(Instant::now());
    info(
        "app",
        format!(
            "terminal diagnostics enabled (CHATARIUM_LOG={})",
            env::var("CHATARIUM_LOG").unwrap_or_else(|_| "info".to_owned())
        ),
    );
}

pub fn error(area: &str, message: impl Display) {
    write(Level::Error, "ERROR", area, message);
}

pub fn warn(area: &str, message: impl Display) {
    write(Level::Warn, "WARN", area, message);
}

pub fn info(area: &str, message: impl Display) {
    write(Level::Info, "INFO", area, message);
}

pub fn debug(area: &str, message: impl Display) {
    write(Level::Debug, "DEBUG", area, message);
}

pub fn short_id(value: &str) -> String {
    if value.chars().count() <= 12 {
        return value.to_owned();
    }
    let suffix = value.chars().rev().take(8).collect::<String>();
    format!("…{}", suffix.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_masks_long_private_identifiers() {
        assert_eq!(short_id("123456789012"), "123456789012");
        assert_eq!(short_id("abcdefghijklmnop"), "…ijklmnop");
    }
}
