/// Errors returned by this crate.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// libcurl failed to set an option or perform the transfer. This includes
    /// malformed URLs, which libcurl parses.
    #[error("curl error: {0}")]
    Curl(#[from] curl::Error),
    #[error("invalid header name: {0}")]
    InvalidHeaderName(#[from] http::header::InvalidHeaderName),
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error("invalid status code: {0}")]
    InvalidStatusCode(#[from] http::status::InvalidStatusCode),
    /// A header value could not be represented as a string.
    #[error("header value is not visible ASCII: {0}")]
    HeaderToStr(#[from] http::header::ToStrError),
    /// The server sent a header line this client does not accept.
    #[error("header parse error: {0}")]
    HeaderParse(String),
    #[error("MIME parse error: {0}")]
    Mime(#[from] mime::FromStrError),
    /// A multipart body could not be parsed.
    #[error("multipart parse error: {0}")]
    Multipart(String),
    /// Reading a request body from a reader or writing a response body to a
    /// writer failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The server redirected more often than [`Request::max_redirects`]
    /// allows.
    ///
    /// [`Request::max_redirects`]: crate::Request::max_redirects
    #[error("too many redirects (more than {0})")]
    TooManyRedirects(u32),
    #[error("body is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Lets `Request::header` take a `HeaderName` or `HeaderValue` directly.
impl From<std::convert::Infallible> for Error {
    fn from(never: std::convert::Infallible) -> Self {
        match never {}
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
