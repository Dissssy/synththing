#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! The desktop binary: everything is in the library (`lib.rs`).

fn main() -> anyhow::Result<()> {
    synththing::main()
}
