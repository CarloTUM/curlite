use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};

use std::path::PathBuf;
use std::time::Duration;

use crate::request::{Auth, HttpVersion, Request};

/// Defaults shared by several requests: headers, timeouts and redirect
/// handling. Requests built from a client start with these values and can
/// override each of them.
///
/// ```no_run
/// use std::time::Duration;
///
/// use curlite::{HeaderValue, header::AUTHORIZATION};
///
/// let api = curlite::Client::new()
///     .header(AUTHORIZATION, HeaderValue::from_static("Bearer secret"))
///     .timeout(Duration::from_secs(5));
/// let me = api.get("https://example.com/me").send()?;
/// let them = api.get("https://example.com/them").send()?;
/// # Ok::<(), curlite::Error>(())
/// ```
///
/// A client holds configuration only. The libcurl handle, and with it the
/// connection cache, belongs to the thread; see [`Request::send`].
#[derive(Debug, Clone)]
pub struct Client {
    headers: HeaderMap,
    timeout: Duration,
    connect_timeout: Duration,
    stall_timeout: Duration,
    follow_redirects: bool,
    max_redirects: u32,
    http_version: Option<HttpVersion>,
    proxy: Option<String>,
    unix_socket: Option<PathBuf>,
    ca_bundle: Option<PathBuf>,
    verify_tls: bool,
    compression: bool,
    auth: Option<Auth>,
    netrc: bool,
}

impl Default for Client {
    fn default() -> Self {
        let defaults = Request::new(Method::GET, "");
        Client {
            headers: HeaderMap::new(),
            timeout: defaults.timeout,
            connect_timeout: defaults.connect_timeout,
            stall_timeout: defaults.stall_timeout,
            follow_redirects: defaults.follow_redirects,
            max_redirects: defaults.max_redirects,
            http_version: defaults.http_version,
            proxy: defaults.proxy,
            unix_socket: defaults.unix_socket,
            ca_bundle: defaults.ca_bundle,
            verify_tls: defaults.verify_tls,
            compression: defaults.compression,
            auth: defaults.auth,
            netrc: defaults.netrc,
        }
    }
}

impl Client {
    /// A client with the same defaults as the free functions.
    pub fn new() -> Self {
        Self::default()
    }

    /// A header sent with every request, unless the request sets the same
    /// name itself. Takes parsed types so that building a client cannot
    /// fail; [`Request::header`] accepts strings.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// See [`Request::timeout`].
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// See [`Request::connect_timeout`].
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// See [`Request::stall_timeout`].
    pub fn stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = timeout;
        self
    }

    /// See [`Request::follow_redirects`].
    pub fn follow_redirects(mut self, follow: bool) -> Self {
        self.follow_redirects = follow;
        self
    }

    /// See [`Request::max_redirects`].
    pub fn max_redirects(mut self, max: u32) -> Self {
        self.max_redirects = max;
        self
    }

    /// See [`Request::http_version`].
    pub fn http_version(mut self, version: HttpVersion) -> Self {
        self.http_version = Some(version);
        self
    }

    /// See [`Request::proxy`].
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// See [`Request::unix_socket`].
    pub fn unix_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.unix_socket = Some(path.into());
        self
    }

    /// See [`Request::ca_bundle`].
    pub fn ca_bundle(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_bundle = Some(path.into());
        self
    }

    /// See [`Request::danger_accept_invalid_certs`].
    pub fn danger_accept_invalid_certs(mut self) -> Self {
        self.verify_tls = false;
        self
    }

    /// See [`Request::compression`].
    pub fn compression(mut self, enabled: bool) -> Self {
        self.compression = enabled;
        self
    }

    /// See [`Request::auth`].
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// See [`Request::basic_auth`].
    pub fn basic_auth(self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth(Auth::Basic {
            user: user.into(),
            password: password.into(),
        })
    }

    /// See [`Request::bearer_auth`].
    pub fn bearer_auth(self, token: impl Into<String>) -> Self {
        self.auth(Auth::Bearer(token.into()))
    }

    /// See [`Request::netrc`].
    pub fn netrc(mut self) -> Self {
        self.netrc = true;
        self
    }

    /// Starts a request with this client's defaults.
    pub fn request(&self, method: Method, url: &str) -> Request {
        let mut request = Request::new(method, url);
        request.headers = self.headers.clone();
        request.timeout = self.timeout;
        request.connect_timeout = self.connect_timeout;
        request.stall_timeout = self.stall_timeout;
        request.follow_redirects = self.follow_redirects;
        request.max_redirects = self.max_redirects;
        request.http_version = self.http_version;
        request.proxy = self.proxy.clone();
        request.unix_socket = self.unix_socket.clone();
        request.ca_bundle = self.ca_bundle.clone();
        request.verify_tls = self.verify_tls;
        request.compression = self.compression;
        request.auth = self.auth.clone();
        request.netrc = self.netrc;
        request
    }

    pub fn get(&self, url: &str) -> Request {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: &str) -> Request {
        self.request(Method::POST, url)
    }

    pub fn put(&self, url: &str) -> Request {
        self.request(Method::PUT, url)
    }

    pub fn patch(&self, url: &str) -> Request {
        self.request(Method::PATCH, url)
    }

    pub fn delete(&self, url: &str) -> Request {
        self.request(Method::DELETE, url)
    }

    pub fn head(&self, url: &str) -> Request {
        self.request(Method::HEAD, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::USER_AGENT;

    #[test]
    fn requests_inherit_and_override() {
        let client = Client::new()
            .header(USER_AGENT, HeaderValue::from_static("client"))
            .header(
                HeaderName::from_static("x-a"),
                HeaderValue::from_static("1"),
            )
            .timeout(Duration::from_secs(3))
            .connect_timeout(Duration::from_secs(1))
            .stall_timeout(Duration::from_secs(2))
            .follow_redirects(false)
            .max_redirects(2)
            .http_version(HttpVersion::Http11)
            .proxy("http://proxy:3128")
            .unix_socket("/run/x.sock")
            .ca_bundle("/etc/ca.pem")
            .danger_accept_invalid_certs()
            .compression(false)
            .bearer_auth("t")
            .netrc();
        let req = client.post("http://localhost").header("X-A", "2");
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.headers[USER_AGENT], "client");
        assert_eq!(req.headers["x-a"], "2");
        assert_eq!(req.timeout, Duration::from_secs(3));
        assert_eq!(req.connect_timeout, Duration::from_secs(1));
        assert_eq!(req.stall_timeout, Duration::from_secs(2));
        assert!(!req.follow_redirects);
        assert_eq!(req.max_redirects, 2);
        assert_eq!(req.http_version, Some(HttpVersion::Http11));
        assert_eq!(req.proxy.as_deref(), Some("http://proxy:3128"));
        assert_eq!(
            req.unix_socket.as_deref(),
            Some(std::path::Path::new("/run/x.sock"))
        );
        assert_eq!(
            req.ca_bundle.as_deref(),
            Some(std::path::Path::new("/etc/ca.pem"))
        );
        assert!(!req.verify_tls);
        assert!(!req.compression);
        assert_eq!(req.auth, Some(Auth::Bearer("t".into())));
        assert!(req.netrc);
    }
}
