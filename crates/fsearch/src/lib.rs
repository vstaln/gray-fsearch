#[cfg(unix)]
pub mod alloc;
#[cfg(unix)]
pub mod content;
#[cfg(unix)]
mod engine;
#[cfg(all(unix, target_os = "macos"))]
mod fsevents;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "fsevents_linux.rs"]
mod fsevents;
#[cfg(unix)]
pub mod index;
#[cfg(unix)]
pub mod live;
#[cfg(unix)]
pub mod query;
#[cfg(all(unix, target_os = "macos"))]
pub mod walk;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "walk_linux.rs"]
pub mod walk;

#[cfg(unix)]
pub use content::{Case, FileMatches, Grep, GrepResult, Hit, grep_file};
#[cfg(unix)]
pub use engine::{Engine, Found, Options, Status, default_dir, gated, has_full_disk_access, no_materialize};
#[cfg(unix)]
pub use query::{GrepMode, Query};

#[cfg(unix)]
mod diag {
    use std::sync::{Arc, RwLock};
    type LogSink = Arc<dyn Fn(&str) + Send + Sync>;
    static SINK: RwLock<Option<LogSink>> = RwLock::new(None);

    pub fn set_log_sink(sink: Option<LogSink>) {
        *SINK.write().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    pub fn line(msg: impl AsRef<str>) {
        let line = format!("{} {}", crate::query::now_secs(), msg.as_ref());
        match SINK.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(f) => f(&line),
            None => eprintln!("{line}"),
        }
    }
}

#[cfg(unix)]
#[doc(hidden)]
pub use diag::line as diag_line;
#[cfg(unix)]
pub use diag::set_log_sink;
