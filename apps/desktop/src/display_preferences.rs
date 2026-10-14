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
    pub render_markdown: bool,
    /// Filled bubble colors for directive, report, request, result and acknowledgment.
    pub coordination_colors: [[u8; 3]; 5],
    /// Standard reader bubble backgrounds for user, agent, MCP and transport messages.
    pub message_colors: [[u8; 3]; 4],
}

impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            show_transport_trace: false,
            show_delivery_history: false,
            render_markdown: true,
            coordination_colors: [
                [42, 57, 88], [36, 69, 62], [80, 61, 38], [66, 51, 83], [49, 61, 67],
            ],
            message_colors: [[27, 43, 65], [26, 44, 36], [42, 31, 56], [50, 39, 26]],
        }
    }
}

impl DisplayPreferences {
    pub const COORDINATION_KINDS: [&'static str; 5] = ["directive", "report", "request", "result", "ack"];
    pub const MESSAGE_KINDS: [&'static str; 4] = ["You", "Agent", "MCP tool", "Transport trace"];

    #[must_use]
    pub fn coordination_color(&self, kind: &str) -> [u8; 3] {
        Self::COORDINATION_KINDS.iter().position(|item| *item == kind)
            .map(|index| self.coordination_colors[index])
            .unwrap_or([43, 51, 63])
    }

    pub fn load(path: &Path) -> Self {
        let Ok(content) = fs::read(path) else {
            return Self::default();
        };
        if content.len() > 4096 {
            return Self::default();
        }
        let Ok(data) = serde_json::from_slice::<Value>(&content) else {
            return Self::default();
        };
        if data.get("schema").and_then(Value::as_str) != Some("chatarium-display-preferences")
            || data.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Self::default();
        }
        Self {
            show_transport_trace: data
                .get("show_transport_trace")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            show_delivery_history: data
                .get("show_delivery_history")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            // Missing in older preferences means the new renderer is enabled.
            render_markdown: data
                .get("render_markdown")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            message_colors: {
                let mut colors = Self::default().message_colors;
                if let Some(items) = data.get("message_colors").and_then(Value::as_array) {
                    for (index, item) in items.iter().take(colors.len()).enumerate() {
                        if let Some(rgb) = item.as_array().filter(|rgb| rgb.len() == 3) {
                            if let Some(channels) = rgb.iter().map(Value::as_u64).collect::<Option<Vec<_>>>() {
                                if channels.iter().all(|channel| *channel <= 255) {
                                    colors[index] = [channels[0] as u8, channels[1] as u8, channels[2] as u8];
                                }
                            }
                        }
                    }
                }
                colors
            },
            coordination_colors: {
                let mut colors = Self::default().coordination_colors;
                if let Some(items) = data.get("coordination_colors").and_then(Value::as_array) {
                    for (index, item) in items.iter().take(colors.len()).enumerate() {
                        if let Some(rgb) = item.as_array().filter(|rgb| rgb.len() == 3) {
                            if let Some(channels) = rgb.iter().map(Value::as_u64).collect::<Option<Vec<_>>>() {
                                if channels.iter().all(|channel| *channel <= 255) {
                                    colors[index] = [channels[0] as u8, channels[1] as u8, channels[2] as u8];
                                }
                            }
                        }
                    }
                }
                colors
            },
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
            "render_markdown": self.render_markdown,
            "coordination_colors": self.coordination_colors,
            "message_colors": self.message_colors,
        }))
        .map_err(|error| error.to_string())?;
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
            "chatarium-display-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        assert_eq!(
            DisplayPreferences::load(&path),
            DisplayPreferences::default()
        );
        let shown = DisplayPreferences {
            show_transport_trace: true,
            show_delivery_history: true,
            render_markdown: false,
            coordination_colors: Self::default().coordination_colors,
            message_colors: Self::default().message_colors,
        };
        shown.save(&path).unwrap();
        assert_eq!(DisplayPreferences::load(&path), shown);
        // A previously saved v1 file without this key must default to
        // Markdown on, while retaining the two existing debug preferences.
        fs::write(
            &path,
            br#"{"schema":"chatarium-display-preferences","version":1,"show_transport_trace":true,"show_delivery_history":false}"#,
        ).unwrap();
        let migrated = DisplayPreferences::load(&path);
        assert!(migrated.render_markdown);
        assert_eq!(migrated.coordination_colors, DisplayPreferences::default().coordination_colors);
        assert_eq!(migrated.message_colors, DisplayPreferences::default().message_colors);
        assert!(migrated.show_transport_trace);
        assert!(!migrated.show_delivery_history);
        fs::write(&path, b"{\"schema\":\"broken\"}").unwrap();
        assert_eq!(
            DisplayPreferences::load(&path),
            DisplayPreferences::default()
        );
        let _ = fs::remove_file(path);
    }
}
