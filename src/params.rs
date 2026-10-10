//! A parameter model for services that exchange named values and files.
//!
//! This is the model the [CPEE] process engine uses for service calls: a
//! request is a list of parameters, and the body format follows from them
//! instead of being chosen by the caller.
//!
//! * [`ParameterType::Query`] parameters go into the URL.
//! * A single body parameter is sent as the body itself: a
//!   [`Parameter::Simple`] one as `name=value` with
//!   `application/x-www-form-urlencoded`, a [`Parameter::Complex`] one as raw
//!   bytes with its MIME type.
//! * Several simple body parameters are sent as
//!   `application/x-www-form-urlencoded`.
//! * As soon as a complex parameter is involved, the body is
//!   `multipart/form-data`.
//!
//! Responses are parsed back into the same model with
//! [`Response::parameters`]: a form body becomes simple parameters, a
//! multipart body one parameter per part (recursively), and anything else a
//! single complex parameter named after the `Content-ID` header, or `result`.
//!
//! [CPEE]: https://cpee.org

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderMap};
use mime::Mime;

use crate::body::{Body, Multipart, encode_form};
use crate::error::{Error, Result};
use crate::request::Request;
use crate::response::{Response, parse_content_type, parse_multipart};

/// Where a [`Parameter::Simple`] goes.
///
/// The type alone decides; a `Body` parameter stays in the body even on `GET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParameterType {
    Query,
    Body,
}

/// A named value or file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Parameter {
    /// A text value. Name and value are stored unencoded and percent-encoded
    /// where the body format requires it.
    Simple {
        name: String,
        value: String,
        param_type: ParameterType,
    },
    /// A file or other typed payload.
    Complex {
        name: String,
        #[cfg_attr(feature = "serde", serde(serialize_with = "serialize_mime"))]
        content_type: Mime,
        /// `Bytes` so that parsed responses can share the buffer with the
        /// response body.
        #[cfg_attr(feature = "serde", serde(serialize_with = "serialize_bytes"))]
        content: Bytes,
    },
}

impl Parameter {
    pub fn simple(
        name: impl Into<String>,
        value: impl Into<String>,
        param_type: ParameterType,
    ) -> Self {
        Parameter::Simple {
            name: name.into(),
            value: value.into(),
            param_type,
        }
    }

    pub fn query(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::simple(name, value, ParameterType::Query)
    }

    pub fn body(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::simple(name, value, ParameterType::Body)
    }

    pub fn complex(name: impl Into<String>, content_type: Mime, content: impl Into<Bytes>) -> Self {
        Parameter::Complex {
            name: name.into(),
            content_type,
            content: content.into(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Parameter::Simple { name, .. } | Parameter::Complex { name, .. } => name,
        }
    }

    fn is_body(&self) -> bool {
        !matches!(
            self,
            Parameter::Simple {
                param_type: ParameterType::Query,
                ..
            }
        )
    }
}

#[cfg(feature = "serde")]
fn serialize_mime<S: serde::Serializer>(mime: &Mime, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(mime.as_ref())
}

#[cfg(feature = "serde")]
fn serialize_bytes<S: serde::Serializer>(bytes: &Bytes, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(bytes)
}

impl Request {
    /// Adds parameters and derives the body format from them, see the
    /// [module documentation](self). Replaces a body set earlier.
    pub fn parameters(mut self, parameters: impl IntoIterator<Item = Parameter>) -> Self {
        let mut body_params: Vec<Parameter> = Vec::new();
        for parameter in parameters {
            if parameter.is_body() {
                body_params.push(parameter);
            } else if let Parameter::Simple { name, value, .. } = parameter {
                self = self.query(&name, &value);
            }
        }
        self.body = body_from_parameters(&self.headers, body_params);
        self
    }
}

fn body_from_parameters(headers: &HeaderMap, mut parameters: Vec<Parameter>) -> Body {
    if parameters.is_empty() {
        return Body::Empty;
    }
    if parameters.len() == 1 {
        return match parameters.pop().expect("one element") {
            Parameter::Simple { name, value, .. } => Body::Raw {
                data: Bytes::from(encode_form([(name.as_str(), value.as_str())])),
                // an explicit Content-Type wins, like for Request::body
                content_type: (!headers.contains_key(CONTENT_TYPE))
                    .then_some(mime::APPLICATION_WWW_FORM_URLENCODED),
            },
            Parameter::Complex {
                content_type,
                content,
                ..
            } => Body::Raw {
                data: content,
                content_type: Some(content_type),
            },
        };
    }
    if parameters
        .iter()
        .all(|p| matches!(p, Parameter::Simple { .. }))
    {
        let pairs = parameters.iter().filter_map(|p| match p {
            Parameter::Simple { name, value, .. } => Some((name.as_str(), value.as_str())),
            Parameter::Complex { .. } => None,
        });
        return Body::Form(encode_form(pairs));
    }
    let mut multipart = Multipart::new();
    for parameter in parameters {
        multipart = match parameter {
            // field parts are not percent-encoded
            Parameter::Simple { name, value, .. } => multipart.text(name, value),
            Parameter::Complex {
                name,
                content_type,
                content,
            } => multipart.bytes(name, content_type, content),
        };
    }
    Body::Multipart(multipart)
}

impl Response {
    /// Parses the body into parameters, see the [module documentation](self).
    ///
    /// A flat body is returned as one complex parameter, multipart and form
    /// bodies are split into one parameter per part or pair. Complex
    /// parameters share the response buffer.
    pub fn parameters(&self) -> Result<Vec<Parameter>> {
        let name = match self.headers.get("content-id") {
            Some(id) => id.to_str()?.trim().replace('"', ""),
            None => "result".to_owned(),
        };
        let content_type = parse_content_type(&self.headers)?;
        parse_part(name, content_type, self.body.clone())
    }
}

/// Parses one body, recursing into multipart parts.
fn parse_part(name: String, content_type: Option<Mime>, body: Bytes) -> Result<Vec<Parameter>> {
    let content_type = content_type.unwrap_or(mime::APPLICATION_OCTET_STREAM);
    if content_type.type_() == mime::MULTIPART {
        let boundary = content_type.get_param(mime::BOUNDARY).ok_or_else(|| {
            Error::HeaderParse("multipart Content-Type without boundary".to_owned())
        })?;
        let mut parameters = Vec::new();
        for part in parse_multipart(&body, boundary.as_str())? {
            let name = if part.name.is_empty() {
                "result".to_owned()
            } else {
                part.name
            };
            parameters.extend(parse_part(name, part.content_type, part.body)?);
        }
        Ok(parameters)
    } else if content_type.essence_str() == mime::APPLICATION_WWW_FORM_URLENCODED {
        Ok(form_urlencoded::parse(&body)
            .map(|(name, value)| Parameter::body(name, value))
            .collect())
    } else {
        Ok(vec![Parameter::Complex {
            name,
            content_type,
            content: body,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post;
    use http::StatusCode;
    use http::header::HeaderValue;
    use std::fs;
    use std::str::FromStr;

    fn fixture(name: &str) -> Response {
        let headers = fs::read_to_string(format!("tests/fixtures/{name}.headers")).unwrap();
        let mut map = HeaderMap::new();
        for line in headers.lines() {
            let (name, value) = line.split_once(':').unwrap();
            map.insert(
                http::HeaderName::from_str(name.trim()).unwrap(),
                HeaderValue::from_str(value.trim()).unwrap(),
            );
        }
        Response {
            status: StatusCode::OK,
            headers: map,
            body: fs::read(format!("tests/fixtures/{name}.body"))
                .unwrap()
                .into(),
        }
    }

    #[test]
    fn query_parameters_go_into_the_url() {
        let req = post("http://localhost/?x=1").parameters([
            Parameter::query("b", "b 1"),
            Parameter::query("c", ""),
            Parameter::body("a", "a1"),
        ]);
        assert_eq!(req.url(), "http://localhost/?x=1&b=b+1&c");
    }

    #[test]
    fn single_simple_parameter_is_a_form_body() {
        let req = post("http://localhost")
            .parameters([Parameter::body("simple_param_ test", "simple_value")]);
        match req.body {
            Body::Raw { data, content_type } => {
                assert_eq!(content_type, Some(mime::APPLICATION_WWW_FORM_URLENCODED));
                assert_eq!(data, "simple_param_+test=simple_value");
            }
            other => panic!("expected raw body, got {other:?}"),
        }
    }

    #[test]
    fn single_complex_parameter_is_sent_raw() {
        let xml: Bytes = fs::read("tests/fixtures/file_example.xml").unwrap().into();
        let req = post("http://localhost").parameters([Parameter::complex(
            "f",
            mime::TEXT_XML,
            xml.clone(),
        )]);
        match req.body {
            Body::Raw { data, content_type } => {
                assert_eq!(content_type, Some(mime::TEXT_XML));
                assert_eq!(data, xml);
            }
            other => panic!("expected raw body, got {other:?}"),
        }
    }

    #[test]
    fn several_simple_parameters_are_a_form() {
        let req = post("http://localhost").parameters([
            Parameter::body("a", "b"),
            Parameter::body("c", "d"),
            Parameter::query("ignored", "1"),
        ]);
        assert!(matches!(req.body, Body::Form(ref s) if s == "a=b&c=d"));
    }

    #[test]
    fn complex_parameter_forces_multipart() {
        let jpg: Bytes = fs::read("tests/fixtures/16x16.jpg").unwrap().into();
        let req = post("http://localhost").parameters([
            Parameter::complex("test_jpg", mime::IMAGE_JPEG, jpg.clone()),
            Parameter::body("test_simple", "test_value"),
        ]);
        let Body::Multipart(multipart) = req.body else {
            panic!("expected multipart");
        };
        assert_eq!(multipart.len(), 2);
        assert_eq!(multipart.parts[0].name, "test_jpg");
        assert_eq!(multipart.parts[0].content_type, Some(mime::IMAGE_JPEG));
        assert_eq!(multipart.parts[0].data, jpg);
        assert_eq!(multipart.parts[1].name, "test_simple");
        assert_eq!(multipart.parts[1].content_type, None);
        assert_eq!(multipart.parts[1].data, "test_value");
    }

    #[test]
    fn parses_form_response() {
        let params = fixture("simple_singular").parameters().unwrap();
        assert_eq!(
            params,
            vec![Parameter::body("simple_param_ test", "simple_value")]
        );
    }

    #[test]
    fn parses_flat_text_response() {
        let response = fixture("text_file_singular");
        let params = response.parameters().unwrap();
        let [
            Parameter::Complex {
                name,
                content_type,
                content,
            },
        ] = params.as_slice()
        else {
            panic!("expected one complex parameter, got {params:?}");
        };
        assert_eq!(name, "result");
        assert_eq!(content_type.get_param("charset").unwrap(), "utf-8");
        assert_eq!(content, response.body());
    }

    #[test]
    fn parses_flat_binary_response_named_by_content_id() {
        let response = fixture("jpg_file_singular");
        let params = response.parameters().unwrap();
        let [
            Parameter::Complex {
                name,
                content_type,
                content,
            },
        ] = params.as_slice()
        else {
            panic!("expected one complex parameter, got {params:?}");
        };
        assert_eq!(name, "moon.jpg");
        assert_eq!(*content_type, mime::APPLICATION_OCTET_STREAM);
        assert_eq!(content, response.body());
    }

    #[test]
    fn parses_text_multipart_response() {
        let params = fixture("text_multipart").parameters().unwrap();
        assert_eq!(params.len(), 3);
        for (i, param) in params.iter().enumerate() {
            let Parameter::Complex {
                name,
                content_type,
                content,
            } = param
            else {
                panic!("expected complex parameter, got {param:?}");
            };
            assert_eq!(name, &format!("simple_param_{i}test"));
            assert_eq!(*content_type, mime::TEXT_PLAIN_UTF_8);
            assert_eq!(content, &format!("simple_value{i}"));
        }
    }

    #[test]
    fn parses_mixed_multipart_response() {
        let params = fixture("mixed_multipart").parameters().unwrap();
        let expected = [
            (
                "test_jpg",
                "image/jpeg",
                fs::read("tests/fixtures/16x16.jpg").unwrap(),
            ),
            (
                "test_xml",
                "text/xml",
                fs::read("tests/fixtures/file_example.xml").unwrap(),
            ),
            (
                "test_simple",
                "text/plain; charset=utf-8",
                b"test_value".to_vec(),
            ),
        ];
        assert_eq!(params.len(), expected.len());
        for (param, (name, content_type, content)) in params.iter().zip(expected) {
            let Parameter::Complex {
                name: n,
                content_type: ct,
                content: c,
            } = param
            else {
                panic!("expected complex parameter, got {param:?}");
            };
            assert_eq!(n, name);
            assert_eq!(ct.to_string(), content_type);
            assert_eq!(c, &content);
        }
    }
}
