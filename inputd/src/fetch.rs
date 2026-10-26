//! mc-fetch URL OUT — download one file over HTTP(S) to OUT (via OUT.part, so a
//! dropped connection never leaves a truncated file under the real name).

use std::io::{self, Write};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: mc-fetch URL OUT");
        std::process::exit(2);
    }
    if let Err(e) = fetch(&args[1], &args[2]) {
        eprintln!("mc-fetch: {}: {e}", args[1]);
        std::process::exit(1);
    }
}

fn fetch(url: &str, out: &str) -> io::Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .build();
    let resp = agent.get(url).call().map_err(|e| io::Error::other(e.to_string()))?;
    let part = format!("{out}.part");
    let mut f = std::fs::File::create(&part)?;
    let n = io::copy(&mut resp.into_reader(), &mut f)?;
    f.flush()?;
    drop(f);
    std::fs::rename(&part, out)?;
    eprintln!("mc-fetch: {out} ({n} bytes)");
    Ok(())
}
