/// Errors returned by this crate.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// libcurl failed to set an option or perform the transfer.
    #[error("curl error: {0}")]
    Curl(#[from] curl::Error),
    /// libcurl rejected a multipart part.
    #[error("curl form error: {0}")]
    CurlForm(#[from] curl::FormError),
    #[error("invalid header name: {0}")]
    InvalidHeaderName(#[from] http::header::InvalidHeaderName),
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
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
    Multipart(#[from] std::io::Error),
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
