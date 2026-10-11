//! Load optional multilingual system fonts without bundling licensed font files.
//!
//! egui does not automatically consult fontconfig for glyph fallback. Keep its
//! built-in Latin typefaces first, and append locally installed Noto faces only
//! for glyphs the built-ins cannot provide. Failure to locate a font preserves
//! the original reader and does not make launching Chatarium depend on a
//! package manager, a network request, or a subprocess.
use eframe::egui;
use std::path::Path;
use std::sync::Arc;

const STATUS_KEY: &str = "chatarium.system_font_fallbacks.v1";

struct FontFallback {
    name: &'static str,
    candidates: &'static [&'static str],
}

const FALLBACKS: &[FontFallback] = &[
    FontFallback {
        name: "chatarium.noto.cjk",
        candidates: &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
            "/usr/share/fonts/noto/NotoSansJP-Regular.otf",
        ],
    },
    FontFallback {
        name: "chatarium.noto.arabic",
        candidates: &[
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
        ],
    },
    FontFallback {
        name: "chatarium.noto.hebrew",
        candidates: &[
            "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
        ],
    },
];

fn font_file_looks_supported(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ttcf")
        || bytes.starts_with(b"OTTO")
        || bytes.starts_with(&[0x00, 0x01, 0x00, 0x00])
}

fn first_available_font(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| {
        let file = Path::new(path);
        if !file.is_file() {
            return None;
        }
        let size = std::fs::metadata(file).ok()?.len();
        if !(4..=128 * 1024 * 1024).contains(&size) {
            return None;
        }
        let bytes = std::fs::read(file).ok()?;
        font_file_looks_supported(&bytes).then_some(bytes)
    })
}

/// Optional system fallbacks are used for every native egui surface, not just
/// the synthetic reader; default proportional and monospace choices remain
/// first for ASCII/Latin glyphs.
pub(super) fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut loaded: Vec<String> = Vec::new();
    #[cfg(target_os = "linux")]
    for fallback in FALLBACKS {
        if let Some(bytes) = first_available_font(fallback.candidates) {
            let name = fallback.name.to_owned();
            fonts
                .font_data
                .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.clone());
            }
            loaded.push(name);
        }
    }
    if !loaded.is_empty() {
        ctx.set_fonts(fonts);
    }
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(STATUS_KEY), loaded));
}

/// Structural evidence for machine-owned GUI QA. No font bytes or personal
/// filesystem paths are exported to the telemetry.
pub(super) fn loaded_names(ctx: &egui::Context) -> Vec<String> {
    ctx.data(|data| {
        data.get_temp::<Vec<String>>(egui::Id::new(STATUS_KEY))
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_magic_rejects_missing_or_unexpected_files() {
        assert!(!font_file_looks_supported(b""));
        assert!(!font_file_looks_supported(b"this is not a font"));
        assert!(font_file_looks_supported(b"ttcf0000"));
        assert!(font_file_looks_supported(b"OTTO0000"));
        assert!(font_file_looks_supported(&[0, 1, 0, 0, 0, 0]));
    }

    #[test]
    fn absent_system_font_paths_are_optional() {
        assert!(
            first_available_font(&["/nonexistent/chatarium-no-fonts/never/font.otf"]).is_none()
        );
        let context = egui::Context::default();
        assert!(loaded_names(&context).is_empty());
    }
}
