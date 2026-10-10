//! Downloads a URL to a file with a progress line, streaming, no size limit.
//!
//!     cargo run --example download -- https://example.com/big.iso big.iso

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (url, path) = (args.next().expect("url"), args.next().expect("file"));
    let mut file = std::fs::File::create(&path)?;
    let response = curlite::get(&url)
        .on_progress(|p| {
            let total = p
                .download_total
                .map_or(String::new(), |t| format!(" of {t}"));
            eprint!("\r{}{total} bytes", p.downloaded);
            let _ = std::io::stderr().flush();
        })
        .send_to(&mut file)?
        .error_for_status()?;
    eprintln!("\n{} {} -> {path}", response.status(), response.url());
    Ok(())
}
