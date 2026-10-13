//! User-controlled local UI visibility, separate from durable audit capture.
//! Hiding a diagnostic never deletes its journal events or admits them to
//! the model context.
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayPreferences {
    pub show_transport_trace: bool,
    pub show_delivery_history: bool,
}

impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            show_transport_trace: false,
            show_delivery_history: false,
        }
    }
}

impl DisplayPreferences {
    pub fn load(path: &Path) -> Self {
        let Ok(content) = fs::read(path) else { return Self::default(); };
        if content.len() > 4096 { return Self::default(); }
        let Ok(data) = serde_json::from_slice::<Value>(&content) else {
            return Self::default();
        };
        if data.get("schema").and_then(Value::as_str) != Some("chatarium-display-preferences")
            || data.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Self::default();
        }
        Self {
            show_transport_trace: data.get("show_transport_trace")
                .and_then(Value::as_bool).unwrap_or(false),
            show_delivery_history: data.get("show_delivery_history")
                .and_then(Value::as_bool).unwrap_or(false),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": "chatarium-display-preferences",
            "version": 1,
            "show_transport_trace": self.show_transport_trace,
            "show_delivery_history": self.show_delivery_history,
        })).map_err(|error| error.to_string())?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        fs::rename(temporary, path).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_hide_both_debug_surfaces_and_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-display-{}-{}.json", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        assert_eq!(DisplayPreferences::load(&path), DisplayPreferences::default());
        let shown = DisplayPreferences {
            show_transport_trace: true,
            show_delivery_history: true,
        };
        shown.save(&path).unwrap();
        assert_eq!(DisplayPreferences::load(&path), shown);
        fs::write(&path, b"{\"schema\":\"broken\"}").unwrap();
        assert_eq!(DisplayPreferences::load(&path), DisplayPreferences::default());
        let _ = fs::remove_file(path);
    }
}
