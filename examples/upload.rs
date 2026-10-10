//! Uploads a file as `multipart/form-data` and prints the response; if the
//! server answers with multipart, each part on its own line.
//!
//!     cargo run --example upload -- https://httpbin.org/post report.pdf

use curlite::{Multipart, mime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (url, path) = (args.next().expect("url"), args.next().expect("file"));
    let name = std::path::Path::new(&path)
        .file_name()
        .map_or("file".into(), |n| n.to_string_lossy().into_owned());
    let form = Multipart::new()
        .text("description", "uploaded by curlite")
        .file(
            "file",
            name,
            mime::APPLICATION_OCTET_STREAM,
            std::fs::read(&path)?,
        );
    let response = curlite::post(&url)
        .multipart(form)
        .send()?
        .error_for_status()?;
    match response.content_type()? {
        Some(ct) if ct.type_() == mime::MULTIPART => {
            for part in response.parts()? {
                println!("{}: {} bytes", part.name, part.body.len());
            }
        }
        _ => println!("{}", response.text().unwrap_or("<binary body>")),
    }
    Ok(())
}
