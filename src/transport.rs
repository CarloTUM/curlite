//! Everything that touches libcurl.

use bytes::Bytes;
use curl::easy::{Easy, Form, List, SeekResult};
use http::header::HeaderMap;
use http::{Method, StatusCode};

use std::cell::{Cell, RefCell};
use std::io::SeekFrom;

use crate::body::Body;
use crate::error::{Error, Result};
use crate::request::Request;
use crate::response::{Response, process_header_line};

thread_local! {
    /// One libcurl handle per thread, reused across requests. `reset()` clears
    /// the previous request's options but keeps libcurl's live connection
    /// cache, so repeat calls to the same host skip the TCP/TLS handshake.
    static HANDLE: RefCell<Easy> = RefCell::new(Easy::new());
}

pub(crate) fn perform(request: Request) -> Result<Response> {
    HANDLE.with_borrow_mut(|easy| {
        // reset() also keeps libcurl's cookie store, but the cookie engine is
        // never enabled here, so no cookie state survives between requests
        easy.reset();
        let result = perform_on(easy, request);
        // reset() does not free libcurl's multipart copies (the form chain and
        // its internal mime conversion); setting an empty form does
        let _ = easy.httppost(Form::new());
        // free the request's copied buffers right away instead of holding them
        // until the thread's next request; keeps the same caches
        easy.reset();
        result
    })
}

fn perform_on(easy: &mut Easy, mut request: Request) -> Result<Response> {
    request.finalize_headers()?;
    let url = request.url?;
    easy.url(url.as_str())?;
    easy.timeout(request.timeout)?;
    easy.follow_location(request.follow_redirects)?;
    easy.max_redirections(request.max_redirects)?;
    if request.method != Method::POST {
        easy.custom_request(request.method.as_str())?;
    }
    if request.method == Method::HEAD {
        easy.nobody(true)?;
    }

    // streamed via the read callback below; post_fields_copy would hold a
    // second copy inside libcurl
    let mut upload: Option<Bytes> = None;
    match request.body {
        Body::Empty => {
            if matches!(request.method, Method::POST | Method::PUT | Method::PATCH) {
                // bodyless POST/PUT/PATCH keep an explicit Content-Length: 0,
                // which strict servers require
                easy.post_fields_copy(b"")?;
            }
        }
        Body::Raw { data, .. } => {
            if data.is_empty() {
                easy.post_fields_copy(b"")?;
            } else {
                upload = Some(data);
            }
        }
        Body::Form(encoded) => upload = Some(Bytes::from(encoded)),
        Body::Multipart(multipart) => {
            let mut form = Form::new();
            for part in &multipart.parts {
                let mut p = form.part(&part.name);
                match &part.filename {
                    // CURLFORM_FILENAME only applies to file parts, so a named
                    // part goes through CURLFORM_BUFFER. That costs one copy of
                    // the data in Rust on top of libcurl's own
                    Some(filename) => p.buffer(filename.as_str(), part.data.to_vec()),
                    None => p.contents(&part.data),
                };
                if let Some(ct) = &part.content_type {
                    p.content_type(ct.as_ref());
                }
                p.add()?;
            }
            easy.httppost(form)?;
        }
    }
    if let Some(data) = &upload {
        // same framing as post_fields_copy (POST + known size), the verb still
        // comes from custom_request
        easy.post(true)?;
        easy.post_field_size(data.len() as u64)?;
    }

    let mut header_list = List::new();
    for (name, value) in &request.headers {
        let value = value.to_str()?.trim();
        // "name;" is libcurl's syntax for a header with an empty value
        let separator = if value.is_empty() { ";" } else { ": " };
        header_list.append(&format!("{}{separator}{value}", name.as_str()))?;
    }
    easy.http_headers(header_list)?;

    let mut body: Vec<u8> = Vec::new();
    let mut headers = HeaderMap::new();
    let mut header_err: Option<Error> = None;
    let upload_pos = Cell::new(0usize);
    let perform_result = {
        let mut transfer = easy.transfer();
        if let Some(data) = &upload {
            transfer.read_function(|buf| {
                let pos = upload_pos.get().min(data.len());
                let n = (data.len() - pos).min(buf.len());
                buf[..n].copy_from_slice(&data[pos..pos + n]);
                upload_pos.set(pos + n);
                Ok(n)
            })?;
            // rewinds for 307/308 and for retries on dead keep-alive
            // connections, which fail without it
            transfer.seek_function(|target| match target {
                SeekFrom::Start(offset) if offset <= data.len() as u64 => {
                    upload_pos.set(offset as usize);
                    SeekResult::Ok
                }
                _ => SeekResult::Fail,
            })?;
        }
        transfer.write_function(|data| {
            body.extend_from_slice(data);
            Ok(data.len())
        })?;
        transfer.header_function(|line| {
            if header_err.is_none()
                && let Err(e) = process_header_line(line, &mut headers)
            {
                header_err = Some(e);
            }
            // returning false aborts the transfer, which kills the keep-alive
            // connection but spares us a body we would throw away anyway
            header_err.is_none()
        })?;
        transfer.perform()
    };
    // the abort above surfaces as a curl write error, so the header error has
    // to be checked first or it is masked
    if let Some(e) = header_err {
        return Err(e);
    }
    perform_result?;
    // return the growth overshoot before the buffer is frozen into Bytes for
    // the response's lifetime
    body.shrink_to_fit();

    Ok(Response {
        status: StatusCode::from_u16(easy.response_code()? as u16)?,
        headers,
        body: Bytes::from(body),
    })
}
