use bytes::{Buf, Bytes};
use http::StatusCode;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use mime::{BOUNDARY, Mime};
use multipart::server::ReadEntry;

use std::io::Read;
use std::str::FromStr;

use crate::error::{Error, Result};

/// A response, read completely into memory.
#[derive(Debug, Clone)]
pub struct Response {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

/// One part of a `multipart/*` response body, see [`Response::parts`].
#[derive(Debug, Clone)]
pub struct ResponsePart {
    /// The `name` of the part's `Content-Disposition`, empty if absent.
    pub name: String,
    /// The `filename` of the part's `Content-Disposition`.
    pub filename: Option<String>,
    /// The part's `Content-Type`, if it has one.
    pub content_type: Option<Mime>,
    pub body: Bytes,
}

impl Response {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The body. Cloning the returned `Bytes` shares the buffer.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    pub fn into_body(self) -> Bytes {
        self.body
    }

    /// The body as a string, if it is valid UTF-8.
    pub fn text(&self) -> Result<&str> {
        Ok(std::str::from_utf8(&self.body)?)
    }

    /// The parsed `Content-Type` header, if present and valid.
    pub fn content_type(&self) -> Result<Option<Mime>> {
        parse_content_type(&self.headers)
    }

    /// Splits a `multipart/*` body into its parts.
    ///
    /// Each part's body shares no memory with [`Response::body`]; it is copied
    /// out once while parsing. Nested multipart parts are not expanded.
    pub fn parts(&self) -> Result<Vec<ResponsePart>> {
        let content_type = self.content_type()?.ok_or_else(|| {
            Error::HeaderParse("multipart response without Content-Type".to_owned())
        })?;
        if content_type.type_() != mime::MULTIPART {
            return Err(Error::HeaderParse(format!(
                "expected a multipart Content-Type, got {content_type}"
            )));
        }
        let boundary = content_type.get_param(BOUNDARY).ok_or_else(|| {
            Error::HeaderParse("multipart Content-Type without boundary".to_owned())
        })?;
        parse_multipart(&self.body, boundary.as_str())
    }

    /// Parses an `application/x-www-form-urlencoded` body into pairs.
    pub fn form(&self) -> Vec<(String, String)> {
        form_urlencoded::parse(&self.body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }
}

pub(crate) fn parse_content_type(headers: &HeaderMap) -> Result<Option<Mime>> {
    headers
        .get(CONTENT_TYPE)
        .map(|value| Ok(value.to_str()?.trim().parse::<Mime>()?))
        .transpose()
}

pub(crate) fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<ResponsePart>> {
    let mut parts = Vec::new();
    let mut multipart = multipart::server::Multipart::with_body(body.reader(), boundary);
    loop {
        match multipart.read_entry_mut() {
            multipart::server::ReadEntryResult::Entry(mut entry) => {
                let mut body = Vec::new();
                entry.data.read_to_end(&mut body)?;
                // read_to_end grows by doubling; return the overshoot before
                // the buffer is frozen into Bytes
                body.shrink_to_fit();
                parts.push(ResponsePart {
                    name: entry.headers.name.to_string(),
                    filename: entry.headers.filename.clone(),
                    content_type: entry.headers.content_type.clone(),
                    body: Bytes::from(body),
                });
            }
            multipart::server::ReadEntryResult::End(_) => return Ok(parts),
            multipart::server::ReadEntryResult::Error(_, error) => return Err(error.into()),
        }
    }
}

/// Processes one header line from libcurl's header callback.
///
/// Clears `headers` when a new `HTTP/` status line arrives, so that headers
/// of earlier phases (`100 Continue`, redirects) do not bleed into the final
/// response. Fails on lines that are not UTF-8 or that `http` does not accept
/// as header name or value.
pub(crate) fn process_header_line(line: &[u8], headers: &mut HeaderMap) -> Result<()> {
    let line = std::str::from_utf8(line)
        .map_err(|_| Error::HeaderParse("header line is not UTF-8".to_owned()))?;
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Ok(());
    }
    if line.starts_with("HTTP/") {
        headers.clear();
        return Ok(());
    }
    let Some((name, value)) = line.split_once(':') else {
        return Ok(());
    };
    headers.append(
        HeaderName::from_str(name.trim())?,
        HeaderValue::from_str(value.trim())?,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lines() {
        let mut headers = HeaderMap::new();
        process_header_line(b"HTTP/1.1 100 Continue\r\n", &mut headers).unwrap();
        process_header_line(b"X-Early: 1\r\n", &mut headers).unwrap();
        process_header_line(b"\r\n", &mut headers).unwrap();
        process_header_line(b"HTTP/1.1 200 OK\r\n", &mut headers).unwrap();
        process_header_line(b"Content-Type: text/plain\r\n", &mut headers).unwrap();
        process_header_line(b"X-Empty:\r\n", &mut headers).unwrap();
        process_header_line(b"garbage line\r\n", &mut headers).unwrap();
        assert!(!headers.contains_key("x-early"));
        assert_eq!(headers[CONTENT_TYPE], "text/plain");
        assert_eq!(headers["x-empty"], "");
        assert_eq!(headers.len(), 2);

        assert!(process_header_line(b"Bad Name: x\r\n", &mut headers).is_err());
        assert!(process_header_line(b"X: \xff\r\n", &mut headers).is_err());
    }
}
