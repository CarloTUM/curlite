use bytes::Bytes;
use http::StatusCode;
use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use mime::{BOUNDARY, Mime};

use std::str::FromStr;

use crate::error::{Error, Result};

/// A response, read completely into memory (or, for [`Request::send_to`],
/// with the body already written out).
///
/// [`Request::send_to`]: crate::Request::send_to
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
    /// The part's body, sharing the response buffer.
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
    /// Part bodies are slices of [`Response::body`]; nothing is copied.
    /// Nested multipart parts are not expanded.
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

    /// Turns a 4xx or 5xx status into [`Error::Status`], which keeps the
    /// response.
    pub fn error_for_status(self) -> Result<Self> {
        if self.status.is_client_error() || self.status.is_server_error() {
            Err(Error::Status(Box::new(self)))
        } else {
            Ok(self)
        }
    }

    /// Parses the body as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
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

/// Splits a multipart body (RFC 2046) into parts that share `body`'s buffer.
///
/// Line breaks are accepted as `\r\n` or bare `\n`. A preamble before the
/// first delimiter and an epilogue after the closing one are skipped.
pub(crate) fn parse_multipart(body: &Bytes, boundary: &str) -> Result<Vec<ResponsePart>> {
    let delimiter = format!("--{boundary}").into_bytes();
    let truncated = || Error::Multipart("body ends inside a part".to_owned());
    let malformed = |what: &dyn std::fmt::Display| Error::Multipart(what.to_string());
    let mut parts = Vec::new();
    // the first delimiter needs no preceding line break, but has to start a
    // line
    let mut pos = find_delimiter(body, 0, &delimiter, |i| i == 0 || body[i - 1] == b'\n')
        .ok_or_else(|| Error::Multipart(format!("boundary {boundary:?} not found")))?;
    loop {
        pos += delimiter.len();
        if body[pos..].starts_with(b"--") {
            return Ok(parts);
        }
        // the rest of the delimiter line is transport padding
        pos = find(body, pos, b"\n").ok_or_else(truncated)? + 1;

        let mut headers = HeaderMap::new();
        loop {
            let line_end = find(body, pos, b"\n").ok_or_else(truncated)?;
            let line = std::str::from_utf8(&body[pos..line_end])
                .map_err(|_| Error::Multipart("part header is not UTF-8".to_owned()))?
                .trim_end_matches('\r');
            pos = line_end + 1;
            if line.is_empty() {
                break;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(Error::Multipart(format!("malformed part header {line:?}")));
            };
            headers.append(
                HeaderName::from_str(name.trim()).map_err(|e| malformed(&e))?,
                HeaderValue::from_str(value.trim()).map_err(|e| malformed(&e))?,
            );
        }

        // the part body runs to the next delimiter line; the line break
        // before it belongs to the delimiter, so search from the blank
        // line's own `\n` to allow empty bodies
        let next = find_delimiter(body, pos - 1, &delimiter, |i| i > 0 && body[i - 1] == b'\n')
            .ok_or_else(truncated)?
            - 1;
        let mut end = next;
        if end > pos && body[end - 1] == b'\r' {
            end -= 1;
        }
        let end = end.max(pos);

        // the header lines were checked to be UTF-8 above; `to_str` would
        // reject the non-ASCII names browsers send
        let disposition = headers
            .get(CONTENT_DISPOSITION)
            .map(|v| std::str::from_utf8(v.as_bytes()).map_err(|e| malformed(&e)))
            .transpose()?;
        parts.push(ResponsePart {
            name: disposition
                .and_then(|d| disposition_param(d, "name"))
                .unwrap_or_default(),
            filename: disposition.and_then(|d| disposition_param(d, "filename")),
            content_type: parse_content_type(&headers).map_err(|e| malformed(&e))?,
            body: body.slice(pos..end),
        });
        pos = next + 1;
    }
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|i| i + from)
}

/// The first delimiter at or after `from` that `at_line_start` accepts and
/// that is followed by `--`, transport padding, a line break or the end, so
/// that a boundary which is a prefix of a longer token does not match.
fn find_delimiter(
    body: &[u8],
    from: usize,
    delimiter: &[u8],
    at_line_start: impl Fn(usize) -> bool,
) -> Option<usize> {
    let mut from = from;
    loop {
        let i = find(body, from, delimiter)?;
        let after = body.get(i + delimiter.len());
        if at_line_start(i) && matches!(after, None | Some(b'-' | b' ' | b'\t' | b'\r' | b'\n')) {
            return Some(i);
        }
        from = i + 1;
    }
}

/// The value of `key` in a `Content-Disposition` style parameter list.
/// Quoted values may contain `;` and backslash escapes, which are resolved.
fn disposition_param(value: &str, key: &str) -> Option<String> {
    let mut rest = value.split_once(';')?.1;
    loop {
        let (k, after_eq) = rest.split_once('=')?;
        let k = k.trim();
        let (v, next) = match after_eq.trim_start().strip_prefix('"') {
            Some(quoted) => {
                let mut v = String::new();
                let mut chars = quoted.char_indices();
                let mut close = None;
                while let Some((i, c)) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some((_, escaped)) = chars.next() {
                                v.push(escaped);
                            }
                        }
                        '"' => {
                            close = Some(i);
                            break;
                        }
                        c => v.push(c),
                    }
                }
                // an unterminated quote takes the rest of the value
                let next = close.and_then(|i| quoted[i + 1..].split_once(';').map(|(_, n)| n));
                (v, next)
            }
            None => match after_eq.split_once(';') {
                Some((v, next)) => (v.trim().to_owned(), Some(next)),
                None => (after_eq.trim().to_owned(), None),
            },
        };
        if k.eq_ignore_ascii_case(key) {
            return Some(v);
        }
        rest = next?;
    }
}

/// Processes one header line from libcurl's header callback.
///
/// A new `HTTP/` status line clears `headers` and records the status, so
/// that headers of earlier phases (`100 Continue`) do not bleed into the
/// final response. Fails on lines that are not UTF-8 or that `http` does
/// not accept as header name or value.
pub(crate) fn process_header_line(
    line: &[u8],
    headers: &mut HeaderMap,
    status: &mut Option<StatusCode>,
) -> Result<()> {
    let line = std::str::from_utf8(line)
        .map_err(|_| Error::HeaderParse("header line is not UTF-8".to_owned()))?;
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Ok(());
    }
    if let Some(rest) = line.strip_prefix("HTTP/") {
        headers.clear();
        let code = rest
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| Error::HeaderParse(format!("malformed status line {line:?}")))?;
        *status = Some(StatusCode::from_u16(code)?);
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
        let mut status = None;
        let mut line = |l: &[u8]| process_header_line(l, &mut headers, &mut status);
        line(b"HTTP/1.1 100 Continue\r\n").unwrap();
        line(b"X-Early: 1\r\n").unwrap();
        line(b"\r\n").unwrap();
        assert!(line(b"HTTP/1.1 abc\r\n").is_err());
        line(b"HTTP/2 200\r\n").unwrap();
        line(b"Content-Type: text/plain\r\n").unwrap();
        line(b"X-Empty:\r\n").unwrap();
        line(b"garbage line\r\n").unwrap();
        assert!(line(b"Bad Name: x\r\n").is_err());
        assert!(line(b"X: \xff\r\n").is_err());
        assert_eq!(status, Some(StatusCode::OK));
        assert!(!headers.contains_key("x-early"));
        assert_eq!(headers[CONTENT_TYPE], "text/plain");
        assert_eq!(headers["x-empty"], "");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn multipart_parsing() {
        let body = Bytes::from_static(
            b"preamble\r\n--B\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n\
              --B  \r\nContent-Disposition: form-data; name=\"f\"; filename=\"x \\\"y\\\".bin\"\r\n\
              Content-Type: application/octet-stream\r\n\r\n\x00--B\x01\r\n\
              --B\r\n\r\n\r\n\
              --B\r\nContent-Type: text/plain\r\n\r\n\r\n\
              --B--\r\nepilogue",
        );
        let parts = parse_multipart(&body, "B").unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].name, "a");
        assert_eq!(parts[0].body, "1");
        assert_eq!(parts[0].content_type, None);
        assert_eq!(parts[1].name, "f");
        assert_eq!(parts[1].filename.as_deref(), Some("x \"y\".bin"));
        assert_eq!(parts[1].content_type, Some(mime::APPLICATION_OCTET_STREAM));
        assert_eq!(parts[1].body, &b"\x00--B\x01"[..]);
        assert_eq!(parts[2].name, "");
        assert_eq!(parts[2].body, "");
        assert_eq!(parts[3].content_type, Some(mime::TEXT_PLAIN));
        assert_eq!(parts[3].body, "");
        // slices, not copies
        assert_eq!(
            parts[0].body.as_ptr(),
            body[body.len() - body.len()..]
                .as_ptr()
                .wrapping_add(find(&body, 0, b"1\r\n").unwrap())
        );
    }

    #[test]
    fn disposition_params() {
        let d = r#"form-data; name="a;b"; filename="x \"y\" \\ z;.txt"; token=plain ; last=1"#;
        assert_eq!(disposition_param(d, "name").as_deref(), Some("a;b"));
        assert_eq!(
            disposition_param(d, "FILENAME").as_deref(),
            Some("x \"y\" \\ z;.txt")
        );
        assert_eq!(disposition_param(d, "token").as_deref(), Some("plain"));
        assert_eq!(disposition_param(d, "last").as_deref(), Some("1"));
        assert_eq!(disposition_param(d, "missing"), None);
        assert_eq!(disposition_param("form-data", "name"), None);
        assert_eq!(
            disposition_param("form-data; name=\"open", "name").as_deref(),
            Some("open")
        );
        assert_eq!(
            disposition_param("form-data; name=grüße", "name").as_deref(),
            Some("grüße")
        );
    }

    #[test]
    fn encoder_output_round_trips_through_the_parser() {
        let segments = crate::Multipart::new()
            .text("grüße", "ä;ö")
            .file(
                "f;g",
                "ré;sumé \"1\".pdf",
                mime::APPLICATION_PDF,
                &b"%PDF"[..],
            )
            .into_segments("xyz");
        let body = Bytes::from(segments.concat());
        let parts = parse_multipart(&body, "xyz").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "grüße");
        assert_eq!(parts[0].body, "ä;ö");
        assert_eq!(parts[1].name, "f;g");
        assert_eq!(parts[1].filename.as_deref(), Some("ré;sumé %221%22.pdf"));
        assert_eq!(parts[1].body, "%PDF");
    }

    #[test]
    fn boundary_prefix_is_not_a_delimiter() {
        let body = Bytes::from_static(
            b"--B\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nline\r\n--Bnot\r\n\r\nmore\r\n--B--\r\n",
        );
        let parts = parse_multipart(&body, "B").unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].body, &b"line\r\n--Bnot\r\n\r\nmore"[..]);
        // the first delimiter must start a line
        let body = Bytes::from_static(b"x--B\r\n\r\n1\r\n--B\r\n\r\n2\r\n--B--");
        let parts = parse_multipart(&body, "B").unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].body, "2");
        // malformed part headers are multipart errors
        let body = Bytes::from_static(b"--B\r\nBad Name: x\r\n\r\n\r\n--B--");
        assert!(matches!(
            parse_multipart(&body, "B").unwrap_err(),
            Error::Multipart(_)
        ));
    }

    #[test]
    fn multipart_bare_newlines_and_errors() {
        let body = Bytes::from_static(b"--B\nContent-Disposition: form-data; name=a\n\nx\n--B--\n");
        let parts = parse_multipart(&body, "B").unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "a");
        assert_eq!(parts[0].body, "x");

        let truncated = Bytes::from_static(b"--B\r\nContent-Type: text/plain\r\n\r\nx\r\n");
        assert!(matches!(
            parse_multipart(&truncated, "B").unwrap_err(),
            Error::Multipart(_)
        ));
        assert!(matches!(
            parse_multipart(&Bytes::from_static(b"nothing"), "B").unwrap_err(),
            Error::Multipart(_)
        ));
        assert!(
            parse_multipart(&Bytes::from_static(b"--B--"), "B")
                .unwrap()
                .is_empty()
        );
    }
}
