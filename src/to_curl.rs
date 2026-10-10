//! [`Request::to_curl`]: the request as a `curl` command line.

use http::Method;
use http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, PROXY_AUTHORIZATION};

use std::fmt::Write as _;

use crate::body::Body;
use crate::request::{Auth, HttpVersion, Request};

/// Bodies and fields up to this size are quoted inline; larger ones are
/// read from standard input in the printed command.
const INLINE_LIMIT: usize = 64 * 1024;

impl Request {
    /// The request as a `curl` command line, for logs, bug reports and
    /// trying things by hand.
    ///
    /// Everything that reaches the wire is in it: method, URL, headers, the
    /// body, redirect handling, HTTP version, proxy, Unix socket, CA bundle,
    /// certificate verification and compression. Timeouts are not; they do
    /// not change the request. Text bodies and fields up to 64 KiB are
    /// quoted inline, binary or larger ones and streamed bodies become `@-`
    /// (standard input) with a note in a trailing comment. Header names are
    /// lowercase, as [`http`] stores them. Values are single-quoted for a
    /// POSIX shell, one option per line.
    ///
    /// Credentials are not printed. Passwords, bearer tokens, credentials
    /// in the URL and the values of `Authorization`, `Proxy-Authorization`
    /// and `Cookie` headers appear as shell variables (`-u "user:$PASSWORD"`,
    /// `--oauth2-bearer "$TOKEN"`, `-H "cookie: $COOKIE"`), so the output
    /// runs once the variable is exported. Anything else, including a key in
    /// the query string or a header of your own, is printed as it is.
    ///
    /// ```
    /// let request = curlite::post("https://example.com/api?v=1")
    ///     .basic_auth("alice", "secret")
    ///     .form([("name", "a b")]);
    /// assert_eq!(
    ///     request.to_curl(),
    ///     "curl 'https://example.com/api?v=1' \\\n  \
    ///        -u \"alice:$PASSWORD\" \\\n  \
    ///        --data 'name=a+b' \\\n  \
    ///        -L --max-redirs 10 \\\n  \
    ///        --compressed"
    /// );
    /// ```
    pub fn to_curl(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let has_body = !matches!(self.body, Body::Empty);
        match &self.method {
            &Method::HEAD => lines.push("-I".to_owned()),
            &Method::GET if !has_body => {}
            // -d and -F imply POST
            &Method::POST if has_body => {}
            method => lines.push(format!("-X {method}")),
        }
        // credentials in the URL become -u, like the others
        let (url, user) = split_userinfo(&self.url);
        if let Some(user) = user {
            lines.push(user_option("", user));
        }
        let has_authorization = self.headers.contains_key(AUTHORIZATION);
        match &self.auth {
            None => {}
            Some(Auth::Bearer(_)) if has_authorization => {}
            Some(Auth::Bearer(_)) => lines.push("--oauth2-bearer \"$TOKEN\"".to_owned()),
            Some(Auth::Negotiate) => lines.push("--negotiate -u :".to_owned()),
            Some(Auth::Basic { user, .. }) => lines.push(user_option("", user)),
            Some(Auth::Digest { user, .. }) => lines.push(user_option("--digest ", user)),
            Some(Auth::Ntlm { user, .. }) => lines.push(user_option("--ntlm ", user)),
        }
        for (name, value) in &self.headers {
            let value = String::from_utf8_lossy(value.as_bytes());
            let value = value.trim();
            let line = if name == AUTHORIZATION || name == PROXY_AUTHORIZATION || name == COOKIE {
                let variable = name.as_str().to_ascii_uppercase().replace('-', "_");
                format!("-H \"{name}: ${variable}\"")
            } else if value.is_empty() {
                // "Name;" is curl's syntax for a header with an empty value
                format!("-H {}", quote(&format!("{name};")))
            } else {
                format!("-H {}", quote(&format!("{name}: {value}")))
            };
            lines.push(line);
        }
        let content_type = match &self.body {
            Body::Raw { content_type, .. } => content_type.as_ref(),
            Body::Reader { content_type, .. } => Some(content_type),
            // curl sets these itself for -d and -F
            Body::Empty | Body::Form(_) | Body::Multipart(_) => None,
        };
        if let Some(content_type) = content_type
            && !self.headers.contains_key(CONTENT_TYPE)
        {
            lines.push(format!(
                "-H {}",
                quote(&format!("Content-Type: {content_type}"))
            ));
        }
        match &self.body {
            Body::Empty => {}
            Body::Raw { data, .. } => match inline(data) {
                Some(text) => lines.push(format!("--data-binary {}", quote(text))),
                None => {
                    lines.push("--data-binary @-".to_owned());
                    notes.push(format!("body: {} bytes on standard input", data.len()));
                }
            },
            Body::Form(encoded) => lines.push(format!("--data {}", quote(encoded))),
            Body::Multipart(multipart) => {
                for part in &multipart.parts {
                    let inline_text = (part.content_type.is_none() && part.filename.is_none())
                        .then(|| inline(&part.data))
                        .flatten();
                    match inline_text {
                        // --form-string takes the value literally, -F would
                        // read `@file` and `<file`
                        Some(text) => lines.push(format!(
                            "--form-string {}",
                            quote(&format!("{}={text}", part.name))
                        )),
                        None => {
                            let mut spec = format!("{}=@-", part.name);
                            if let Some(filename) = &part.filename {
                                let _ = write!(spec, ";filename={filename}");
                            }
                            if let Some(content_type) = &part.content_type {
                                let _ = write!(spec, ";type={content_type}");
                            }
                            lines.push(format!("-F {}", quote(&spec)));
                            notes.push(format!(
                                "{}: {} bytes on standard input",
                                part.name,
                                part.data.len()
                            ));
                        }
                    }
                }
            }
            Body::Reader { .. } => {
                lines.push("--data-binary @-".to_owned());
                notes.push("body: streamed, on standard input".to_owned());
            }
        }
        if self.netrc {
            lines.push("--netrc-optional".to_owned());
        }
        match &self.proxy_auth {
            None => {}
            Some(Auth::Bearer(_)) => {
                lines.push("--proxy-header \"Proxy-Authorization: Bearer $PROXY_TOKEN\"".to_owned())
            }
            Some(Auth::Negotiate) => lines.push("--proxy-negotiate --proxy-user :".to_owned()),
            Some(Auth::Basic { user, .. }) => lines.push(proxy_user_option("", user)),
            Some(Auth::Digest { user, .. }) => {
                lines.push(proxy_user_option("--proxy-digest ", user))
            }
            Some(Auth::Ntlm { user, .. }) => lines.push(proxy_user_option("--proxy-ntlm ", user)),
        }
        if self.follow_redirects {
            lines.push(format!("-L --max-redirs {}", self.max_redirects));
        }
        if let Some(version) = self.http_version {
            lines.push(
                match version {
                    HttpVersion::Http11 => "--http1.1",
                    HttpVersion::Http2 => "--http2",
                    HttpVersion::Http2PriorKnowledge => "--http2-prior-knowledge",
                    HttpVersion::Http3 => "--http3",
                }
                .to_owned(),
            );
        }
        if let Some(proxy) = &self.proxy {
            lines.push(format!("--proxy {}", quote(proxy)));
        }
        if let Some(path) = &self.unix_socket {
            lines.push(format!("--unix-socket {}", quote(&path.to_string_lossy())));
        }
        if let Some(path) = &self.ca_bundle {
            lines.push(format!("--cacert {}", quote(&path.to_string_lossy())));
        }
        if let Some((cert, key)) = &self.client_cert {
            lines.push(format!(
                "--cert {} --key {}",
                quote(&cert.to_string_lossy()),
                quote(&key.to_string_lossy())
            ));
        }
        if !self.verify_tls {
            lines.push("-k".to_owned());
        }
        if self.compression {
            lines.push("--compressed".to_owned());
        }

        let mut out = format!("curl {}", quote(&url));
        for line in &lines {
            out.push_str(" \\\n  ");
            out.push_str(line);
        }
        if !notes.is_empty() {
            out.push_str("  # ");
            out.push_str(&notes.join("; "));
        }
        out
    }
}

/// `data` as text, if it is UTF-8 without control characters (line breaks
/// and tabs allowed) and small enough to put on a command line.
fn inline(data: &[u8]) -> Option<&str> {
    if data.len() > INLINE_LIMIT {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    text.chars()
        .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .then_some(text)
}

/// Single-quoted for a POSIX shell; a `'` inside becomes `'\''`.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The URL without its `user:password@`, and the user name if there was one.
fn split_userinfo(url: &str) -> (String, Option<&str>) {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return (url.to_owned(), None);
    };
    let end = url[start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| start + i);
    let Some(at) = url[start..end].rfind('@').map(|i| start + i) else {
        return (url.to_owned(), None);
    };
    let user = url[start..at].split(':').next().unwrap_or("");
    (format!("{}{}", &url[..start], &url[at + 1..]), Some(user))
}

/// `-u "user:$PASSWORD"`, the user escaped for double quotes.
fn user_option(prefix: &str, user: &str) -> String {
    format!("{prefix}-u \"{}:$PASSWORD\"", escape_double(user))
}

/// `--proxy-user "user:$PROXY_PASSWORD"`.
fn proxy_user_option(prefix: &str, user: &str) -> String {
    format!(
        "{prefix}--proxy-user \"{}:$PROXY_PASSWORD\"",
        escape_double(user)
    )
}

fn escape_double(user: &str) -> String {
    let mut escaped = String::with_capacity(user.len());
    for c in user.chars() {
        if matches!(c, '"' | '$' | '`' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use crate::{Auth, HttpVersion, Multipart, delete, get, head, post, put};
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn get_with_defaults() {
        let request = get("https://example.com/a?b=1").query("c", "d e");
        assert_eq!(
            request.to_curl(),
            "curl 'https://example.com/a?b=1&c=d+e' \\\n  -L --max-redirs 10 \\\n  --compressed"
        );
    }

    #[test]
    fn methods() {
        assert!(head("http://h").to_curl().contains("\n  -I \\\n"));
        assert!(delete("http://h").to_curl().contains("\n  -X DELETE \\\n"));
        let put = put("http://h").body(mime::TEXT_PLAIN, "x").to_curl();
        assert!(put.contains("\n  -X PUT \\\n"), "{put}");
        // -d implies POST, GET with a body has to say so
        let post = post("http://h").form([("a", "b")]).to_curl();
        assert!(!post.contains("-X"), "{post}");
        let get = get("http://h").body(mime::TEXT_PLAIN, "x").to_curl();
        assert!(get.contains("-X GET"), "{get}");
    }

    #[test]
    fn headers_and_quoting() {
        let request = get("http://h")
            .header("X-Empty", "")
            .header("X-Quote", "it's")
            .follow_redirects(false)
            .compression(false);
        assert_eq!(
            request.to_curl(),
            "curl 'http://h' \\\n  -H 'x-empty;' \\\n  -H 'x-quote: it'\\''s'"
        );
    }

    #[test]
    fn raw_body_text_and_binary() {
        let text = post("http://h")
            .body(mime::APPLICATION_JSON, r#"{"a":"b'c"}"#)
            .follow_redirects(false)
            .compression(false)
            .to_curl();
        assert_eq!(
            text,
            "curl 'http://h' \\\n  -H 'Content-Type: application/json' \\\n  --data-binary '{\"a\":\"b'\\''c\"}'"
        );
        // an explicit Content-Type header is not repeated
        let explicit = post("http://h")
            .header("Content-Type", "text/x-custom")
            .body(mime::APPLICATION_JSON, "{}")
            .to_curl();
        assert_eq!(explicit.to_lowercase().matches("content-type").count(), 1);
        assert!(explicit.contains("text/x-custom"));

        let binary = post("http://h")
            .body(mime::APPLICATION_OCTET_STREAM, &b"\x00\x01\x02"[..])
            .follow_redirects(false)
            .compression(false)
            .to_curl();
        assert_eq!(
            binary,
            "curl 'http://h' \\\n  -H 'Content-Type: application/octet-stream' \\\n  --data-binary @-  # body: 3 bytes on standard input"
        );
        let large = post("http://h")
            .body(mime::TEXT_PLAIN, vec![b'a'; 64 * 1024 + 1])
            .to_curl();
        assert!(large.contains("--data-binary @- \\\n"), "{large}");
        assert!(large.ends_with("# body: 65537 bytes on standard input"));
    }

    #[test]
    fn multipart_and_streamed_bodies() {
        let request = post("http://h")
            .multipart(
                Multipart::new()
                    .text("note", "@not a file")
                    .file("doc", "q3.pdf", mime::APPLICATION_PDF, &b"%PDF"[..])
                    .bytes("raw", mime::APPLICATION_OCTET_STREAM, &b"\xff"[..]),
            )
            .follow_redirects(false)
            .compression(false);
        assert_eq!(
            request.to_curl(),
            "curl 'http://h' \\\n  \
               --form-string 'note=@not a file' \\\n  \
               -F 'doc=@-;filename=q3.pdf;type=application/pdf' \\\n  \
               -F 'raw=@-;type=application/octet-stream'  \
               # doc: 4 bytes on standard input; raw: 1 bytes on standard input"
        );
        let streamed = put("http://h")
            .body_reader(mime::TEXT_PLAIN, Cursor::new(b"x".to_vec()))
            .follow_redirects(false)
            .compression(false)
            .to_curl();
        assert_eq!(
            streamed,
            "curl 'http://h' \\\n  -X PUT \\\n  -H 'Content-Type: text/plain' \\\n  --data-binary @-  # body: streamed, on standard input"
        );
    }

    #[test]
    fn secrets_become_variables() {
        let base = || get("http://h").follow_redirects(false).compression(false);
        assert_eq!(
            base().basic_auth("al\"i$ce", "pw").to_curl(),
            "curl 'http://h' \\\n  -u \"al\\\"i\\$ce:$PASSWORD\""
        );
        assert_eq!(
            base()
                .auth(Auth::Digest {
                    user: "u".into(),
                    password: "p".into()
                })
                .to_curl(),
            "curl 'http://h' \\\n  --digest -u \"u:$PASSWORD\""
        );
        assert_eq!(
            base()
                .auth(Auth::Ntlm {
                    user: "u".into(),
                    password: "p".into()
                })
                .netrc()
                .to_curl(),
            "curl 'http://h' \\\n  --ntlm -u \"u:$PASSWORD\" \\\n  --netrc-optional"
        );
        assert_eq!(
            base().auth(Auth::Negotiate).to_curl(),
            "curl 'http://h' \\\n  --negotiate -u :"
        );
        assert_eq!(
            base()
                .proxy("http://proxy:3128")
                .proxy_auth(Auth::Ntlm {
                    user: "dom\\\\u".into(),
                    password: "p".into()
                })
                .to_curl(),
            "curl 'http://h' \\\n  --proxy-ntlm --proxy-user \"dom\\\\\\\\u:$PROXY_PASSWORD\" \\\n  --proxy 'http://proxy:3128'"
        );
        assert_eq!(
            base().bearer_auth("t").to_curl(),
            "curl 'http://h' \\\n  --oauth2-bearer \"$TOKEN\""
        );
        assert_eq!(
            base().header("Cookie", "session=abc").to_curl(),
            "curl 'http://h' \\\n  -H \"cookie: $COOKIE\""
        );
        assert_eq!(
            get("http://al:pw@h/p?k=v")
                .follow_redirects(false)
                .compression(false)
                .to_curl(),
            "curl 'http://h/p?k=v' \\\n  -u \"al:$PASSWORD\""
        );
        // an explicit header wins over the bearer, and is itself hidden
        let explicit = base()
            .bearer_auth("t")
            .header("Authorization", "Basic abc")
            .header("Proxy-Authorization", "Basic xyz")
            .to_curl();
        assert_eq!(
            explicit,
            "curl 'http://h' \\\n  -H \"authorization: $AUTHORIZATION\" \\\n  -H \"proxy-authorization: $PROXY_AUTHORIZATION\""
        );
        assert!(!explicit.contains("abc") && !explicit.contains("xyz"));
    }

    #[test]
    fn connection_options_but_not_timeouts() {
        let request = get("http://h")
            .http_version(HttpVersion::Http2PriorKnowledge)
            .proxy("socks5h://localhost:1080")
            .unix_socket("/run/docker.sock")
            .ca_bundle("/etc/ca.pem")
            .client_cert("/c.pem", "/k.pem")
            .danger_accept_invalid_certs()
            .max_redirects(3)
            .timeout(Duration::from_secs(5))
            .stall_timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(5));
        assert_eq!(
            request.to_curl(),
            "curl 'http://h' \\\n  \
               -L --max-redirs 3 \\\n  \
               --http2-prior-knowledge \\\n  \
               --proxy 'socks5h://localhost:1080' \\\n  \
               --unix-socket '/run/docker.sock' \\\n  \
               --cacert '/etc/ca.pem' \\\n  \
               --cert '/c.pem' --key '/k.pem' \\\n  \
               -k \\\n  \
               --compressed"
        );
    }
}
