use crate::frecency::{self, Frecency};
use crate::git::{self, Git};
use fsearch::walk::{KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER};
use fsearch::{Engine, Found, GrepMode, Options, Query};
use regex::bytes::Regex;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("fsearch.sock")
}

pub fn serve(dir: PathBuf, home: String) {
    std::fs::create_dir_all(&dir).ok();

    let Ok(mut lock) = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(dir.join("socket.lock")) else {
        return;
    };
    if !try_lock(&lock) {
        fsearch::diag_line("another fsearch daemon is running");
        return;
    }

    let _ = lock.set_len(0).and_then(|_| write!(lock, "{}", std::process::id()));
    let git = Git::new(&home);
    git::kill_on_term();

    let root = std::env::var_os("FSEARCH_ROOT").map(PathBuf::from);
    let engine = match Engine::start(Options { dir: dir.clone(), home, root, skip: None }) {
        Ok(e) => e,
        Err(e) => {
            fsearch::diag_line(&e);
            return;
        }
    };
    let g = git.clone();
    engine.on_change(move |dirs| g.changed(dirs));
    let cx = Arc::new(Ctx { engine, git, frecency: Frecency::load(dir.join("frecency")) });
    let sock = socket_path(&dir);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind socket");
    for conn in listener.incoming().flatten() {
        let cx = cx.clone();
        std::thread::spawn(move || handle(conn, &cx));
    }
}

struct Ctx {
    engine: Engine,
    git: Arc<Git>,
    frecency: Arc<Frecency>,
}

fn try_lock(f: &std::fs::File) -> bool {
    unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(f), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

pub fn stop(dir: &Path) -> Result<(), String> {
    let path = dir.join("socket.lock");
    let Ok(lock) = std::fs::File::open(&path) else {
        return Ok(());
    };
    let mut killed = None;
    for _ in 0..300 {
        if try_lock(&lock) {
            return Ok(());
        }
        if killed.is_none() {
            killed = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<i32>().ok()).filter(|&p| p > 1);
            if let Some(pid) = killed {
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(match killed {
        Some(pid) => format!("daemon {pid} did not exit"),
        None => "a daemon is running but did not record its pid; stop it by hand".into(),
    })
}

const WARM: Duration = Duration::from_millis(250);

fn handle(mut conn: UnixStream, cx: &Ctx) {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    let Ok(r) = conn.try_clone() else { return };
    let mut out = Vec::new();
    for line in BufReader::new(r).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        out.clear();
        respond(&line, cx, &mut out);
        if conn.write_all(&out).is_err() {
            return;
        }
        if waiting(&conn) {
            cx.engine.keep_warm(WARM);
        }
    }
}

fn waiting(conn: &UnixStream) -> bool {
    let mut b = 0u8;
    let n = unsafe { libc::recv(std::os::fd::AsRawFd::as_raw_fd(conn), (&raw mut b).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
}

fn respond(line: &str, cx: &Ctx, out: &mut Vec<u8>) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return put(out, &json!({"ok": false, "error": format!("bad json: {e}")}));
        }
    };
    let id = v.get("id").unwrap_or(&Value::Null);
    match run(&v, cx) {
        Ok(Reply::Hits(found, took_us, next, annotate)) => {
            let hits = found
                .iter()
                .map(|f| Hit {
                    git: annotate.then(|| cx.git.status(f.path.as_os_str().as_bytes()).and_then(git::word)).flatten(),
                    kind: kind_name(f.kind),
                    mtime: f.mtime,
                    path: f.path.to_string_lossy(),
                    score: f.score,
                    size: f.size,
                })
                .collect();
            put(out, &Hits { hits, id, next, ok: true, took_us })
        }
        Ok(Reply::Value(mut r)) => {
            r["id"] = id.clone();
            put(out, &r)
        }
        Err(e) => put(out, &json!({"ok": false, "error": e, "id": id})),
    }
}

fn put(out: &mut Vec<u8>, v: &impl serde::Serialize) {
    serde_json::to_writer(&mut *out, v).expect("json");
    out.push(b'\n');
}

enum Reply {
    Value(Value),

    Hits(Vec<Found>, u64, Option<usize>, bool),
}

#[derive(serde::Serialize)]
struct Hits<'a> {
    hits: Vec<Hit<'a>>,
    id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<usize>,
    ok: bool,
    took_us: u64,
}

#[derive(serde::Serialize)]
struct Hit<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<&'static str>,
    kind: &'static str,
    mtime: u32,
    path: std::borrow::Cow<'a, str>,
    score: i32,
    size: u64,
}

fn run(v: &Value, cx: &Ctx) -> Result<Reply, String> {
    let engine = &cx.engine;
    let op = v.get("op").and_then(Value::as_str).unwrap_or("search");
    let is_grep = op == "grep"
        || (op == "search"
            && v.get("q").and_then(Value::as_str).is_some_and(|q| ["grep:", "regex:", "sym:", "content:", "symbol:"].iter().any(|k| q.contains(k))));
    match op {
        "ping" => Ok(Reply::Value(json!({"ok": true}))),
        "save" => {
            engine.save();
            Ok(Reply::Value(json!({"ok": true, "scheduled": true})))
        }
        _ if is_grep => grep(v, cx).map(Reply::Value),
        "open" => {
            let paths: Vec<&str> = match (v.get("path"), v.get("paths")) {
                (Some(Value::String(p)), _) => vec![p],
                (_, Some(Value::Array(a))) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err("open needs a path (or paths)".into()),
            };
            let now = fsearch::query::now_secs();
            let scores: Vec<f32> = paths.iter().map(|p| cx.frecency.open(&real(p), now)).collect();
            Ok(Reply::Value(json!({"ok": true, "scores": scores})))
        }
        "recent" => {
            let (q, _) = parse_request(v, engine.home())?;
            let top = frecent(cx, q.scope.as_deref(), fsearch::query::now_secs());
            let hits = top.into_iter().filter_map(|(p, s)| Some(Found { score: (s * 100.0) as i32, ..found(&q, &p)? }));
            Ok(Reply::Hits(hits.take(q.limit).collect(), 0, None, v.get("git") == Some(&Value::Bool(true))))
        }
        "git" => {
            let p = v.get("path").and_then(Value::as_str).ok_or("git needs a path")?;
            let r = cx.git.load(&[&real(p)[..], b"/."].concat());
            let snap = r.as_ref().and_then(|r| r.snap(Duration::ZERO));
            Ok(Reply::Value(json!({
                "ok": true,
                "root": r.as_ref().map(|r| String::from_utf8_lossy(&r.root).into_owned()),
                "read": snap.is_some(),
                "changed": snap.map(|s| s.files.values().chain(s.dirs.values()).filter(|&&b| b & git::IGNORED == 0).count()),
            })))
        }
        "status" => {
            let s = engine.status();
            if !s.ready {
                return Err(engine.indexing());
            }
            let mut v = serde_json::to_value(s).map_err(|e| e.to_string())?;
            v["ok"] = true.into();
            Ok(Reply::Value(v))
        }
        "search" | "glob" => {
            let (mut q, sels) = parse_request(v, engine.home())?;
            let x = Extras::parse(v, &mut q, sels, op == "glob", false)?;
            let t = Instant::now();
            if x.plain() {
                let found = engine.search(&q)?;
                return Ok(Reply::Hits(found, t.elapsed().as_micros() as u64, None, false));
            }
            x.ready(cx, &q)?;
            let path = |f: &Found| f.path.as_os_str().as_bytes().to_vec();

            let reranks = x.boost || x.near.is_some();
            let (limit, want) = (q.limit, if reranks { x.offset.saturating_add(q.limit).max(RERANK) } else { x.offset.saturating_add(q.limit) });
            let (mut found, cut) = if x.listed() {
                let mut all = from_git(cx, &q, &x, want.saturating_add(1));
                all.retain(|f| x.keep(&cx.git, &path(f)));
                (all, false)
            } else {
                fetch_kept(
                    &x,
                    want,
                    MAX_FETCH,
                    |f| x.keep(&cx.git, &path(f)),
                    |n| {
                        q.limit = n;
                        Ok((engine.search(&q)?, false))
                    },
                )?
            };
            if reranks {
                rerank(cx, &mut found, x.boost, x.near.as_deref());
            }
            let next = x.cut(&mut found, limit).or(cut.then_some(x.offset + found.len()));
            Ok(Reply::Hits(found, t.elapsed().as_micros() as u64, next, x.annotate))
        }
        _ => Err(format!("unknown op {op}")),
    }
}

#[derive(Default)]
struct Extras {
    offset: usize,
    paged: bool,

    globs: Vec<(Regex, bool)>,

    sels: Vec<(git::Sel, bool)>,
    annotate: bool,
    hide_ignored: bool,
    cwd: Option<Vec<u8>>,
    boost: bool,
    near: Option<Vec<u8>>,
}

const EXTRAS: &[&str] = &["offset", "cursor", "glob", "cwd", "git", "ignored", "boost", "near"];

const RERANK: usize = 256;

const GIT_WAIT: Duration = Duration::from_secs(5);

const MAX_REPOS: usize = 16;

const MAX_DIRS: usize = 64;

impl Extras {
    fn parse(v: &Value, q: &mut Query, sels: Vec<(git::Sel, bool)>, glob_op: bool, grep: bool) -> Result<Extras, String> {
        let cwd = v.get("cwd").and_then(Value::as_str).map(real);

        let at = v.get("offset").or(v.get("cursor").filter(|_| !grep));
        let mut x = Extras {
            offset: 0,
            paged: glob_op || at.is_some(),
            globs: Vec::new(),
            annotate: v.get("git") == Some(&Value::Bool(true)),
            hide_ignored: v.get("ignored") == Some(&Value::Bool(false)) && !sels.contains(&(git::Sel::Ignored, false)),
            sels,
            cwd,
            boost: v.get("boost") == Some(&Value::Bool(true)),
            near: v.get("near").and_then(Value::as_str).map(real),
        };
        if let Some(o) = at {
            x.offset = (o.as_u64().or_else(|| o.as_str()?.parse().ok()).ok_or("offset must be a number")?).min(MAX_FETCH as u64) as usize;
        }
        let mut globs: Vec<&str> = match v.get("glob") {
            None => Vec::new(),
            Some(Value::String(g)) => vec![g],
            Some(Value::Array(a)) => a.iter().map(|g| g.as_str().ok_or("glob must be a string or a list of them")).collect::<Result<_, _>>()?,
            Some(_) => return Err("glob must be a string or a list of them".into()),
        };
        if glob_op {
            globs.insert(0, v.get("pattern").and_then(Value::as_str).ok_or("glob needs a pattern")?);
            q.kind = q.kind.or(Some(KIND_FILE));
        }

        if !x.sels.is_empty()
            && q.scope.is_none()
            && let Some(c) = &x.cwd
        {
            q.scope = Some(git_root(c).unwrap_or_else(|| c.clone()));
        }
        let base = q.scope.clone().or(x.cwd.clone()).unwrap_or_default();
        for g in globs {
            let (neg, g) = g.strip_prefix('!').map_or((false, g), |g| (true, g));
            let re = glob_regex(g, &base)?;
            if neg || q.path_re.is_some() {
                x.globs.push((re, neg));
                continue;
            }

            if let Some(dir) = glob_dir(g, &base)
                && q.scope.as_ref().is_none_or(|s| within(&dir, s))
            {
                q.scope = Some(dir);
            }
            if q.exts.is_empty() {
                q.exts = glob_exts(g);
            }
            q.path_re = Some(re);
        }
        Ok(x)
    }

    fn plain(&self) -> bool {
        !self.paged && self.globs.is_empty() && self.sels.is_empty() && !self.annotate && !self.hide_ignored && !self.boost && self.near.is_none()
    }

    fn listed(&self) -> bool {
        self.sels.iter().any(|&(s, neg)| !neg && s.listed())
    }

    fn base<'a>(&'a self, q: &'a Query) -> Option<&'a [u8]> {
        q.scope.as_deref().or(self.cwd.as_deref())
    }

    fn ready(&self, cx: &Ctx, q: &Query) -> Result<(), String> {
        let Some(base) = self.base(q).filter(|_| !self.sels.is_empty() || self.annotate || self.hide_ignored || self.boost) else { return Ok(()) };
        if self.sels.is_empty() {
            cx.git.load(&[base, b"/."].concat());
            return Ok(());
        }

        let deadline = Instant::now() + GIT_WAIT;
        for r in repos_at(cx, Some(base)) {
            if r.snap(deadline.saturating_duration_since(Instant::now())).is_none() {
                return Err(format!("still reading the git status of {}: try again in a few seconds", String::from_utf8_lossy(&r.root)));
            }
        }
        Ok(())
    }

    fn keep(&self, git: &Git, path: &[u8]) -> bool {
        if !self.globs.iter().all(|(re, neg)| re.is_match(path) != *neg) {
            return false;
        }
        if self.sels.is_empty() && !self.hide_ignored {
            return true;
        }
        let s = git.status(path);
        self.sels.iter().all(|&(sel, neg)| sel.test(s) != neg) && !(self.hide_ignored && s.is_some_and(|s| s & git::IGNORED != 0))
    }

    fn cut<T>(&self, v: &mut Vec<T>, limit: usize) -> Option<usize> {
        let want = self.offset.saturating_add(limit);
        let more = v.len() > want;
        v.truncate(want);
        v.drain(..self.offset.min(v.len()));
        more.then_some(self.offset + v.len())
    }
}

fn from_git(cx: &Ctx, q: &Query, x: &Extras, want: usize) -> Vec<Found> {
    let deadline = Instant::now() + GIT_WAIT;
    let pass = |bits: u8| x.sels.iter().filter(|(s, neg)| !neg && s.listed()).all(|(s, _)| s.test(Some(bits)));
    let (mut out, mut dirs) = (Vec::new(), Vec::new());
    for r in repos_at(cx, x.base(q)) {
        let Some(snap) = r.snap(deadline.saturating_duration_since(Instant::now())) else { continue };
        for (rel, _) in snap.files.iter().chain(&snap.dirs).filter(|(_, b)| pass(**b)) {
            let path = [&r.root[..], b"/", rel].concat();
            if let Some(f) = found(q, &path) {
                out.push(f);
            }
            if snap.dirs.contains_key(rel) {
                dirs.push(path);
            }
        }
    }
    dirs.sort();

    let inside = dirs.into_iter().filter_map(|d| match &q.scope {
        Some(s) if !within(&d, s) => within(s, &d).then(|| s.clone()),
        _ => Some(d),
    });
    for d in inside.take(MAX_DIRS) {
        let mut sub = q.clone();
        (sub.scope, sub.limit) = (Some(d), want);
        out.extend(cx.engine.search(&sub).unwrap_or_default());
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn repos_at(cx: &Ctx, base: Option<&[u8]>) -> Vec<Arc<git::Repo>> {
    let Some(base) = base else { return cx.git.known_under(b"/") };
    let mut repos: Vec<Arc<git::Repo>> = cx.git.load(&[base, b"/."].concat()).into_iter().collect();
    let mut dq = Query::parse("", cx.engine.home()).unwrap_or_default();
    (dq.name_re, dq.scope, dq.limit) = (Regex::new(r"^\.git$").ok(), Some(base.to_vec()), MAX_REPOS);
    for f in cx.engine.search(&dq).unwrap_or_default() {
        if let Some(r) = cx.git.load(f.path.as_os_str().as_bytes())
            && !repos.iter().any(|o| Arc::ptr_eq(o, &r))
        {
            repos.push(r);
        }
    }
    repos
}

fn within(path: &[u8], dir: &[u8]) -> bool {
    path.starts_with(dir) && (path.len() == dir.len() || dir.ends_with(b"/") || path[dir.len()] == b'/')
}

fn found(q: &Query, path: &[u8]) -> Option<Found> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(path)).ok()?;
    let t = m.file_type();
    let kind = if t.is_dir() {
        KIND_DIR
    } else if t.is_symlink() {
        KIND_LINK
    } else if t.is_file() {
        KIND_FILE
    } else {
        KIND_OTHER
    };
    let (size, mtime) = (m.size(), m.mtime().clamp(0, u32::MAX as i64) as u32);
    let score = q.match_path(path, kind, size, mtime)?;
    Some(Found { path: PathBuf::from(std::ffi::OsStr::from_bytes(path)), kind, size, mtime, score })
}

fn git_root(dir: &[u8]) -> Option<Vec<u8>> {
    let mut d = dir;
    loop {
        if std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(&[d, b"/.git"].concat())).is_ok() {
            return Some(d.to_vec());
        }
        d = &d[..d.iter().rposition(|&b| b == b'/').filter(|&i| i > 0)?];
    }
}

fn real(p: &str) -> Vec<u8> {
    let p = match p.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    };
    std::fs::canonicalize(&p).map_or_else(|_| p.into_bytes(), |c| c.as_os_str().as_bytes().to_vec())
}

const MAX_FETCH: usize = 20_000;

const MAX_GREP: usize = 2_000;

fn fetch_kept<T>(
    x: &Extras,
    want: usize,
    cap: usize,
    keep: impl Fn(&T) -> bool,
    mut search: impl FnMut(usize) -> Result<(Vec<T>, bool), String>,
) -> Result<(Vec<T>, bool), String> {
    let mut fetch = if x.globs.is_empty() && x.sels.is_empty() { want.saturating_add(1) } else { want.saturating_mul(2).saturating_add(16) };
    loop {
        let n = fetch.min(cap);
        let (mut got, cut) = search(n)?;
        let full = got.len() == n;
        got.retain(&keep);
        if got.len() > want || !full || n == cap || cut {
            return Ok((got, cut));
        }
        fetch = fetch.saturating_mul(8);
    }
}

fn frecent(cx: &Ctx, dir: Option<&[u8]>, now: u32) -> Vec<(Vec<u8>, f32)> {
    let mut m: std::collections::HashMap<Vec<u8>, f32> = std::collections::HashMap::new();
    let commits = dir.map(|d| cx.git.history_under(d, now)).unwrap_or_default();
    for (p, s) in cx.frecency.under(dir.unwrap_or(b"/"), now).into_iter().chain(commits) {
        *m.entry(p).or_default() += s;
    }
    let mut v: Vec<(Vec<u8>, f32)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

fn rerank(cx: &Ctx, hits: &mut [Found], boost: bool, near: Option<&[u8]>) {
    let now = fsearch::query::now_secs();
    let n = hits.len().min(RERANK);
    for f in &mut hits[..n] {
        let p = f.path.as_os_str().as_bytes();
        if boost {
            f.score += frecency::boost(cx.frecency.score(p, now) + cx.git.history(p, now) + frecency::decay(now.saturating_sub(f.mtime)));
        }
        if let Some(near) = near {
            f.score += nearness(near, p);
        }
    }

    hits[..n].sort_by_key(|f| std::cmp::Reverse(f.score));
}

fn nearness(near: &[u8], path: &[u8]) -> i32 {
    if path == near {
        return -20;
    }
    fn dirs(p: &[u8]) -> std::slice::Split<'_, u8, impl FnMut(&u8) -> bool> {
        p[..p.iter().rposition(|&b| b == b'/').unwrap_or(0)].split(|&b| b == b'/')
    }
    let common = dirs(near).zip(dirs(path)).take_while(|(x, y)| x == y).count();
    (12 - 3 * (dirs(near).count() + dirs(path).count() - 2 * common) as i32).max(0)
}

fn glob_regex(glob: &str, base: &[u8]) -> Result<Regex, String> {
    let g = glob.strip_prefix("./").unwrap_or(glob);
    let (body, dir) = g.strip_suffix('/').map_or((g, false), |b| (b, true));
    let mut re = String::from("(?is)^");
    match body.strip_prefix('/') {
        Some(abs) => {
            re.push('/');
            translate(abs, &mut re)?;
        }
        None => {
            re += &regex::escape(&String::from_utf8_lossy(base));
            re.push('/');
            if dir || !body.contains('/') {
                re += "(?:.*/)?";
            }
            translate(body, &mut re)?;
        }
    }
    re += if dir { "(?:/|$)" } else { "$" };
    Regex::new(&re).map_err(|e| format!("bad glob {glob}: {e}"))
}

fn translate(g: &str, re: &mut String) -> Result<(), String> {
    let mut cs = g.chars().peekable();
    let mut braces = 0;
    while let Some(c) = cs.next() {
        match c {
            '*' if cs.peek() == Some(&'*') => {
                cs.next();
                if cs.peek() == Some(&'/') {
                    cs.next();
                    *re += "(?:.*/)?";
                } else {
                    *re += ".*";
                }
            }
            '*' => *re += "[^/]*",
            '?' => *re += "[^/]",
            '[' => {
                re.push('[');
                if cs.peek().is_some_and(|&c| c == '!' || c == '^') {
                    cs.next();
                    re.push('^');
                }
                let mut first = true;
                loop {
                    match cs.next() {
                        None => return Err(format!("unclosed [ in glob {g}")),
                        Some(']') if !first => break,
                        Some(c @ ('[' | ']' | '\\' | '&' | '~' | '-')) if c != '-' || first => {
                            re.push('\\');
                            re.push(c);
                        }
                        Some(c) => re.push(c),
                    }
                    first = false;
                }
                re.push(']');
            }
            '{' => {
                braces += 1;
                *re += "(?:";
            }
            '}' if braces > 0 => {
                braces -= 1;
                re.push(')');
            }
            ',' if braces > 0 => re.push('|'),
            '\\' => *re += &regex::escape(&cs.next().map_or(String::new(), String::from)),
            c => *re += &regex::escape(c.encode_utf8(&mut [0; 4])),
        }
    }
    if braces > 0 { Err(format!("unclosed {{ in glob {g}")) } else { Ok(()) }
}

fn glob_dir(g: &str, base: &[u8]) -> Option<Vec<u8>> {
    let g = g.strip_prefix("./").unwrap_or(g);
    if g.ends_with('/') || !g.contains('/') {
        return None;
    }
    let (abs, body) = g.strip_prefix('/').map_or((false, g), |b| (true, b));
    let parts: Vec<&str> = body.split('/').collect();
    let lit: Vec<&str> = parts[..parts.len() - 1].iter().take_while(|p| !p.contains(['*', '?', '[', '{', '\\'])).copied().collect();
    let mut dir = if abs { Vec::new() } else { base.to_vec() };
    for p in lit {
        dir.push(b'/');
        dir.extend_from_slice(p.as_bytes());
    }
    if dir.is_empty() {
        return None;
    }
    let real = std::fs::canonicalize(std::ffi::OsStr::from_bytes(&dir)).map_or(dir, |p| p.as_os_str().as_bytes().to_vec());
    Some(real)
}

fn glob_exts(g: &str) -> Vec<Vec<u8>> {
    let name = g.rsplit('/').next().unwrap_or(g);
    let Some(rest) = name.strip_prefix("*.") else { return Vec::new() };
    let list = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest);
    let exts: Vec<Vec<u8>> = list.split(',').map(|e| e.to_ascii_lowercase().into_bytes()).collect();
    let plain = exts.iter().all(|e| !e.is_empty() && e.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    if plain { exts } else { Vec::new() }
}

fn grep(v: &Value, cx: &Ctx) -> Result<Value, String> {
    let engine = &cx.engine;
    let (mut q, sels) = parse_request(v, engine.home())?;
    let x = Extras::parse(v, &mut q, sels, false, true)?;
    let mode = match v.get("mode").and_then(Value::as_str) {
        Some("regex") => GrepMode::Regex,
        Some("symbol") => GrepMode::Symbol,
        Some("literal") => GrepMode::Literal,
        Some(m) => return Err(format!("unknown mode {m}")),
        None => q.grep_mode,
    };
    let case = match v.get("case").and_then(Value::as_str) {
        None | Some("smart") => fsearch::Case::Smart,
        Some("sensitive") => fsearch::Case::Sensitive,
        Some("insensitive") => fsearch::Case::Insensitive,
        Some(c) => return Err(format!("unknown case {c}")),
    };
    let pattern = v.get("pattern").and_then(Value::as_str).map(str::to_string).or(q.grep.take());
    let mut g = match (v.get("patterns").and_then(Value::as_array), pattern) {
        (Some(ps), _) => fsearch::Grep::any_of(&ps.iter().filter_map(Value::as_str).collect::<Vec<_>>(), case)?,
        (None, Some(p)) => fsearch::Grep::with_case(&p, mode, case)?,
        (None, None) => return Err("grep needs a pattern".into()),
    };
    let file = v.get("file").and_then(Value::as_str).map(|f| match f.strip_prefix('~') {
        Some(r) => format!("{}{r}", engine.home()),
        None if !f.starts_with('/') => format!("{}/{f}", engine.home()),
        None => f.to_string(),
    });
    let num = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as usize);

    g.max_per_file = num("per_file").unwrap_or(if file.is_some() { 200 } else { g.max_per_file });
    if let Some(ms) = v.get("budget_ms").and_then(Value::as_u64) {
        g.budget = (ms > 0).then(|| Duration::from_millis(ms));
    }
    (g.before, g.after) = (num("before").or(num("context")).unwrap_or(0), num("after").or(num("context")).unwrap_or(0));
    g.defs = v.get("defs").and_then(Value::as_bool).unwrap_or(false);
    g.skip = num("cursor").unwrap_or(0);
    let t = Instant::now();
    let source = |indexed| if indexed { "index" } else { "scan" };
    let (r, source, next) = if let Some(f) = &file {
        let r = fsearch::content::grep_file(&g, f.as_bytes());
        let next = r.next;
        (r, "file", next)
    } else if x.plain() {
        let (r, indexed) = engine.grep(&q, &g)?;
        let next = r.next;
        (r, source(indexed), next)
    } else if x.listed() {
        x.ready(cx, &q)?;
        let mut paths = from_git(cx, &q.clone_for_scan(), &x, MAX_GREP);
        paths.retain(|f| f.kind & 3 == KIND_FILE && f.size <= fsearch::content::MAX_FILE && x.keep(&cx.git, f.path.as_os_str().as_bytes()));
        paths.sort_by_key(|f| std::cmp::Reverse(f.mtime));
        let paths: Vec<Vec<u8>> = paths.into_iter().map(|f| f.path.as_os_str().as_bytes().to_vec()).collect();
        let mut r = fsearch::content::verify(&g, &paths, x.offset.saturating_add(q.limit).saturating_add(1));
        let next = x.cut(&mut r.files, q.limit).or((!r.complete).then_some(x.offset + r.files.len()));
        (r, "scan", next)
    } else {
        x.ready(cx, &q)?;
        let limit = q.limit;
        let mut last = None;
        let (mut files, cut) = fetch_kept(
            &x,
            x.offset.saturating_add(limit),
            MAX_GREP,
            |f: &fsearch::FileMatches| x.keep(&cx.git, &f.path),
            |n| {
                q.limit = n;
                let (mut r, indexed) = engine.grep(&q, &g)?;
                let (files, cut) = (std::mem::take(&mut r.files), !r.complete);
                last = Some((r, indexed));
                Ok((files, cut))
            },
        )?;
        let next = x.cut(&mut files, limit).or(cut.then_some(x.offset + files.len()));
        let (mut r, indexed) = last.expect("page searched");
        r.files = files;
        (r, source(indexed), next)
    };
    let files: Vec<Value> = r
        .files
        .iter()
        .map(|f| {
            let matches = f.lines.iter().zip(&f.hits).map(|((n, t), h)| {
                let mut m = json!({"line": n, "text": t, "col": h.col, "ranges": h.ranges});
                if h.def {
                    m["def"] = true.into();
                }
                if g.before > 0 {
                    m["before"] = json!(h.before);
                }
                if g.after > 0 {
                    m["after"] = json!(h.after);
                }
                m
            });
            let mut j = json!({"path": String::from_utf8_lossy(&f.path), "matches": matches.collect::<Vec<_>>()});
            if let Some(w) = x.annotate.then(|| cx.git.status(&f.path).and_then(git::word)).flatten() {
                j["git"] = w.into();
            }
            j
        })
        .collect();
    let mut out = json!({
        "ok": true,
        "took_us": t.elapsed().as_micros() as u64,
        "source": source,
        "candidates": r.candidates,
        "read": r.read,
        "complete": r.complete,
        "indexing": engine.status().content_pending,
        "files": files,
    });
    if let Some(n) = next {
        out["next"] = n.into();
    }
    Ok(out)
}

fn parse_request(v: &Value, home: &str) -> Result<(Query, Vec<(git::Sel, bool)>), String> {
    let (rest, sels) = take_git(v.get("q").and_then(Value::as_str).unwrap_or(""))?;
    let mut q = Query::parse(&rest, home)?;
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if EXTRAS.contains(&k.as_str()) {
                continue;
            }
            if k == "limit" {
                q.limit = val.as_u64().ok_or("limit must be a number")? as usize;
            } else {
                let s = match val {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                q.filter(k, &s, home)?;
            }
        }
    }
    Ok((q, sels))
}

#[allow(clippy::type_complexity)]
fn take_git(q: &str) -> Result<(std::borrow::Cow<'_, str>, Vec<(git::Sel, bool)>), String> {
    if !q.contains("git:") {
        return Ok((q.into(), Vec::new()));
    }
    let (mut rest, mut sels) = (Vec::new(), Vec::new());
    for w in q.split(' ') {
        let (neg, b) = w.strip_prefix('!').map_or((false, w), |b| (true, b));
        match b.strip_prefix("git:") {
            Some(v) => {
                let sel =
                    git::Sel::parse(v).ok_or(format!("unknown git status {v}: modified, untracked, staged, clean, ignored, tracked or conflict"))?;
                sels.push((sel, neg));
            }
            None => rest.push(w),
        }
    }
    Ok((rest.join(" ").into(), sels))
}

fn kind_name(k: u8) -> &'static str {
    match k & 3 {
        KIND_FILE => "file",
        KIND_DIR => "dir",
        KIND_LINK => "link",
        _ => "other",
    }
}

pub fn connect(dir: &Path) -> std::io::Result<UnixStream> {
    let sock = socket_path(dir);
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("daemon.log"))?;
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);

    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    cmd.env("MallocSpaceEfficient", "1");
    cmd.arg("serve").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
    for _ in 0..1500 {
        std::thread::sleep(Duration::from_millis(2));
        if let Ok(s) = UnixStream::connect(&sock) {
            return Ok(s);
        }
    }
    UnixStream::connect(&sock)
}
