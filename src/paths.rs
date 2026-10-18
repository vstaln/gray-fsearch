use std::path::PathBuf;

pub fn user_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::home_dir()
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// $GRAY_HOME, else ~/.gray — the same resolution gray_core::paths used,
/// so the plugin shares the existing index dir in place.
pub fn gray_home() -> Option<PathBuf> {
    std::env::var_os("GRAY_HOME")
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| user_home().map(|p| p.join(".gray")))
}
