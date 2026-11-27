//! Fallback fonts for the glyphs iced's built-in font lacks.

/// Discover fallback fonts via fontconfig at startup and hand them to
/// iced as `Settings::fonts`. iced 0.12's bundled default only covers
/// Latin — without fallbacks, anything past ASCII (emoji, ⚠, Cyrillic,
/// CJK, …) silently drops from the render.
///
/// Queries cover three tiers of glyph coverage:
/// - emoji (color, e.g. Noto Color Emoji) for actual emoji;
/// - a dedicated symbols font for ⚠ / arrows / checkmarks;
/// - a general-purpose sans-serif for wide script coverage;
/// - a monospace for the rare places that want it.
///
/// Failures (no `fc-match`, missing fonts, unreadable files) degrade
/// gracefully — the app still runs, just without the extra coverage.
/// We log each load/miss to stderr so the first "I see boxes" report
/// is traceable.
#[cfg(target_os = "android")]
pub(super) fn fallback_fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    // Android has no fontconfig; iced_android maps Roboto, the symbol and emoji fonts from /system/fonts.
    iced_android::fonts()
}

#[cfg(not(target_os = "android"))]
pub(super) fn fallback_fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    let mut paths: Vec<String> = Vec::new();
    for query in [
        "Noto Color Emoji",
        "Noto Sans Symbols 2",
        "Noto Sans",
        "sans-serif",
        "emoji",
        "monospace",
    ] {
        // Generic aliases usually resolve to a file an earlier query
        // already found; loading it twice would just double its RAM.
        if let Some(path) = fc_match_path(query)
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    paths
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .map(std::borrow::Cow::Owned)
        .collect()
}

#[cfg(not(target_os = "android"))]
fn fc_match_path(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}"])
        .arg(pattern)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!path.is_empty()).then_some(path)
}
