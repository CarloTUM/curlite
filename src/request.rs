use bytes::Bytes;
use http::Method;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use mime::Mime;
use url::Url;

use std::time::Duration;

use crate::body::{Body, Multipart, encode_form};
use crate::error::{Error, Result};
use crate::response::Response;
use crate::transport;

/// A request under construction. Created with [`Request::new`] or one of the
/// free functions ([`get`], [`post`], ...), and sent with [`Request::send`].
///
/// Builder methods that can fail (`header`, `query`) record the error and
/// return it from `send`, so that a request can be built in one expression.
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
#[derive(Debug)]
pub struct Request {
    pub(crate) method: Method,
    pub(crate) url: Result<Url>,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Body,
    pub(crate) timeout: Duration,
    pub(crate) follow_redirects: bool,
    pub(crate) max_redirects: u32,
    /// First error from a builder method, returned by `send`.
    error: Option<Error>,
}

impl Request {
    /// Starts a request. `url` may contain a query string, which is kept.
    pub fn new(method: Method, url: &str) -> Self {
        let url = Url::parse(url.trim()).map_err(Error::from).map(|mut url| {
            // fragments are client side only and never sent
            url.set_fragment(None);
            url
        });
        Request {
            method,
            url,
            headers: HeaderMap::new(),
            body: Body::Empty,
            timeout: Duration::from_secs(20),
            follow_redirects: true,
            max_redirects: 10,
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
        if let Ok(url) = &mut self.url {
            let pair = encode_form([(name, value)]);
            let query = match url.query() {
                Some(existing) if !existing.is_empty() => format!("{existing}&{pair}"),
                _ => pair,
            };
            url.set_query(Some(&query));
        }
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

    /// Time limit for the whole transfer, default 20 seconds. Zero means no
    /// limit.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether to follow 3xx redirects, default `true`. When disabled, the
    /// redirect response itself is returned.
    pub fn follow_redirects(mut self, follow: bool) -> Self {
        self.follow_redirects = follow;
        self
    }

    /// Maximum number of redirects to follow before failing, default 10.
    pub fn max_redirects(mut self, max: u32) -> Self {
        self.max_redirects = max;
        self
    }

    /// Performs the request and reads the whole response into memory.
    ///
    /// Requests on the same thread share one libcurl handle, so connections
    /// to the same host are reused across calls.
    pub fn send(self) -> Result<Response> {
        if let Some(error) = self.error {
            return Err(error);
        }
        transport::perform(self)
    }

    /// The URL the request will be sent to, including query parameters.
    pub fn url(&self) -> Result<&Url> {
        self.url.as_ref().map_err(|e| match e {
            Error::Url(e) => Error::Url(*e),
            // `url` only ever holds a parse error
            _ => unreachable!(),
        })
    }

    pub(crate) fn record(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Sets the `Content-Type` header that the body implies.
    pub(crate) fn finalize_headers(&mut self) -> Result<()> {
        match &mut self.body {
            Body::Raw { content_type, .. } => {
                if let Some(ct) = content_type.take()
                    && !self.headers.contains_key(CONTENT_TYPE)
                {
                    self.headers
                        .insert(CONTENT_TYPE, HeaderValue::from_str(ct.as_ref())?);
                }
            }
            Body::Form(_) => {
                self.headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
            // libcurl sets the multipart content type with its boundary
            Body::Multipart(_) => {
                self.headers.remove(CONTENT_TYPE);
            }
            Body::Empty => {}
        }
        Ok(())
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

    #[test]
    fn url_keeps_query_and_drops_fragment() {
        let req = get("https://example.com/path?test=value&onlyname#frag");
        assert_eq!(
            req.url().unwrap().as_str(),
            "https://example.com/path?test=value&onlyname"
        );
    }

    #[test]
    fn query_appends_encoded_pairs() {
        let req = get("https://example.com/?test=value&onlyname")
            .query("b", "b 1")
            .query("c", "");
        assert_eq!(
            req.url().unwrap().as_str(),
            "https://example.com/?test=value&onlyname&b=b+1&c"
        );
        let req = get("https://example.com").query("a", "=");
        assert_eq!(req.url().unwrap().as_str(), "https://example.com/?a=%3D");
    }

    #[test]
    fn invalid_url_surfaces_at_send() {
        let err = get("not a url").send().unwrap_err();
        assert!(matches!(err, Error::Url(_)));
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

    #[test]
    fn body_sets_content_type_unless_present() {
        let mut req = post("http://localhost").body(mime::TEXT_XML, "<a/>");
        req.finalize_headers().unwrap();
        assert_eq!(req.headers[CONTENT_TYPE], "text/xml");
        assert!(matches!(
            req.body,
            Body::Raw {
                content_type: None,
                ..
            }
        ));

        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "application/custom")
            .body(mime::TEXT_XML, "<a/>");
        req.finalize_headers().unwrap();
        assert_eq!(req.headers[CONTENT_TYPE], "application/custom");
    }

    #[test]
    fn form_and_multipart_own_the_content_type() {
        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "text/plain")
            .form([("a", "b"), ("c", "d")]);
        req.finalize_headers().unwrap();
        assert_eq!(
            req.headers[CONTENT_TYPE],
            "application/x-www-form-urlencoded"
        );
        assert!(matches!(req.body, Body::Form(ref s) if s == "a=b&c=d"));

        let mut req = post("http://localhost")
            .header(CONTENT_TYPE, "text/plain")
            .multipart(Multipart::new().text("a", "b"));
        req.finalize_headers().unwrap();
        assert!(!req.headers.contains_key(CONTENT_TYPE));
    }
}
