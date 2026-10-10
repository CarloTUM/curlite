//! The multipart parser on arbitrary bytes: the first line is the boundary,
//! the rest the body. Nested parts are split too. Must never panic.

#![no_main]

use curlite::{Bytes, ResponsePart};
use libfuzzer_sys::fuzz_target;

fn split(part: &ResponsePart, depth: u8) {
    if depth == 0 {
        return;
    }
    if let Ok(parts) = part.parts() {
        for part in &parts {
            let _ = (&part.name, &part.filename, &part.content_type, part.body.len());
            split(part, depth - 1);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(line_end) = data.iter().position(|&b| b == b'\n') else {
        return;
    };
    let Ok(boundary) = std::str::from_utf8(&data[..line_end]) else {
        return;
    };
    let Ok(content_type) = format!("multipart/mixed; boundary={boundary}").parse() else {
        return;
    };
    let root = ResponsePart {
        name: String::new(),
        filename: None,
        content_type: Some(content_type),
        body: Bytes::copy_from_slice(&data[line_end + 1..]),
    };
    split(&root, 4);
});
