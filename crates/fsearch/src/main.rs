#[cfg(unix)]
mod frecency;
#[cfg(unix)]
mod git;
#[cfg(unix)]
mod mcp;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod unix_main;

#[cfg(unix)]
fn main() {
    unix_main::main();
}

#[cfg(not(unix))]
fn main() {}
