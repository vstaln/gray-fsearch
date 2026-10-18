use fsearch::{Engine, Grep, GrepMode, Options, Query};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub fn main() {
    let mut a = std::env::args().skip(1);
    let dir = PathBuf::from(a.next().expect("index dir"));
    let tree = PathBuf::from(a.next().expect("tree"));
    let timeout = a.next().and_then(|s| s.parse().ok()).unwrap_or(120);

    let probe = a.next();
    let probe_scope = a.next();

    let home = tree.to_string_lossy().into_owned();

    let t0 = Instant::now();
    let e = Engine::start(Options { dir, home: home.clone(), root: Some(tree.clone()), skip: None }).expect("engine");
    while !e.status().ready {
        assert!(t0.elapsed() < Duration::from_secs(timeout), "index never became ready: {}", e.indexing());
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("index ready in {:?}: {} entries", t0.elapsed(), e.status().entries);

    while e.status().content_pending != 0 && std::env::var_os("SMOKE_NO_CONTENT_WAIT").is_none() {
        assert!(t0.elapsed() < Duration::from_secs(timeout), "content index never drained: {}", e.indexing());
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("content ready: {} docs", e.status().content_docs);

    if let Some(q) = probe {
        let scope = probe_scope.as_deref().unwrap_or("");
        let text = if scope.is_empty() { q.clone() } else { format!("{q} in:{scope}") };
        let t = Instant::now();
        let hits = e.search(&Query::parse(&text, &home).unwrap()).unwrap();
        println!("name search '{text}': {} hits in {:?} ({} entries)", hits.len(), t.elapsed(), e.status().entries);
        for h in hits.iter().take(5) {
            println!("  {}", h.path.display());
        }
        println!("SMOKE OK");
        return;
    }

    let t = Instant::now();
    let hits = e.search(&Query::parse("smoke_target", &home).unwrap()).unwrap();
    let name_ms = t.elapsed();
    assert!(
        hits.iter().any(|h| h.path.ends_with("smoke_target.txt")),
        "name search missed: {}",
        hits.iter().take(3).map(|h| h.path.display().to_string()).collect::<Vec<_>>().join(", ")
    );
    println!("name search ok: {} hits in {name_ms:?}", hits.len());

    let t = Instant::now();
    let scoped = e.search(&Query::parse(&format!("smoke_target in:{}", tree.join("deep/nest").display()), &home).unwrap()).unwrap();
    let scoped_ms = t.elapsed();
    assert!(!scoped.is_empty(), "scope empty: `in:` resolved nothing");
    assert!(
        scoped.iter().all(|h| h.path.starts_with(tree.join("deep/nest"))),
        "scope leaked: {}",
        scoped.iter().take(3).map(|h| h.path.display().to_string()).collect::<Vec<_>>().join(", ")
    );
    println!("scoped search ok: {} hits in {scoped_ms:?}", scoped.len());

    let typo = e.search(&Query::parse("smkoe_target", &home).unwrap()).unwrap();
    assert!(typo.iter().any(|h| h.path.ends_with("smoke_target.txt")), "typo tolerance missed");
    println!("typo tolerance ok");

    let q = Query::parse(&format!("in:{}", tree.display()), &home).unwrap();
    let (res, indexed) = e.grep(&q, &Grep::new("needle_phrase_4242", GrepMode::Literal).unwrap()).unwrap();
    assert!(
        res.files.iter().any(|f| String::from_utf8_lossy(&f.path).contains("needle.txt")),
        "grep missed: {}",
        res.files.iter().take(3).map(|f| String::from_utf8_lossy(&f.path).to_string()).collect::<Vec<_>>().join(", ")
    );
    println!("content grep ok (indexed={indexed}): {} files", res.files.len());

    println!("SMOKE OK");
}
