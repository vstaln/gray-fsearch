#[cfg(unix)]
#[path = "inner/firstrun.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {}
