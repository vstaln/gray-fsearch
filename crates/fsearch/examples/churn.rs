#[cfg(unix)]
#[path = "inner/churn.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {}
