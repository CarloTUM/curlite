//! Everything that touches libcurl.
//!
//! Transfers run on a multi handle, one per thread, which owns the connection
//! cache. Driving it step by step is what lets a response be read as a
//! stream and keeps user code (writers, readers) out of libcurl's callbacks.

use bytes::Bytes;
use curl::easy::{Easy2, Handler as CurlHandler, ReadError, SeekResult, WriteError};
use curl::multi::{Easy2Handle, Multi};
use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, LOCATION};
use http::{Method, StatusCode};

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::io::{self, Read, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use crate::body::ReadSeek;
use crate::error::{Error, Result};
use crate::request::{HttpVersion, Request, Upload};
use crate::response::{Response, process_header_line};

/// Streaming transfers pause once this much body is buffered and not yet
/// consumed, and resume when the consumer has drained it.
const PAUSE_ABOVE: usize = 128 * 1024;

thread_local! {
    /// One multi handle per thread. It owns the connection cache, so repeat
    /// calls to the same host skip the TCP and TLS handshakes.
    static MULTI: Multi = Multi::new();
    /// An easy handle kept for the next request, so that back-to-back
    /// requests do not pay for `curl_easy_init`.
    static SPARE: RefCell<Option<Easy2<Handler>>> = const { RefCell::new(None) };
    /// Set while libcurl runs callbacks. A request started from inside one
    /// (a `body_reader` that fetches lazily) must not touch the shared multi
    /// handle and gets a private one.
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
    /// Completion results by transfer token. `curl_multi_perform` reports a
    /// multi-wide count of running handles and queues one message per
    /// finished handle, so with several transfers on the thread each has to
    /// find its own.
    static RESULTS: RefCell<HashMap<usize, std::result::Result<(), curl::Error>>> =
        RefCell::new(HashMap::new());
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(1) };
}

/// Keeps `IN_CALLBACK` set while libcurl runs, restoring the outer value
/// afterwards (a nested request's own steps must not clear it) and on panic.
struct CallbackGuard(bool);

impl CallbackGuard {
    fn enter() -> Self {
        CallbackGuard(IN_CALLBACK.replace(true))
    }
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        IN_CALLBACK.set(self.0);
    }
}

/// libcurl's callbacks for one transfer.
#[derive(Default)]
pub(crate) struct Handler {
    source: Option<Box<dyn ReadSeek>>,
    status: Option<StatusCode>,
    headers: HeaderMap,
    /// The blank line after a non-1xx status line has arrived.
    headers_done: bool,
    /// `Content-Length`, reserved before the first body chunk when `presize`.
    expected_len: Option<usize>,
    presize: bool,
    /// Body bytes not yet consumed.
    body: Vec<u8>,
    /// Pause the transfer when `body` holds this much.
    pause_above: Option<usize>,
    paused: bool,
    header_err: Option<Error>,
    read_err: Option<io::Error>,
    /// A panic from the user's reader, resumed outside libcurl. The curl
    /// crate would otherwise swallow it and fail every later callback on
    /// the thread.
    panic: Option<Box<dyn Any + Send>>,
}

impl Handler {
    fn clear(&mut self) {
        let mut body = std::mem::take(&mut self.body);
        body.clear();
        *self = Handler {
            body,
            ..Handler::default()
        };
    }
}

impl CurlHandler for Handler {
    fn write(&mut self, data: &[u8]) -> std::result::Result<usize, WriteError> {
        if self
            .pause_above
            .is_some_and(|limit| self.body.len() >= limit)
        {
            // the chunk is delivered again after unpause
            self.paused = true;
            return Err(WriteError::Pause);
        }
        if self.presize
            && self.body.is_empty()
            && let Some(len) = self.expected_len.take()
        {
            // a lying Content-Length must not abort on allocation failure;
            // growth takes over if the reservation fails
            let _ = self.body.try_reserve_exact(len);
        }
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn read(&mut self, buf: &mut [u8]) -> std::result::Result<usize, ReadError> {
        let Some(source) = &mut self.source else {
            return Ok(0);
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            loop {
                match source.read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        }));
        match result {
            Ok(Ok(n)) => Ok(n),
            Ok(Err(e)) => {
                self.read_err = Some(e);
                Err(ReadError::Abort)
            }
            Err(panic) => {
                self.panic = Some(panic);
                Err(ReadError::Abort)
            }
        }
    }

    fn seek(&mut self, whence: SeekFrom) -> SeekResult {
        let Some(source) = &mut self.source else {
            return SeekResult::CantSeek;
        };
        match catch_unwind(AssertUnwindSafe(|| source.seek(whence))) {
            Ok(Ok(_)) => SeekResult::Ok,
            Ok(Err(_)) => SeekResult::CantSeek,
            Err(panic) => {
                self.panic = Some(panic);
                SeekResult::Fail
            }
        }
    }

    fn header(&mut self, line: &[u8]) -> bool {
        let mut status = self.status;
        if let Err(e) = process_header_line(line, &mut self.headers, &mut status) {
            self.header_err = Some(e);
            // false aborts the transfer, which kills the keep-alive
            // connection but spares us a body we would throw away anyway
            return false;
        }
        self.status = status;
        if line == b"\r\n" || line == b"\n" {
            // the blank line after `100 Continue` does not end the headers
            self.headers_done = status.is_some_and(|s| !s.is_informational());
            self.expected_len = self
                .headers
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()?.trim().parse().ok());
        }
        true
    }

    #[cfg(feature = "log")]
    fn debug(&mut self, kind: curl::easy::InfoType, data: &[u8]) {
        use curl::easy::InfoType;
        let prefix = match kind {
            InfoType::Text => "* ",
            InfoType::HeaderIn => "< ",
            InfoType::HeaderOut => "> ",
            _ => return,
        };
        for line in String::from_utf8_lossy(data).lines() {
            log::trace!(target: "curlite", "{prefix}{line}");
        }
    }
}

enum MultiRef {
    Shared,
    Private(Multi),
}

impl MultiRef {
    fn with<R>(&self, f: impl FnOnce(&Multi) -> R) -> R {
        match self {
            MultiRef::Shared => MULTI.with(f),
            MultiRef::Private(multi) => f(multi),
        }
    }
}

/// How a transfer buffers its body.
#[derive(Clone, Copy)]
pub(crate) enum Mode {
    /// Everything, sized from `Content-Length`.
    Memory,
    /// Chunks until the consumer drains them, with backpressure.
    Stream,
}

/// A transfer whose final response headers have arrived; the body may still
/// be coming. Dropping it mid-way cancels the transfer.
pub(crate) struct Transfer {
    handle: Option<Easy2Handle<Handler>>,
    multi: MultiRef,
    token: usize,
    done: bool,
}

impl fmt::Debug for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transfer")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// Starts `request`, follows redirects, and returns once the headers of the
/// final response are in.
///
/// libcurl's own following is off, because with `CURLOPT_CUSTOMREQUEST` it
/// keeps the verb across every redirect, so a `DELETE` answered with `303`
/// would be repeated as `DELETE`.
pub(crate) fn start(
    mut request: Request,
    mut upload: Option<Upload>,
    mode: Mode,
) -> Result<Transfer> {
    let mut multi = if IN_CALLBACK.get() {
        MultiRef::Private(Multi::new())
    } else {
        MultiRef::Shared
    };
    let mut easy = SPARE
        .with(|spare| spare.borrow_mut().take())
        .unwrap_or_else(|| Easy2::new(Handler::default()));
    let mut hops = 0;
    loop {
        easy.reset();
        let handler = easy.get_mut();
        handler.clear();
        handler.presize = matches!(mode, Mode::Memory);
        handler.pause_above = matches!(mode, Mode::Stream).then_some(PAUSE_ABOVE);
        handler.source = upload.as_mut().and_then(|upload| upload.source.take());
        if let Err(e) = configure(&mut easy, &request, upload.as_ref()) {
            recycle(easy);
            return Err(e);
        }
        let token = NEXT_TOKEN.replace(NEXT_TOKEN.get().wrapping_add(1));
        let mut handle = multi.with(|m| m.add2(easy))?;
        handle.set_token(token)?;
        let mut transfer = Transfer {
            handle: Some(handle),
            multi,
            token,
            done: false,
        };
        transfer.drive(|h| h.headers_done)?;
        let handler = transfer.handler();
        let status = handler
            .status
            .ok_or_else(|| Error::HeaderParse("transfer ended without a status line".to_owned()))?;
        if !request.follow_redirects || !is_followed(status, handler.headers.contains_key(LOCATION))
        {
            return Ok(transfer);
        }
        // a redirect to follow: finish it, discarding its body (which in
        // stream mode would otherwise pause the transfer for good), and take
        // the handle back
        while !transfer.done {
            transfer.drain(|_| Ok::<(), Error>(()))?;
            transfer.step(|h| !h.body.is_empty())?;
        }
        let next;
        (easy, multi, next) = transfer.detach()?;
        // the Location header resolved against the request URL
        let Some(next) = next else {
            recycle(easy);
            return Err(Error::HeaderParse(
                "redirect without a resolvable Location".to_owned(),
            ));
        };
        if let Some(upload) = &mut upload {
            upload.source = easy.get_mut().source.take();
        }
        match status.as_u16() {
            // RFC 9110: 303 always switches to GET, 301/302 do so for POST
            303 if request.method != Method::GET && request.method != Method::HEAD => {
                drop_body(&mut request, &mut upload);
            }
            301 | 302 if request.method == Method::POST => drop_body(&mut request, &mut upload),
            _ => {
                if let Some(source) = upload.as_mut().and_then(|u| u.source.as_mut()) {
                    source.seek(SeekFrom::Start(0))?;
                }
            }
        }
        if hops == request.max_redirects {
            recycle(easy);
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
    request.headers.remove(CONTENT_TYPE);
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

fn configure(easy: &mut Easy2<Handler>, request: &Request, upload: Option<&Upload>) -> Result<()> {
    easy.url(&request.url)?;
    easy.timeout(request.timeout)?;
    if let Some(connect_timeout) = request.connect_timeout {
        easy.connect_timeout(connect_timeout)?;
    }
    easy.follow_location(false)?;
    if let Some(version) = request.http_version {
        easy.http_version(match version {
            HttpVersion::Http11 => curl::easy::HttpVersion::V11,
            HttpVersion::Http2 => curl::easy::HttpVersion::V2TLS,
            HttpVersion::Http2PriorKnowledge => curl::easy::HttpVersion::V2PriorKnowledge,
            HttpVersion::Http3 => curl::easy::HttpVersion::V3,
        })?;
    }
    if let Some(proxy) = &request.proxy {
        easy.proxy(proxy)?;
    }
    if let Some(path) = &request.unix_socket {
        easy.unix_socket_path(Some(path))?;
    }
    if let Some(path) = &request.ca_bundle {
        easy.cainfo(path)?;
    }
    if !request.verify_tls {
        easy.ssl_verify_peer(false)?;
        easy.ssl_verify_host(false)?;
    }
    if request.compression {
        // "" advertises every encoding this libcurl can decode
        easy.accept_encoding("")?;
    }
    #[cfg(feature = "log")]
    if log::log_enabled!(target: "curlite", log::Level::Trace) {
        easy.verbose(true)?;
    }

    if request.method != Method::POST {
        easy.custom_request(request.method.as_str())?;
    }
    if request.method == Method::HEAD {
        easy.nobody(true)?;
    }
    // streamed via the read callback; post_fields_copy would hold a second
    // copy inside libcurl
    match upload {
        Some(upload) if upload.len > 0 => {
            // same framing as post_fields_copy (POST + known size), the verb
            // still comes from custom_request
            easy.post(true)?;
            easy.post_field_size(upload.len)?;
        }
        Some(_) => easy.post_fields_copy(b"")?,
        None => {
            if matches!(request.method, Method::POST | Method::PUT | Method::PATCH) {
                // bodyless POST/PUT/PATCH keep an explicit Content-Length: 0,
                // which strict servers require
                easy.post_fields_copy(b"")?;
            }
        }
    }

    let mut header_list = curl::easy::List::new();
    let mut line = String::new();
    for (name, value) in &request.headers {
        let value = value.to_str()?.trim();
        // "name;" is libcurl's syntax for a header with an empty value
        let separator = if value.is_empty() { ";" } else { ": " };
        line.clear();
        let _ = write!(line, "{}{separator}{value}", name.as_str());
        header_list.append(&line)?;
    }
    easy.http_headers(header_list)?;
    Ok(())
}

/// Keeps `easy` for the next request on this thread.
fn recycle(mut easy: Easy2<Handler>) {
    easy.reset();
    easy.get_mut().clear();
    let _ = SPARE.try_with(|spare| {
        let mut spare = spare.borrow_mut();
        if spare.is_none() {
            *spare = Some(easy);
        }
    });
}

impl Transfer {
    fn handle(&self) -> &Easy2Handle<Handler> {
        self.handle.as_ref().expect("transfer is attached")
    }

    pub(crate) fn handler(&self) -> &Handler {
        self.handle().get_ref()
    }

    fn handler_mut(&mut self) -> &mut Handler {
        self.handle
            .as_mut()
            .expect("transfer is attached")
            .get_mut()
    }

    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Runs libcurl until `until` holds or the transfer ends.
    pub(crate) fn drive(&mut self, until: impl Fn(&Handler) -> bool) -> Result<()> {
        while !self.done && !until(self.handler()) {
            self.step(&until)?;
        }
        Ok(())
    }

    /// One round of libcurl work. Waits for socket activity afterwards
    /// unless the transfer ended or `ready` holds, so that data that just
    /// arrived reaches the consumer without a delay.
    pub(crate) fn step(&mut self, ready: impl Fn(&Handler) -> bool) -> Result<()> {
        let result = self.step_inner(&ready);
        if result.is_err() {
            self.done = true;
        }
        result
    }

    fn step_inner(&mut self, ready: &dyn Fn(&Handler) -> bool) -> Result<()> {
        if self.handler().paused {
            self.handle().unpause_write()?;
            self.handler_mut().paused = false;
        }
        self.multi.with(|m| {
            let _guard = CallbackGuard::enter();
            m.perform()
        })?;
        let handler = self.handler_mut();
        if let Some(panic) = handler.panic.take() {
            resume_unwind(panic);
        }
        if let Some(e) = handler.header_err.take() {
            return Err(e);
        }
        if let Some(e) = handler.read_err.take() {
            return Err(e.into());
        }
        // file every finished handle's result under its token; ours may be
        // among them, another transfer's is kept for it
        self.multi.with(|m| {
            m.messages(|message| {
                if let (Ok(token), Some(result)) = (message.token(), message.result()) {
                    RESULTS.with_borrow_mut(|results| results.insert(token, result));
                }
            });
        });
        if let Some(result) = RESULTS.with_borrow_mut(|results| results.remove(&self.token)) {
            self.done = true;
            return result.map_err(Error::from);
        }
        if !self.handler().paused && !ready(self.handler()) {
            self.multi
                .with(|m| m.poll(&mut [], Duration::from_millis(500)))?;
        }
        Ok(())
    }

    /// Hands the buffered body to `consume` and clears the buffer.
    pub(crate) fn drain<E>(
        &mut self,
        consume: impl FnOnce(&[u8]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let body = &mut self.handler_mut().body;
        if body.is_empty() {
            return Ok(());
        }
        let result = consume(body);
        body.clear();
        result
    }

    /// The response with whatever body is buffered; recycles the handle.
    pub(crate) fn into_response(mut self) -> Result<Response> {
        let (mut easy, _, _) = self.detach()?;
        let handler = easy.get_mut();
        let status = handler
            .status
            .ok_or_else(|| Error::HeaderParse("transfer ended without a status line".to_owned()))?;
        let headers = std::mem::take(&mut handler.headers);
        let mut body = std::mem::take(&mut handler.body);
        // return any growth overshoot before the buffer is frozen into Bytes
        // for the response's lifetime; a no-op when Content-Length sized it
        body.shrink_to_fit();
        recycle(easy);
        Ok(Response {
            status,
            headers,
            body: Bytes::from(body),
        })
    }

    /// Removes the handle from the multi handle and returns it with the
    /// resolved redirect target, if the response had one.
    fn detach(&mut self) -> Result<(Easy2<Handler>, MultiRef, Option<String>)> {
        let handle = self.handle.take().expect("transfer is attached");
        let next = handle.redirect_url()?.map(str::to_owned);
        let easy = self.multi.with(|m| m.remove2(handle))?;
        let multi = std::mem::replace(&mut self.multi, MultiRef::Shared);
        RESULTS.with_borrow_mut(|results| results.remove(&self.token));
        Ok((easy, multi, next))
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        let _ = RESULTS.try_with(|results| results.borrow_mut().remove(&self.token));
        let Some(handle) = self.handle.take() else {
            return;
        };
        if IN_CALLBACK.get() {
            // libcurl forbids removing a handle from inside a callback and
            // the curl crate would free it anyway; leaking it is the safe
            // way out of a reader that drops a transfer
            std::mem::forget(handle);
            return;
        }
        let removed = match &self.multi {
            MultiRef::Shared => MULTI.try_with(|m| m.remove2(handle)).ok(),
            MultiRef::Private(m) => Some(m.remove2(handle)),
        };
        if let Some(Ok(easy)) = removed {
            recycle(easy);
        }
    }
}

pub(crate) fn send(request: Request, upload: Option<Upload>) -> Result<Response> {
    let mut transfer = start(request, upload, Mode::Memory)?;
    transfer.drive(|_| false)?;
    transfer.into_response()
}

pub(crate) fn send_to(
    request: Request,
    upload: Option<Upload>,
    writer: &mut dyn Write,
) -> Result<Response> {
    let mut transfer = start(request, upload, Mode::Stream)?;
    loop {
        transfer.drain(|chunk| writer.write_all(chunk))?;
        if transfer.done() {
            return transfer.into_response();
        }
        transfer.step(|h| !h.body.is_empty())?;
    }
}

/// A response whose body is read on demand, see [`Request::send_reader`].
///
/// libcurl only works while the reader is read from; the connection stays
/// open in the meantime. Dropping the reader early cancels the transfer and
/// closes that connection.
///
/// [`Request::send_reader`]: crate::Request::send_reader
#[derive(Debug)]
pub struct ResponseReader {
    transfer: Transfer,
    status: StatusCode,
    headers: HeaderMap,
    pos: usize,
}

impl ResponseReader {
    pub(crate) fn new(request: Request, upload: Option<Upload>) -> Result<Self> {
        let mut transfer = start(request, upload, Mode::Stream)?;
        let handler = transfer.handler_mut();
        let status = handler
            .status
            .ok_or_else(|| Error::HeaderParse("transfer ended without a status line".to_owned()))?;
        let headers = std::mem::take(&mut handler.headers);
        Ok(ResponseReader {
            transfer,
            status,
            headers,
            pos: 0,
        })
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The parsed `Content-Type` header, if present and valid.
    pub fn content_type(&self) -> Result<Option<mime::Mime>> {
        crate::response::parse_content_type(&self.headers)
    }

    /// Reads the rest of the body into memory.
    pub fn into_response(mut self) -> Result<Response> {
        let mut body = Vec::new();
        self.read_to_end(&mut body)?;
        Ok(Response {
            status: self.status,
            headers: std::mem::take(&mut self.headers),
            body: Bytes::from(body),
        })
    }
}

impl Read for ResponseReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let handler = self.transfer.handler_mut();
            if self.pos < handler.body.len() {
                let n = (handler.body.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&handler.body[self.pos..self.pos + n]);
                self.pos += n;
                if self.pos == handler.body.len() {
                    handler.body.clear();
                    self.pos = 0;
                }
                return Ok(n);
            }
            if self.transfer.done() {
                return Ok(0);
            }
            self.transfer
                .step(|h| !h.body.is_empty())
                .map_err(io::Error::other)?;
        }
    }
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
