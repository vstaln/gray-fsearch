#[cfg(unix)]
#[path = "inner/fresh.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {}
