#[cfg(unix)]
#[path = "inner/smoke.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {}
