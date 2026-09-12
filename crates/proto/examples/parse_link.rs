//! Print what bliss makes of a `vless://` link.
//!
//! Reads stdin on purpose: a live link carries a UUID and Reality keys, and
//! stdin keeps it out of shell history, out of the process table and off disk.
//!
//!     cargo run -p bliss-proto --example parse_link
//!     <paste link, then Ctrl-Z Enter on Windows / Ctrl-D elsewhere>

use std::io::Read;

use bliss_proto::ServerConfig;
use color_eyre::eyre::{Context, Result};

fn main() -> Result<()> {
    color_eyre::install()?;

    let mut link = String::new();
    std::io::stdin()
        .read_to_string(&mut link)
        .wrap_err("failed to read the link from stdin")?;

    let config: ServerConfig = link
        .trim()
        .parse()
        .wrap_err("this does not look like a vless:// link bliss can use")?;

    println!("{config:#?}");
    Ok(())
}
