use bytes::Bytes;
use mime::Mime;

/// The body of a request.
#[derive(Debug, Clone, Default)]
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
/// libcurl copies the contents of every part into its own buffers when the
/// request is sent, so a multipart body is held in memory twice for the
/// duration of the request. Send large payloads as a raw [`Request::body`]
/// if that matters.
///
/// [`Request::body`]: crate::Request::body
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
}
