//! fsearch — the file-search plugin binary. One binary, several entries:
//!   * argv0 `grep`/`rg`  → PATH shim (asks $GRAY_SEARCH_SOCK, else execs
//!     the real tool untouched) — the extraction of `gray`'s argv0 arm.
//!   * `fsearch manifest` → manifest JSON (install probe / plugin check).
//!   * `fsearch sidecar`  → NDJSON wire loop + socket server + shim dir.
//!     Bare invocation on piped stdin is the same (host spawn signal).
//!   * `fsearch find|grep|status|serve` → CLI (`gray fsearch <verb>`).

use std::path::PathBuf;

const HELP: &str = "\
fsearch — file/content search plugin for gray

usage:
  fsearch manifest                      print the plugin manifest (JSON)
  fsearch sidecar                       run the plugin wire loop (stdio)
  fsearch find PATTERN [PATH] [--limit N]     find files by glob
  fsearch grep PATTERN [PATH] [flags]         search file contents
      --limit N   cap matches           --glob G    only matching files
      -i          case-insensitive      -F          literal pattern
      --context N lines of context
  fsearch status                        index/socket/shim status (JSON)
  fsearch serve --root DIR --sock PATH [--index-dir DIR]
                                        run the answer server (foreground)

invoked as `grep` or `rg` (via PATH shim): ask the index socket which
files could match, then exec the real tool on that list — or verbatim on
decline. GRAY_SEARCH_INDEX=off disables the shim; GRAY_SHIM_TRACE=<file>
logs one JSON line per call.";

fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(tool) = fsearch_plugin::search_shim::invoked_as() {
        fsearch_plugin::search_shim::run(&tool);
    }
    real_main()
}

#[tokio::main]
async fn real_main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("manifest") => {
            println!("{}", fsearch_plugin::sidecar::manifest_json());
            Ok(())
        }
        Some("sidecar") => fsearch_plugin::sidecar::run(),
        Some("find") => run_find(&args[1..]).await,
        Some("grep") => run_grep(&args[1..]).await,
        Some("status") => run_status(),
        Some("serve") => run_serve(&args[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => {
            eprintln!("fsearch: unknown verb '{other}'\n\n{HELP}");
            std::process::exit(2);
        }
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                print!("{HELP}");
                Ok(())
            } else {
                fsearch_plugin::sidecar::run()
            }
        }
    }
}

#[cfg(unix)]
fn ctx() -> fsearch_plugin::ToolContext {
    fsearch_plugin::ToolContext {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ..Default::default()
    }
}

#[cfg(unix)]
async fn run_find(args: &[String]) -> anyhow::Result<()> {
    let mut pattern = None;
    let mut path = None;
    let mut limit = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" => {
                limit = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("--limit needs a number"))?,
                );
            }
            "-h" | "--help" => {
                println!("usage: fsearch find PATTERN [PATH] [--limit N]");
                return Ok(());
            }
            s if s.starts_with("--limit=") => {
                limit = Some(s["--limit=".len()..].parse()?);
            }
            s if pattern.is_none() => pattern = Some(s.to_string()),
            s if path.is_none() => path = Some(s.to_string()),
            s => anyhow::bail!("unexpected argument '{s}'"),
        }
    }
    let Some(pattern) = pattern else {
        eprintln!("usage: fsearch find PATTERN [PATH] [--limit N]");
        std::process::exit(2);
    };
    let ctx = ctx();
    let args = fsearch_plugin::search_cmd::SearchArgs {
        pattern,
        path: path.map(|p| fsearch_plugin::tool::resolve_path(&ctx.cwd, &p)),
        limit,
        ..Default::default()
    };
    println!("{}", fsearch_plugin::search_cmd::find(&args, &ctx).await);
    Ok(())
}

#[cfg(unix)]
async fn run_grep(args: &[String]) -> anyhow::Result<()> {
    let mut a = fsearch_plugin::search_cmd::SearchArgs::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--limit" => {
                a.limit = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("--limit needs a number"))?,
                );
            }
            s if s.starts_with("--limit=") => a.limit = Some(s["--limit=".len()..].parse()?),
            "--glob" => {
                a.glob = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--glob needs a pattern"))?
                        .clone(),
                );
            }
            s if s.starts_with("--glob=") => a.glob = Some(s["--glob=".len()..].to_string()),
            "--context" => {
                a.context = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("--context needs a number"))?,
                );
            }
            s if s.starts_with("--context=") => {
                a.context = Some(s["--context=".len()..].parse()?);
            }
            "-i" | "--ignore-case" => a.ignore_case = true,
            "-F" | "--literal" => a.literal = true,
            "-h" | "--help" => {
                println!(
                    "usage: fsearch grep PATTERN [PATH] [--limit N] [--glob G] [-i] [-F] [--context N]"
                );
                return Ok(());
            }
            s if a.pattern.is_empty() => a.pattern = s.to_string(),
            s if a.path.is_none() => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                a.path = Some(fsearch_plugin::tool::resolve_path(&cwd, s));
            }
            s => anyhow::bail!("unexpected argument '{s}'"),
        }
    }
    if a.pattern.is_empty() {
        eprintln!("usage: fsearch grep PATTERN [PATH] [flags]");
        std::process::exit(2);
    }
    let ctx = ctx();
    println!("{}", fsearch_plugin::search_cmd::grep(&a, &ctx).await);
    Ok(())
}

#[cfg(not(unix))]
async fn run_find(_: &[String]) -> anyhow::Result<()> {
    anyhow::bail!("fsearch find is not supported on this platform (unix only)")
}

#[cfg(not(unix))]
async fn run_grep(_: &[String]) -> anyhow::Result<()> {
    anyhow::bail!("fsearch grep is not supported on this platform (unix only)")
}

fn run_status() -> anyhow::Result<()> {
    use serde_json::{Value, json};
    let base = fsearch_plugin::sidecar::base_dir();
    let sock = fsearch_plugin::sidecar::socket_path();
    let bin = fsearch_plugin::sidecar::bin_dir();
    #[cfg(unix)]
    let index = base.as_ref().and_then(|d| {
        let p = d.join("index.bin");
        fsearch::index::Index::load(&p).map(|i| {
            json!({"entries": i.n, "event_id": i.event_id, "index_bin_bytes":
                std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)})
        })
    });
    #[cfg(not(unix))]
    let index: Option<Value> = None;
    let shims = |d: &Option<PathBuf>| -> Value {
        d.iter()
            .flat_map(|d| ["grep", "rg"].into_iter().map(move |t| (d.join(t), t)))
            .map(|(p, t)| {
                json!({"name": t, "path": p.to_string_lossy(),
                    "target": std::fs::read_link(&p).ok().map(|t| t.to_string_lossy().into_owned())})
            })
            .collect()
    };
    println!(
        "{}",
        json!({
            "index_dir": base,
            "index": index.unwrap_or(Value::Null),
            "socket": {
                "path": sock,
                "live": sock.as_deref().is_some_and(fsearch_plugin::sidecar::socket_alive),
            },
            "shim_dir": bin,
            "shims": shims(&bin),
            "env": {
                "GRAY_SEARCH_SOCK": std::env::var("GRAY_SEARCH_SOCK").ok(),
                "GRAY_SEARCH_INDEX": std::env::var("GRAY_SEARCH_INDEX").ok(),
                "GRAY_SHIM_TRACE": std::env::var("GRAY_SHIM_TRACE").ok(),
            },
        })
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_serve(args: &[String]) -> anyhow::Result<()> {
    let mut root = None;
    let mut sock = None;
    let mut index_dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = it.next().map(PathBuf::from),
            "--sock" => sock = it.next().map(PathBuf::from),
            "--index-dir" => index_dir = it.next().map(PathBuf::from),
            s if s.starts_with("--root=") => root = Some(PathBuf::from(&s["--root=".len()..])),
            s if s.starts_with("--sock=") => sock = Some(PathBuf::from(&s["--sock=".len()..])),
            s if s.starts_with("--index-dir=") => {
                index_dir = Some(PathBuf::from(&s["--index-dir=".len()..]));
            }
            s => anyhow::bail!("unexpected argument '{s}'"),
        }
    }
    let (Some(root), Some(sock)) = (root, sock) else {
        eprintln!("usage: fsearch serve --root DIR --sock PATH [--index-dir DIR]");
        std::process::exit(2);
    };
    fsearch_plugin::search_serve::serve(&root, &sock, index_dir.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(not(target_os = "linux"))]
fn run_serve(_: &[String]) -> anyhow::Result<()> {
    anyhow::bail!("fsearch serve is not supported on this platform (linux only)")
}
