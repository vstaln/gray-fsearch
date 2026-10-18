use std::path::{Path, PathBuf};

use crate::tool::{Tool, ToolContext};
use serde_json::{Value, json};
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default, Clone)]
pub struct SearchArgs {
    pub pattern: String,

    pub path: Option<PathBuf>,
    pub limit: Option<usize>,

    pub glob: Option<String>,
    pub ignore_case: bool,
    pub literal: bool,
    pub context: Option<usize>,
}

impl SearchArgs {
    fn root(&self, cwd: &Path) -> PathBuf {
        self.path.clone().unwrap_or_else(|| cwd.to_path_buf())
    }
}

pub async fn find(args: &SearchArgs, ctx: &ToolContext) -> String {
    find_on(args, ctx, &global_index()).await
}

pub(crate) async fn find_on(args: &SearchArgs, ctx: &ToolContext, b: &IndexBackend) -> String {
    match b {
        #[cfg(unix)]
        IndexBackend::Fs(p) => find_with_fsearch(args, ctx, p.clone()).await,
        IndexBackend::Off => find_call(args, ctx).await,
    }
}

async fn find_call(args: &SearchArgs, ctx: &ToolContext) -> String {
    tool_text(
        &crate::FindTool,
        ctx,
        json!({
            "pattern": args.pattern,
            "path": args.root(&ctx.cwd).to_string_lossy(),
            "limit": args.limit.unwrap_or(100),
        }),
    )
    .await
}

pub enum IndexBackend {
    #[cfg(unix)]
    Fs(std::sync::Arc<crate::search_index_fs::FsearchPool>),
    Off,
}

pub(crate) fn global_index() -> IndexBackend {
    static BACKEND: std::sync::OnceLock<IndexBackend> = std::sync::OnceLock::new();
    BACKEND
        .get_or_init(
            || match backend_kind(std::env::var("GRAY_SEARCH_INDEX").ok().as_deref()) {
                BackendKind::Off => IndexBackend::Off,
                #[cfg(unix)]
                BackendKind::Fs => {
                    IndexBackend::Fs(crate::search_index_fs::global_fs_pool().clone())
                }
            },
        )
        .clone()
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BackendKind {
    #[cfg(unix)]
    Fs,
    Off,
}

pub(crate) fn backend_kind(v: Option<&str>) -> BackendKind {
    match v {
        Some("off") | Some("none") => BackendKind::Off,
        #[cfg(unix)]
        _ => BackendKind::Fs,
        #[cfg(not(unix))]
        _ => BackendKind::Off,
    }
}

impl Clone for IndexBackend {
    fn clone(&self) -> Self {
        match self {
            #[cfg(unix)]
            Self::Fs(p) => Self::Fs(p.clone()),
            Self::Off => Self::Off,
        }
    }
}

#[cfg(unix)]
pub async fn find_with_fsearch(
    args: &SearchArgs,
    ctx: &ToolContext,
    pool: std::sync::Arc<crate::search_index_fs::FsearchPool>,
) -> String {
    let root = args.root(&ctx.cwd);
    let limit = args.limit.unwrap_or(100);
    if !ctx.cancel.is_cancelled() && pool.warm_picker(&root).is_some() {
        let (dir, pat) = (root.clone(), args.pattern.clone());
        let lane = pool.clone();
        let hits = tokio::select! {
            out = spawn_index(move || lane.indexed_glob(&dir, &pat, limit)) => out,
            _ = ctx.cancel.cancelled() => None,
        };
        if let Some(hits) = hits {
            return render_find(&hits, limit);
        }
    }
    if !ctx.cancel.is_cancelled() {
        pool.warm_in_background(&root);
    }
    tool_text(
        &crate::FindTool,
        ctx,
        json!({
            "pattern": args.pattern,
            "path": root.to_string_lossy(),
            "limit": limit,
        }),
    )
    .await
}

pub async fn grep(args: &SearchArgs, ctx: &ToolContext) -> String {
    grep_on(args, ctx, &global_index()).await
}

pub(crate) async fn grep_on(args: &SearchArgs, ctx: &ToolContext, b: &IndexBackend) -> String {
    match b {
        #[cfg(unix)]
        IndexBackend::Fs(p) => grep_with_fsearch(args, ctx, p.clone()).await,
        IndexBackend::Off => grep_call(args, ctx).await,
    }
}

#[cfg(unix)]
pub async fn grep_with_fsearch(
    args: &SearchArgs,
    ctx: &ToolContext,
    pool: std::sync::Arc<crate::search_index_fs::FsearchPool>,
) -> String {
    let root = args.root(&ctx.cwd);
    let limit = args.limit.unwrap_or(100);
    let context = args.context.unwrap_or(0);
    if !ctx.cancel.is_cancelled() && pool.warm_picker(&root).is_some() {
        let (dir, pat, g) = (root.clone(), args.pattern.clone(), args.glob.clone());
        let (ic, lit, lim, ctxn) = (args.ignore_case, args.literal, limit, context);
        let lane = pool.clone();
        let found = tokio::select! {
            out = spawn_index(move || {
                let cancel = CancellationToken::new();
                lane.indexed_grep(&dir, &pat, g.as_deref(), ic, lit, lim, ctxn, &cancel)
            }) => out,
            _ = ctx.cancel.cancelled() => None,
        };
        if let Some(found) = found {
            return crate::grep::render_matches(&root, &found.hits, limit, found.limit_reached);
        }
    }
    if !ctx.cancel.is_cancelled() {
        pool.warm_in_background(&root);
    }
    grep_call(args, ctx).await
}

async fn grep_call(args: &SearchArgs, ctx: &ToolContext) -> String {
    let root = args.root(&ctx.cwd);
    let limit = args.limit.unwrap_or(100);
    let context = args.context.unwrap_or(0);
    let mut call = json!({
        "pattern": args.pattern,
        "path": root.to_string_lossy(),
        "limit": limit,
    });
    if context > 0 {
        call["context"] = json!(context);
    }
    if let Some(g) = &args.glob {
        call["glob"] = json!(g);
    }
    if args.ignore_case {
        call["ignoreCase"] = json!(true);
    }
    if args.literal {
        call["literal"] = json!(true);
    }
    tool_text(&crate::GrepTool, ctx, call).await
}

#[cfg(unix)]
pub(crate) async fn spawn_index<F, T>(f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.ok().flatten()
}

#[cfg(unix)]
fn render_find(hits: &[String], limit: usize) -> String {
    use crate::MAX_BYTES;
    use crate::truncate::{append_notices, format_size, truncate_head};
    if hits.is_empty() {
        return "No files found matching pattern".to_string();
    }
    let mut notices: Vec<String> = Vec::new();
    if hits.len() >= limit {
        notices.push(format!(
            "{limit} results limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
    }
    let trunc = truncate_head(&hits.join("\n"));
    if trunc.truncated {
        notices.push(format!("{} limit reached", format_size(MAX_BYTES)));
    }
    if notices.is_empty() {
        trunc.content
    } else {
        let mut out = trunc.content;
        append_notices(&mut out, &notices);
        out
    }
}

async fn tool_text(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> String {
    tool.execute(ctx, args).await.content
}
