//! fsearch plugin — file/content search over a persistent fsearch index.
//!
//! Everything under `search_*`, `find`, `grep`, `grep_builtin`,
//! `index_filter`, `truncate`, and `redaction` is ported wholesale from
//! `gray/crates/gray-tools` (+ `gray-core/src/redaction.rs`). The only
//! edits are import lines: `gray_core::agent`/`tool_out`/`message` →
//! `crate::tool`, `gray_core::paths` → `crate::paths`,
//! `gray_core::redaction` → `crate::redaction`. Module names are kept
//! identical so `crate::` paths inside the ports resolve unchanged.

pub mod find;
pub mod grep;
pub mod grep_builtin;
pub mod index_filter;
pub mod paths;
pub mod redaction;
pub mod search_cmd;
#[cfg(unix)]
pub mod search_index_fs;
pub mod search_plan;
#[cfg(target_os = "linux")]
pub mod search_serve;
#[cfg(unix)]
pub mod search_shim;
#[cfg(target_os = "linux")]
pub mod search_watch;
pub mod sidecar;
pub mod tool;
pub mod truncate;

pub use find::FindTool;
pub use grep::GrepTool;
pub use tool::Tool;
pub(crate) use tool::{MAX_BYTES, fail, finish, get_opt_bool, get_opt_u64, get_str, resolve_path};
pub use tool::{ToolContext, ToolOutput};
