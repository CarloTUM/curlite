use bytes::Bytes;
use http::Method;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use mime::Mime;

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::body::{Body, Multipart, ReadSeek, Segments, boundary, encode_form};
use crate::error::{Error, Result};
use crate::response::Response;
use crate::transport::{self, ResponseReader};

/// The HTTP version to ask libcurl for, see [`Request::http_version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpVersion {
    Http11,
    /// HTTP/2 over TLS when the server offers it (ALPN), HTTP/1.1 otherwise.
    Http2,
    /// HTTP/2 without negotiation, also over plain TCP.
    Http2PriorKnowledge,
    /// HTTP/3 when the server offers it. Needs a libcurl built with it.
    Http3,
}

/// HTTP authentication, see [`Request::auth`]. Credentials are handed to
/// libcurl, which answers the server's challenge; only `Basic` and `Bearer`
/// are sent without being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Auth {
    Basic {
        user: String,
        password: String,
    },
    /// `Authorization: Bearer <token>`, unless the request sets that header.
    Bearer(String),
    Digest {
        user: String,
        password: String,
    },
    /// Kerberos (SPNEGO) with the credentials of the process, like
    /// `curl --negotiate -u :`. A system libcurl has it; a static one
    /// needs the `curl` crate's `spnego` feature and the GSS-API headers.
    Negotiate,
    /// NTLM. A static libcurl needs the `curl` crate's `ntlm` feature.
    Ntlm {
        user: String,
        password: String,
    },
}

/// Bytes moved so far in one transfer, see [`Request::on_progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Progress {
    /// Response body bytes received, after decompression.
    pub downloaded: u64,
    /// The response's `Content-Length`, once the headers are in. With
    /// compression it counts the encoded bytes.
    pub download_total: Option<u64>,
    /// Request body bytes sent.
    pub uploaded: u64,
    /// The request body's size, `None` without a body.
    pub upload_total: Option<u64>,
}

pub(crate) struct ProgressFn(pub(crate) Box<dyn FnMut(Progress)>);

impl std::fmt::Debug for ProgressFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressFn")
    }
}

/// A request under construction. Created with [`Request::new`], one of the
/// free functions ([`get`], [`post`], ...) or a [`Client`], and sent with
/// [`Request::send`] or [`Request::send_to`].
///
/// Builder methods that can fail (`header`) record the error and return it
/// from `send`, so that a request can be built in one expression.
///
/// ```no_run
/// let response = curlite::post("https://example.com/api?debug=1")
///     .header("Authorization", "Bearer secret")
///     .form([("name", "value")])
///     .send()?;
/// assert!(response.status().is_success());
/// # Ok::<(), curlite::Error>(())
/// ```
///
/// [`get`]: crate::get
/// [`post`]: crate::post
/// [`Client`]: crate::Client
#[derive(Debug)]
pub struct Request {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Body,
    pub(crate) timeout: Duration,
    pub(crate) connect_timeout: Duration,
    pub(crate) stall_timeout: Duration,
    pub(crate) follow_redirects: bool,
    pub(crate) max_redirects: u32,
    pub(crate) http_version: Option<HttpVersion>,
    pub(crate) proxy: Option<String>,
    pub(crate) unix_socket: Option<PathBuf>,
    pub(crate) ca_bundle: Option<PathBuf>,
    pub(crate) verify_tls: bool,
    pub(crate) compression: bool,
    pub(crate) auth: Option<Auth>,
    pub(crate) netrc: bool,
    /// Certificate and key files for TLS client authentication.
    pub(crate) client_cert: Option<(PathBuf, PathBuf)>,
    pub(crate) progress: Option<ProgressFn>,
    pub(crate) max_body_size: Option<u64>,
    /// First error from a builder method, returned by `send`.
    error: Option<Error>,
}

/// A request body ready for libcurl.
pub(crate) struct Upload {
    /// Moved into the transfer's handler while it runs.
    pub(crate) source: Option<Box<dyn ReadSeek>>,
    pub(crate) len: u64,
}

impl Request {
    /// Starts a request. `url` may contain a query string, which is kept; a
    /// fragment is dropped. The URL is parsed by libcurl when the request is
    /// sent.
    pub fn new(method: Method, url: &str) -> Self {
        let url = url.trim();
        let url = url.split_once('#').map_or(url, |(before, _)| before);
        Request {
            method,
            url: url.to_owned(),
            headers: HeaderMap::new(),
            body: Body::Empty,
            timeout: Duration::ZERO,
            connect_timeout: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(30),
            follow_redirects: true,
            max_redirects: 10,
            http_version: None,
            proxy: None,
            unix_socket: None,
            ca_bundle: None,
            verify_tls: true,
            compression: true,
            auth: None,
            netrc: false,
            client_cert: None,
            progress: None,
            max_body_size: Some(64 * 1024 * 1024),
            error: None,
        }
    }

    /// Sets a header, replacing any previous value with the same name.
    ///
    /// Header values must be visible ASCII. Pass a [`HeaderValue`] built with
    /// `HeaderValue::from_bytes` for opaque octets.
    pub fn header<N, V>(mut self, name: N, value: V) -> Self
    where
        N: TryInto<HeaderName>,
        N::Error: Into<Error>,
        V: TryInto<HeaderValue>,
        V::Error: Into<Error>,
    {
        match (name.try_into(), value.try_into()) {
            (Ok(name), Ok(value)) => {
                self.headers.insert(name, value);
            }
            (Err(e), _) => self.record(e.into()),
            (_, Err(e)) => self.record(e.into()),
        }
        self
    }

    /// Replaces all headers.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Appends a query parameter. Name and value are percent-encoded. A pair
    /// with an empty value is serialized as just its name.
    pub fn query(mut self, name: &str, value: &str) -> Self {
        let pair = encode_form([(name, value)]);
        match self.url.split_once('?') {
            None => {
                self.url.push('?');
            }
            Some((_, query)) if !query.is_empty() && !query.ends_with('&') => {
                self.url.push('&');
            }
            Some(_) => {}
        }
        self.url.push_str(&pair);
        self
    }

    /// Sends `data` as the body with the given `Content-Type`, unless a
    /// `Content-Type` header was set explicitly. The data is not copied.
    pub fn body(mut self, content_type: Mime, data: impl Into<Bytes>) -> Self {
        self.body = Body::Raw {
            data: data.into(),
            content_type: Some(content_type),
        };
        self
    }

    /// Streams the body from `reader` with the given `Content-Type`, unless
    /// a `Content-Type` header was set explicitly. Nothing is buffered beyond
    /// libcurl's upload chunk.
    ///
    /// The reader has to seek: its length is taken from seeking to the end,
    /// and it is rewound for `307`/`308` redirects and for retries on a
    /// connection the server closed. For in-memory data use
    /// [`Request::body`], which does not copy.
    pub fn body_reader(mut self, content_type: Mime, reader: impl Read + Seek + 'static) -> Self {
        self.body = Body::Reader {
            reader: Box::new(reader),
            content_type,
        };
        self
    }

    /// Sends `value` serialized as JSON with `application/json`, unless a
    /// `Content-Type` header was set explicitly. A serialization error is
    /// returned from `send`.
    #[cfg(feature = "json")]
    pub fn json<T: serde::Serialize + ?Sized>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(data) => self.body(mime::APPLICATION_JSON, data),
            Err(e) => {
                self.record(e.into());
                self
            }
        }
    }

    /// Sends `pairs` as an `application/x-www-form-urlencoded` body.
    pub fn form<'a>(mut self, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        self.body = Body::Form(encode_form(pairs));
        self
    }

    /// Sends a `multipart/form-data` body.
    pub fn multipart(mut self, multipart: Multipart) -> Self {
        self.body = Body::Multipart(multipart);
        self
    }

    /// Time limit for one transfer, each redirect hop counting separately.
    /// Default none: a transfer that keeps moving may take as long as it
    /// needs, one that stops moving is caught by [`Request::stall_timeout`].
    /// Zero means no limit.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Time limit for establishing the connection, including the TLS
    /// handshake, default 30 seconds. Zero restores libcurl's default of
    /// 300 seconds.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Aborts the transfer once no data has moved in either direction for
    /// this long, default 30 seconds. Unlike [`Request::timeout`] it does
    /// not punish a large download for being large. Rounded up to whole
    /// seconds; zero disables the check. A stalled transfer fails with
    /// [`Error::Curl`] and `is_operation_timedout()`.
    ///
    /// Time a [`send_reader`](Request::send_reader) spends waiting for its
    /// consumer does not count: libcurl only runs while the reader is read.
    pub fn stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = timeout;
        self
    }

    /// Whether to follow 3xx redirects, default `true`. When disabled, the
    /// redirect response itself is returned.
    ///
    /// Redirects are followed by this crate, not by libcurl: `303` turns any
    /// method into `GET`, `301` and `302` turn `POST` into `GET`, `307` and
    /// `308` resend the body with the same method. Only `http` and `https`
    /// targets are followed, anything else is [`Error::UnsupportedScheme`].
    /// `Authorization`, `Cookie` and [`Request::auth`] credentials are
    /// dropped when the redirect leaves the origin (scheme, host and port);
    /// other headers, such as an API key of your own, travel with the
    /// request, as do proxy, Unix socket and client certificate settings,
    /// like in curl.
    pub fn follow_redirects(mut self, follow: bool) -> Self {
        self.follow_redirects = follow;
        self
    }

    /// Maximum number of redirects to follow before failing with
    /// [`Error::TooManyRedirects`], default 10.
    pub fn max_redirects(mut self, max: u32) -> Self {
        self.max_redirects = max;
        self
    }

    /// The HTTP version to use. By default libcurl decides: HTTP/2 over TLS
    /// when it was built with HTTP/2 and the server offers it, HTTP/1.1
    /// otherwise.
    pub fn http_version(mut self, version: HttpVersion) -> Self {
        self.http_version = Some(version);
        self
    }

    /// Sends the request through `proxy`, such as `http://proxy:3128` or
    /// `socks5h://localhost:1080`. Without this, libcurl honours the
    /// `http_proxy`, `https_proxy`, `all_proxy` and `no_proxy` environment
    /// variables.
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// Connects to a Unix domain socket instead of the URL's host, which is
    /// still sent as `Host`. For Docker, systemd and similar local APIs.
    pub fn unix_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.unix_socket = Some(path.into());
        self
    }

    /// A PEM file with the CA certificates to trust, instead of the system
    /// store.
    ///
    /// Without this, on a libcurl built on OpenSSL with a hashed certificate
    /// directory (`/etc/ssl/certs/<hash>.0`, as Debian, Ubuntu, Alpine and
    /// Arch ship it), `curlite` lets OpenSSL look the needed CA
    /// certificates up by hash instead of parsing the whole bundle, which
    /// saves about 20 ms of CPU per process on x86 and more on small
    /// devices. Setting a bundle turns that off.
    pub fn ca_bundle(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_bundle = Some(path.into());
        self
    }

    /// Skips TLS certificate and host name verification. For development
    /// against self-signed certificates only.
    pub fn danger_accept_invalid_certs(mut self) -> Self {
        self.verify_tls = false;
        self
    }

    /// Whether to ask for compressed responses (`Accept-Encoding` with every
    /// encoding this libcurl can decode) and decode them transparently,
    /// default `true`. Off, the raw body is returned.
    pub fn compression(mut self, enabled: bool) -> Self {
        self.compression = enabled;
        self
    }

    /// Authenticates with the given method. Credentials are dropped, like an
    /// `Authorization` header, when a redirect leaves the origin.
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// [`Auth::Basic`].
    pub fn basic_auth(self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth(Auth::Basic {
            user: user.into(),
            password: password.into(),
        })
    }

    /// [`Auth::Bearer`].
    pub fn bearer_auth(self, token: impl Into<String>) -> Self {
        self.auth(Auth::Bearer(token.into()))
    }

    /// Takes credentials for the host from `~/.netrc` when the request has
    /// none, like `curl --netrc-optional`. Looked up per host, so they
    /// survive redirects.
    pub fn netrc(mut self) -> Self {
        self.netrc = true;
        self
    }

    /// Authenticates the TLS connection with a client certificate: PEM
    /// files with the certificate and the private key, which may be the
    /// same file.
    pub fn client_cert(mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.client_cert = Some((cert.into(), key.into()));
        self
    }

    /// Upper bound for a body that [`Request::send`] holds in memory,
    /// default 64 MiB. A larger `Content-Length` fails before the body is
    /// read, a body that grows past the bound fails as it arrives, both
    /// with [`Error::BodyTooLarge`]. `None` removes the bound.
    /// [`Request::send_to`] and [`Request::send_reader`] are not bounded.
    pub fn max_body_size(mut self, limit: Option<u64>) -> Self {
        self.max_body_size = limit;
        self
    }

    /// Reports transfer progress. `f` runs between libcurl's steps whenever
    /// the byte counts change and once more when the transfer ends, never
    /// inside a libcurl callback, so it may block or send requests of its
    /// own. The counts are per transfer: a redirect that is followed starts
    /// them over.
    pub fn on_progress(mut self, f: impl FnMut(Progress) + 'static) -> Self {
        self.progress = Some(ProgressFn(Box::new(f)));
        self
    }

    /// Performs the request and reads the whole response into memory.
    ///
    /// Requests on the same thread share one connection cache, so calls to
    /// the same host reuse the connection.
    pub fn send(mut self) -> Result<Response> {
        let upload = self.prepare()?;
        transport::send(self, upload)
    }

    /// Performs the request and writes the response body to `writer` as it
    /// arrives, instead of holding it in memory. The returned response has
    /// an empty body. Bodies of redirects that are followed are discarded.
    ///
    /// The writer runs between libcurl's steps, never inside its callbacks,
    /// so it may send requests of its own.
    pub fn send_to(mut self, writer: &mut dyn Write) -> Result<Response> {
        let upload = self.prepare()?;
        transport::send_to(self, upload, writer)
    }

    /// Performs the request up to the response headers and returns the body
    /// as a reader that pulls from the connection on demand, with
    /// backpressure.
    pub fn send_reader(mut self) -> Result<ResponseReader> {
        let upload = self.prepare()?;
        ResponseReader::new(self, upload)
    }

    fn prepare(&mut self) -> Result<Option<Upload>> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        self.prepare_body()
    }

    /// The URL the request will be sent to, including query parameters.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn record(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Takes the body out of the request, sets the headers it implies
    /// (`Content-Type`, a default `User-Agent`) and returns what to upload.
    pub(crate) fn prepare_body(&mut self) -> Result<Option<Upload>> {
        let set_content_type = |headers: &mut HeaderMap, mime: &Mime| -> Result<()> {
            headers.insert(CONTENT_TYPE, HeaderValue::from_str(mime.as_ref())?);
            Ok(())
        };
        if !self.headers.contains_key(USER_AGENT) {
            self.headers.insert(
                USER_AGENT,
                HeaderValue::from_static(concat!("curlite/", env!("CARGO_PKG_VERSION"))),
            );
        }
        let upload = match std::mem::take(&mut self.body) {
            Body::Empty => None,
            Body::Raw { data, content_type } => {
                if let Some(ct) = content_type
                    && !self.headers.contains_key(CONTENT_TYPE)
                {
                    set_content_type(&mut self.headers, &ct)?;
                }
                Some(Upload::segments(vec![data]))
            }
            Body::Form(encoded) => {
                set_content_type(&mut self.headers, &mime::APPLICATION_WWW_FORM_URLENCODED)?;
                Some(Upload::segments(vec![Bytes::from(encoded)]))
            }
            Body::Multipart(multipart) => {
                let boundary = boundary();
                let mime: Mime = format!("multipart/form-data; boundary={boundary}")
                    .parse()
                    .expect("valid boundary");
                set_content_type(&mut self.headers, &mime)?;
                Some(Upload::segments(multipart.into_segments(&boundary)))
            }
            Body::Reader {
                mut reader,
                content_type,
            } => {
                if !self.headers.contains_key(CONTENT_TYPE) {
                    set_content_type(&mut self.headers, &content_type)?;
                }
                let len = reader.seek(SeekFrom::End(0))?;
                reader.seek(SeekFrom::Start(0))?;
                Some(Upload {
                    source: Some(reader),
                    len,
                })
            }
        };
        Ok(upload)
    }
}

impl Upload {
    fn segments(parts: Vec<Bytes>) -> Self {
        let segments = Segments::new(parts);
        Upload {
            len: segments.len(),
            source: Some(Box::new(segments)),
        }
    }
}

macro_rules! shorthand {
    ($($(#[$doc:meta])* $name:ident => $method:ident),* $(,)?) => {
        $(
            $(#[$doc])*
            pub fn $name(url: &str) -> Request {
                Request::new(Method::$method, url)
            }
        )*
    };
}

shorthand! {
    /// Starts a `GET` request.
    get => GET,
    /// Starts a `POST` request.
    post => POST,
    /// Starts a `PUT` request.
    put => PUT,
    /// Starts a `PATCH` request.
    patch => PATCH,
    /// Starts a `DELETE` request.
    delete => DELETE,
    /// Starts a `HEAD` request.
    head => HEAD,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn url_keeps_query_and_drops_fragment() {
        let req = get(" https://example.com/path?test=value&onlyname#frag ");
        assert_eq!(req.url(), "https://example.com/path?test=value&onlyname");
    }

    #[test]
    fn query_appends_encoded_pairs() {
        let req = get("https://example.com/?test=value&onlyname")
            .query("b", "b 1")
            .query("c", "");
        assert_eq!(
            req.url(),
            "https://example.com/?test=value&onlyname&b=b+1&c"
        );
        let req = get("https://example.com").query("a", "=");
        assert_eq!(req.url(), "https://example.com?a=%3D");
        let req = get("https://example.com/?").query("a", "1");
        assert_eq!(req.url(), "https://example.com/?a=1");
        let req = get("https://example.com/?x=1&").query("a", "1");
        assert_eq!(req.url(), "https://example.com/?x=1&a=1");
    }

    #[test]
    fn invalid_url_surfaces_at_send() {
        let err = get("not a url").send().unwrap_err();
        assert!(
            matches!(err, Error::Curl(ref e) if e.is_url_malformed()),
            "{err:?}"
        );
    }

    #[test]
    fn invalid_header_surfaces_at_send() {
        let err = get("http://localhost")
            .header("bad header", "x")
            .send()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeaderName(_)));
        let err = get("http://localhost")
            .header("ok", "bad\nvalue")
            .send()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeaderValue(_)));
    }

    fn read_all(upload: Option<Upload>) -> (u64, Vec<u8>) {
        let mut upload = upload.expect("a body");
        let mut out = Vec::new();
        upload
            .source
            .as_mut()
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        (upload.len, out)
    }

    #[test]
    fn body_sets_content_type_unless_present() {
        let mut req = post("http://localhost").body(mime::TEXT_XML, "<a/>");
        let upload = req.prepare_body().unwrap();
        assert_eq!(req.headers[CONTENT_TYPE], "text/xml");
        assert_eq!(read_all(upload), (4, b"<a/>".to_vec()));

        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "application/custom")
            .body(mime::TEXT_XML, "<a/>");
        req.prepare_body().unwrap();
        assert_eq!(req.headers[CONTENT_TYPE], "application/custom");

        let mut req =
            post("http://localhost").body_reader(mime::TEXT_PLAIN, Cursor::new(b"hello".to_vec()));
        let upload = req.prepare_body().unwrap();
        assert_eq!(req.headers[CONTENT_TYPE], "text/plain");
        assert_eq!(read_all(upload), (5, b"hello".to_vec()));
    }

    #[test]
    fn form_and_multipart_own_the_content_type() {
        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "text/plain")
            .form([("a", "b"), ("c", "d")]);
        let upload = req.prepare_body().unwrap();
        assert_eq!(
            req.headers[CONTENT_TYPE],
            "application/x-www-form-urlencoded"
        );
        assert_eq!(read_all(upload).1, b"a=b&c=d");

        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "text/plain")
            .multipart(Multipart::new().text("a", "b"));
        let upload = req.prepare_body().unwrap();
        let content_type = req.headers[CONTENT_TYPE].to_str().unwrap().to_owned();
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let (len, encoded) = read_all(upload);
        assert_eq!(len as usize, encoded.len());
        assert!(encoded.starts_with(format!("--{boundary}\r\n").as_bytes()));
    }

    #[test]
    fn default_user_agent() {
        let mut req = get("http://localhost");
        req.prepare_body().unwrap();
        assert_eq!(
            req.headers[USER_AGENT],
            concat!("curlite/", env!("CARGO_PKG_VERSION"))
        );
        let mut req = get("http://localhost").header(USER_AGENT, "mine");
        req.prepare_body().unwrap();
        assert_eq!(req.headers[USER_AGENT], "mine");
    }
}
