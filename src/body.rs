use bytes::Bytes;
use mime::Mime;

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Read, Seek, SeekFrom};

/// `Read + Seek`, boxed. Seeking is needed to rewind a body for `307`/`308`
/// redirects and for libcurl's retries on dead keep-alive connections.
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// The body of a request.
#[derive(Default)]
pub(crate) enum Body {
    #[default]
    Empty,
    /// Sent as is. `content_type` is set as the `Content-Type` header unless
    /// the request already has one.
    Raw {
        data: Bytes,
        content_type: Option<Mime>,
    },
    /// `application/x-www-form-urlencoded`, already encoded.
    Form(String),
    Multipart(Multipart),
    /// Streamed from a reader; its length comes from seeking to the end.
    Reader {
        reader: Box<dyn ReadSeek>,
        content_type: Mime,
    },
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Body::Empty => f.write_str("Empty"),
            Body::Raw { data, content_type } => f
                .debug_struct("Raw")
                .field("len", &data.len())
                .field("content_type", content_type)
                .finish(),
            Body::Form(encoded) => f.debug_tuple("Form").field(encoded).finish(),
            Body::Multipart(multipart) => f.debug_tuple("Multipart").field(multipart).finish(),
            Body::Reader { content_type, .. } => f
                .debug_struct("Reader")
                .field("content_type", content_type)
                .finish_non_exhaustive(),
        }
    }
}

/// A `multipart/form-data` body.
///
/// ```
/// use curlite::Multipart;
///
/// let form = Multipart::new()
///     .text("description", "quarterly report")
///     .file("report", "q3.pdf", mime::APPLICATION_PDF, vec![0u8; 16]);
/// ```
///
/// The body is encoded by this crate and streamed to libcurl part by part;
/// part contents are not copied.
#[derive(Debug, Clone, Default)]
pub struct Multipart {
    pub(crate) parts: Vec<Part>,
}

/// One part of a [`Multipart`] body.
#[derive(Debug, Clone)]
pub struct Part {
    pub(crate) name: String,
    pub(crate) data: Bytes,
    pub(crate) content_type: Option<Mime>,
    pub(crate) filename: Option<String>,
}

impl Multipart {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a text field. Field parts carry no `Content-Type`.
    pub fn text(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.part(Part {
            name: name.into(),
            data: Bytes::from(value.into()),
            content_type: None,
            filename: None,
        })
    }

    /// Adds a part with a `Content-Type` and no filename.
    pub fn bytes(
        self,
        name: impl Into<String>,
        content_type: Mime,
        data: impl Into<Bytes>,
    ) -> Self {
        self.part(Part {
            name: name.into(),
            data: data.into(),
            content_type: Some(content_type),
            filename: None,
        })
    }

    /// Adds a file part, i.e. one with a `filename` in its `Content-Disposition`.
    pub fn file(
        self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: Mime,
        data: impl Into<Bytes>,
    ) -> Self {
        self.part(Part {
            name: name.into(),
            data: data.into(),
            content_type: Some(content_type),
            filename: Some(filename.into()),
        })
    }

    fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Encodes the body as `multipart/form-data` with `boundary`, as a list of
    /// segments: generated framing and the parts' own buffers, in order.
    pub(crate) fn into_segments(self, boundary: &str) -> Vec<Bytes> {
        let mut segments = Vec::with_capacity(self.parts.len() * 3 + 1);
        for part in self.parts {
            let mut head = format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
                escape_quoted(&part.name)
            );
            if let Some(filename) = &part.filename {
                head.push_str(&format!("; filename=\"{}\"", escape_quoted(filename)));
            }
            head.push_str("\r\n");
            if let Some(content_type) = &part.content_type {
                head.push_str(&format!("Content-Type: {content_type}\r\n"));
            }
            head.push_str("\r\n");
            segments.push(Bytes::from(head));
            segments.push(part.data);
            segments.push(Bytes::from_static(b"\r\n"));
        }
        segments.push(Bytes::from(format!("--{boundary}--\r\n")));
        segments
    }
}

/// Percent-encodes the three characters that would break a quoted
/// `Content-Disposition` parameter, as browsers do (WHATWG `multipart/form-data`).
fn escape_quoted(value: &str) -> String {
    value
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22")
}

/// A boundary that does not occur in any sane payload: 32 random hex digits.
pub(crate) fn boundary() -> String {
    // `RandomState::new()` is seeded randomly per thread and advances on
    // every call, so two states hash the same input to different values
    let a = RandomState::new().hash_one(0u8);
    let b = RandomState::new().hash_one(1u8);
    format!("----curlite-{a:016x}{b:016x}")
}

/// Several buffers read back to back, with `Seek` over the concatenation.
pub(crate) struct Segments {
    parts: Vec<Bytes>,
    pos: u64,
    len: u64,
}

impl Segments {
    pub(crate) fn new(parts: Vec<Bytes>) -> Self {
        let len = parts.iter().map(|p| p.len() as u64).sum();
        Segments { parts, pos: 0, len }
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

impl Read for Segments {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        let mut start = 0u64;
        for part in &self.parts {
            let end = start + part.len() as u64;
            if self.pos < end && filled < buf.len() {
                let offset = (self.pos - start) as usize;
                let n = (part.len() - offset).min(buf.len() - filled);
                buf[filled..filled + n].copy_from_slice(&part[offset..offset + n]);
                filled += n;
                self.pos += n as u64;
            }
            start = end;
        }
        Ok(filled)
    }
}

impl Seek for Segments {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let pos = match target {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        match pos {
            Some(pos) if pos <= self.len => {
                self.pos = pos;
                Ok(pos)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside the body",
            )),
        }
    }
}

/// Percent-encodes `pairs` as `application/x-www-form-urlencoded`.
///
/// A pair with an empty value is serialized as just its name, so that
/// `?flag` style parameters round-trip.
pub(crate) fn encode_form<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.extend(form_urlencoded::byte_serialize(name.as_bytes()));
        if !value.is_empty() {
            out.push('=');
            out.extend(form_urlencoded::byte_serialize(value.as_bytes()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding() {
        assert_eq!(encode_form([("a", "b"), ("c", "d")]), "a=b&c=d");
        assert_eq!(
            encode_form([("flag", ""), ("x y", "=&")]),
            "flag&x+y=%3D%26"
        );
        assert_eq!(encode_form([]), "");
    }

    #[test]
    fn multipart_encoding() {
        let segments = Multipart::new()
            .text("a \"q\"", "1")
            .file(
                "f",
                "x\ny.bin",
                mime::APPLICATION_OCTET_STREAM,
                &b"\x00\x01"[..],
            )
            .into_segments("B");
        let mut all = Vec::new();
        for s in &segments {
            all.extend_from_slice(s);
        }
        assert_eq!(
            all,
            b"--B\r\nContent-Disposition: form-data; name=\"a %22q%22\"\r\n\r\n1\r\n\
              --B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"x%0Ay.bin\"\r\n\
              Content-Type: application/octet-stream\r\n\r\n\x00\x01\r\n\
              --B--\r\n"
        );
        assert!(Multipart::new().into_segments("B").concat() == b"--B--\r\n");
    }

    #[test]
    fn segments_read_and_seek() {
        let mut s = Segments::new(vec![
            Bytes::from_static(b"ab"),
            Bytes::new(),
            Bytes::from_static(b"cde"),
        ]);
        assert_eq!(s.len(), 5);
        let mut buf = [0u8; 4];
        assert_eq!(s.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"abcd");
        assert_eq!(s.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf[..1], b"e");
        assert_eq!(s.read(&mut buf).unwrap(), 0);

        assert_eq!(s.seek(SeekFrom::Start(1)).unwrap(), 1);
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"bcde");
        assert_eq!(s.seek(SeekFrom::End(-2)).unwrap(), 3);
        assert_eq!(s.seek(SeekFrom::Current(-1)).unwrap(), 2);
        assert!(s.seek(SeekFrom::Start(6)).is_err());
        assert!(s.seek(SeekFrom::Current(-3)).is_err());
    }

    #[test]
    fn boundaries_differ() {
        let a = boundary();
        let b = boundary();
        assert_ne!(a, b);
        assert_eq!(a.len(), "----curlite-".len() + 32);
    }
}
