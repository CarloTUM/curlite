//! Everything that touches libcurl.

use bytes::Bytes;
use curl::easy::{Easy, List, ReadError, SeekResult};
use http::header::{AUTHORIZATION, CONTENT_LENGTH, HeaderMap, LOCATION};
use http::{Method, StatusCode};

use std::cell::{Cell, RefCell};
use std::io::{self, SeekFrom, Write};

use crate::error::{Error, Result};
use crate::request::{Request, Upload};
use crate::response::{Response, process_header_line};

thread_local! {
    /// One libcurl handle per thread, reused across requests. `reset()` clears
    /// the previous request's options but keeps libcurl's live connection
    /// cache, so repeat calls to the same host skip the TCP/TLS handshake.
    static HANDLE: RefCell<Easy> = RefCell::new(Easy::new());
}

/// Where a response body goes.
pub(crate) enum Sink<'a> {
    Memory,
    Writer(&'a mut dyn Write),
}

pub(crate) fn perform(
    request: Request,
    upload: Option<Upload>,
    sink: Sink<'_>,
) -> Result<Response> {
    let run = |easy: &mut Easy| {
        let result = follow(easy, request, upload, sink);
        // free the request's buffers right away instead of holding them until
        // the thread's next request; keeps the connection cache
        easy.reset();
        result
    };
    HANDLE.with(|handle| match handle.try_borrow_mut() {
        Ok(mut easy) => run(&mut easy),
        // a request sent from inside a `send_to` writer or `body_reader`
        // reader while the thread's handle is busy: use a throwaway handle
        // without the connection cache instead of panicking
        Err(_) => run(&mut Easy::new()),
    })
}

/// Performs the request and follows redirects. libcurl's own following is
/// off, because with `CURLOPT_CUSTOMREQUEST` it keeps the verb across every
/// redirect, so a `DELETE` answered with `303` would be repeated as `DELETE`.
fn follow(
    easy: &mut Easy,
    mut request: Request,
    mut upload: Option<Upload>,
    mut sink: Sink<'_>,
) -> Result<Response> {
    let mut hops = 0;
    loop {
        easy.reset();
        let response = perform_once(easy, &request, upload.as_mut(), &mut sink)?;
        if !request.follow_redirects
            || !is_followed(response.status, response.headers.contains_key(LOCATION))
        {
            return Ok(response);
        }
        // the Location header resolved against the request URL
        let Some(next) = easy.redirect_url()?.map(str::to_owned) else {
            return Ok(response);
        };
        match response.status.as_u16() {
            // RFC 9110: 303 always switches to GET, 301/302 do so for POST
            303 if request.method != Method::GET && request.method != Method::HEAD => {
                drop_body(&mut request, &mut upload);
            }
            301 | 302 if request.method == Method::POST => drop_body(&mut request, &mut upload),
            _ => {
                if let Some(upload) = &mut upload {
                    upload.source.seek(SeekFrom::Start(0))?;
                }
            }
        }
        if hops == request.max_redirects {
            return Err(Error::TooManyRedirects(request.max_redirects));
        }
        hops += 1;
        if !same_origin(&request.url, &next) {
            request.headers.remove(AUTHORIZATION);
        }
        request.url = next;
    }
}

/// Whether a response with this status is followed when redirects are on
/// and it has a `Location`: 300, 304 and a 3xx without `Location` are
/// returned.
fn is_followed(status: StatusCode, has_location: bool) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) && has_location
}

fn drop_body(request: &mut Request, upload: &mut Option<Upload>) {
    request.method = Method::GET;
    request.headers.remove(http::header::CONTENT_TYPE);
    *upload = None;
}

/// Scheme, host and port of `url`, lowercased, without resolving default
/// ports: `https://a.com` and `https://a.com:443` count as different origins,
/// which errs on the side of dropping credentials. A URL without a scheme is
/// `http://`, as libcurl assumes.
fn origin(url: &str) -> String {
    let url = if url.contains("://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let url = url.as_str();
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    let end = url[after_scheme..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| after_scheme + i);
    let authority = &url[after_scheme..end];
    // userinfo is not part of the origin
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{}{host}", &url[..after_scheme]).to_ascii_lowercase()
}

fn same_origin(a: &str, b: &str) -> bool {
    origin(a) == origin(b)
}

fn perform_once(
    easy: &mut Easy,
    request: &Request,
    upload: Option<&mut Upload>,
    sink: &mut Sink<'_>,
) -> Result<Response> {
    easy.url(&request.url)?;
    easy.timeout(request.timeout)?;
    if let Some(connect_timeout) = request.connect_timeout {
        easy.connect_timeout(connect_timeout)?;
    }
    easy.follow_location(false)?;
    if request.method != Method::POST {
        easy.custom_request(request.method.as_str())?;
    }
    if request.method == Method::HEAD {
        easy.nobody(true)?;
    }

    // streamed via the read callback below; post_fields_copy would hold a
    // second copy inside libcurl
    let upload = match upload {
        Some(upload) if upload.len > 0 => {
            // same framing as post_fields_copy (POST + known size), the verb
            // still comes from custom_request
            easy.post(true)?;
            easy.post_field_size(upload.len)?;
            Some(upload)
        }
        Some(_) => {
            easy.post_fields_copy(b"")?;
            None
        }
        None => {
            if matches!(request.method, Method::POST | Method::PUT | Method::PATCH) {
                // bodyless POST/PUT/PATCH keep an explicit Content-Length: 0,
                // which strict servers require
                easy.post_fields_copy(b"")?;
            }
            None
        }
    };

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
    let status = Cell::new(None::<StatusCode>);
    let mut header_err: Option<Error> = None;
    let mut io_err: Option<io::Error> = None;
    // the first body chunk sizes the buffer from Content-Length
    let expected_len = Cell::new(None::<usize>);
    let has_location = Cell::new(false);
    let follow_redirects = request.follow_redirects;
    // both upload callbacks need the source; libcurl never runs them
    // concurrently
    let source = upload.map(|upload| RefCell::new(&mut upload.source));
    let perform_result = {
        let mut transfer = easy.transfer();
        if let Some(source) = &source {
            transfer.read_function(|buf| {
                loop {
                    match source.borrow_mut().read(buf) {
                        Ok(n) => return Ok(n),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return Err(ReadError::Abort),
                    }
                }
            })?;
            // rewinds for libcurl's retries on dead keep-alive connections,
            // which fail without it
            transfer.seek_function(|target| match source.borrow_mut().seek(target) {
                Ok(_) => SeekResult::Ok,
                Err(_) => SeekResult::CantSeek,
            })?;
        }
        transfer.header_function(|line| {
            let mut current = status.get();
            if header_err.is_none()
                && let Err(e) = process_header_line(line, &mut headers, &mut current)
            {
                header_err = Some(e);
            }
            status.set(current);
            has_location.set(headers.contains_key(LOCATION));
            if let Some(len) = headers
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()?.trim().parse::<usize>().ok())
            {
                expected_len.set(Some(len));
            }
            // returning false aborts the transfer, which kills the keep-alive
            // connection but spares us a body we would throw away anyway
            header_err.is_none()
        })?;
        transfer.write_function(|chunk| {
            // the body of a redirect about to be followed is kept in memory
            // and dropped; everything else goes where the caller asked
            let to_memory = match sink {
                Sink::Memory => true,
                Sink::Writer(_) => {
                    follow_redirects
                        && status
                            .get()
                            .is_some_and(|s| is_followed(s, has_location.get()))
                }
            };
            if to_memory {
                if body.is_empty()
                    && let Some(len) = expected_len.take()
                {
                    // a lying Content-Length must not abort on allocation
                    // failure; growth takes over if the reservation fails
                    let _ = body.try_reserve_exact(len);
                }
                body.extend_from_slice(chunk);
            } else if let Sink::Writer(writer) = sink
                && let Err(e) = writer.write_all(chunk)
            {
                io_err = Some(e);
                // a short count makes libcurl abort with a write error
                return Ok(0);
            }
            Ok(chunk.len())
        })?;
        transfer.perform()
    };
    // the aborts above surface as curl errors, so the recorded errors have
    // to be checked first or they are masked
    if let Some(e) = header_err {
        return Err(e);
    }
    if let Some(e) = io_err {
        return Err(e.into());
    }
    perform_result?;
    // return any growth overshoot before the buffer is frozen into Bytes for
    // the response's lifetime; a no-op when Content-Length sized it
    body.shrink_to_fit();

    Ok(Response {
        status: StatusCode::from_u16(easy.response_code()? as u16)?,
        headers,
        body: Bytes::from(body),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(same_origin("https://A.com/x?y", "https://a.com/z"));
        assert!(same_origin("http://u:p@a.com/x", "http://a.com"));
        assert!(!same_origin("https://a.com", "http://a.com"));
        assert!(!same_origin("https://a.com", "https://a.com:8443"));
        assert!(!same_origin("https://a.com", "https://b.com"));
        assert!(same_origin("a.com/x", "http://A.com/y"));
        assert_eq!(origin("https://a.com"), "https://a.com");
        assert_eq!(origin(""), "http://");
    }
}
