//! End-to-end tests against a minimal HTTP/1.1 server on a loopback port.

use curlite::{Multipart, StatusCode};

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// What the server saw.
#[derive(Debug)]
struct Received {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Serves `responses` in order, one connection per response, and returns what
/// was received. `responses` are complete HTTP responses without framing
/// help: they must carry a `Content-Length` or `Connection: close`.
fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<Received>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut received = Vec::new();
        let mut stream: Option<TcpStream> = None;
        for response in responses {
            // reuse the connection as long as the client keeps it open
            let mut conn = match stream.take() {
                Some(conn) => conn,
                None => listener.accept().unwrap().0,
            };
            let request = match read_request(&mut conn) {
                Some(request) => request,
                None => {
                    conn = listener.accept().unwrap().0;
                    read_request(&mut conn).unwrap()
                }
            };
            received.push(request);
            conn.write_all(response.as_bytes()).unwrap();
            conn.flush().unwrap();
            stream = Some(conn);
        }
        received
    });
    (url, handle)
}

fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    let length: usize = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, v)| v.parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Some(Received {
        request_line: request_line.trim_end().to_owned(),
        headers,
        body,
    })
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> String {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(std::str::from_utf8(body).unwrap());
    out
}

#[test]
fn get_with_query_and_headers() {
    let (url, server) = serve(vec![response(
        "200 OK",
        &[
            ("Content-Type", "text/plain; charset=utf-8"),
            ("X-Reply", "yes"),
        ],
        b"hello",
    )]);

    let resp = curlite::get(&format!("{url}/path?a=1"))
        .query("b", "two words")
        .header("X-Test", "value")
        .send()
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-reply"], "yes");
    assert_eq!(resp.text().unwrap(), "hello");
    assert_eq!(
        resp.content_type().unwrap().unwrap(),
        mime::TEXT_PLAIN_UTF_8
    );

    let received = server.join().unwrap().remove(0);
    assert_eq!(received.request_line, "GET /path?a=1&b=two+words HTTP/1.1");
    assert_eq!(received.header("x-test"), Some("value"));
    assert!(received.body.is_empty());
}

#[test]
fn head_has_no_body() {
    let (url, server) = serve(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n".to_owned(),
    ]);
    let resp = curlite::head(&url).send().unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.body().is_empty());
    assert_eq!(server.join().unwrap()[0].request_line, "HEAD / HTTP/1.1");
}

#[test]
fn raw_body_streams_once_with_content_type() {
    let (url, server) = serve(vec![response("201 Created", &[], b"")]);
    let payload = vec![b'x'; 300_000];
    let resp = curlite::put(&url)
        .body(mime::APPLICATION_OCTET_STREAM, payload.clone())
        .send()
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let received = server.join().unwrap().remove(0);
    assert_eq!(received.request_line, "PUT / HTTP/1.1");
    assert_eq!(
        received.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(received.header("content-length"), Some("300000"));
    assert_eq!(received.body, payload);
}

#[test]
fn bodyless_post_sends_content_length_zero() {
    let (url, server) = serve(vec![response("204 No Content", &[], b"")]);
    curlite::post(&url).send().unwrap();
    let received = server.join().unwrap().remove(0);
    assert_eq!(received.request_line, "POST / HTTP/1.1");
    assert_eq!(received.header("content-length"), Some("0"));
}

#[test]
fn form_body() {
    let (url, server) = serve(vec![response("200 OK", &[], b"")]);
    curlite::post(&url)
        .form([("name", "a b"), ("flag", "")])
        .send()
        .unwrap();
    let received = server.join().unwrap().remove(0);
    assert_eq!(
        received.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(received.body, b"name=a+b&flag");
}

#[test]
fn multipart_body_round_trips_through_the_parser() {
    let (url, server) = serve(vec![response("200 OK", &[], b"")]);
    curlite::post(&url)
        .multipart(Multipart::new().text("description", "a report").file(
            "file",
            "r.pdf",
            mime::APPLICATION_PDF,
            &b"%PDF-1.4 fake"[..],
        ))
        .send()
        .unwrap();
    let received = server.join().unwrap().remove(0);
    let content_type = received.header("content-type").unwrap();
    assert!(content_type.starts_with("multipart/form-data; boundary="));

    // feed what the server got back into the response parser
    let (status, headers, body) = (
        StatusCode::OK,
        [("Content-Type", content_type)],
        received.body.clone(),
    );
    let echoed = serve(vec![response(status.as_str(), &headers, &body)]);
    let parts = curlite::get(&echoed.0).send().unwrap().parts().unwrap();
    echoed.1.join().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].name, "description");
    assert_eq!(parts[0].content_type, None);
    assert_eq!(parts[0].body, "a report");
    assert_eq!(parts[1].name, "file");
    assert_eq!(parts[1].filename.as_deref(), Some("r.pdf"));
    assert_eq!(parts[1].content_type, Some(mime::APPLICATION_PDF));
    assert_eq!(parts[1].body, "%PDF-1.4 fake");
}

#[test]
fn redirects_are_followed_or_returned() {
    let (url, server) = serve(vec![
        response("302 Found", &[("Location", "/other")], b""),
        response("200 OK", &[], b"landed"),
    ]);
    let resp = curlite::get(&url).send().unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().unwrap(), "landed");
    let received = server.join().unwrap();
    assert_eq!(received[1].request_line, "GET /other HTTP/1.1");

    let (url, server) = serve(vec![response("302 Found", &[("Location", "/other")], b"")]);
    let resp = curlite::get(&url).follow_redirects(false).send().unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/other");
    server.join().unwrap();
}

#[test]
fn error_status_is_not_an_error() {
    let (url, server) = serve(vec![response("404 Not Found", &[], b"nope")]);
    let resp = curlite::get(&url).send().unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(resp.text().unwrap(), "nope");
    server.join().unwrap();
}

#[test]
fn connection_is_reused_on_the_same_thread() {
    let (url, server) = serve(vec![
        response("200 OK", &[], b"1"),
        response("200 OK", &[], b"2"),
    ]);
    assert_eq!(curlite::get(&url).send().unwrap().text().unwrap(), "1");
    assert_eq!(curlite::get(&url).send().unwrap().text().unwrap(), "2");
    // the server only accepts a second connection if the first one was closed
    server.join().unwrap();
}

#[test]
fn invalid_response_header_is_an_error() {
    let (url, server) = serve(vec![
        "HTTP/1.1 200 OK\r\nBad Header Name: x\r\nContent-Length: 0\r\n\r\n".to_owned(),
    ]);
    let err = curlite::get(&url).send().unwrap_err();
    assert!(
        matches!(err, curlite::Error::InvalidHeaderName(_)),
        "{err:?}"
    );
    server.join().unwrap();
}

#[test]
fn redirect_303_turns_post_into_get_without_body() {
    let (url, server) = serve(vec![
        response("303 See Other", &[("Location", "/done")], b""),
        response("200 OK", &[], b"ok"),
    ]);
    let resp = curlite::post(&url).form([("a", "1")]).send().unwrap();
    assert_eq!(resp.text().unwrap(), "ok");
    let received = server.join().unwrap();
    assert_eq!(received[0].request_line, "POST / HTTP/1.1");
    assert_eq!(received[0].body, b"a=1");
    assert_eq!(received[1].request_line, "GET /done HTTP/1.1");
    assert_eq!(received[1].header("content-type"), None);
    assert_eq!(received[1].header("content-length"), None);
    assert!(received[1].body.is_empty());
}

#[test]
fn redirect_303_turns_delete_into_get() {
    let (url, server) = serve(vec![
        response("303 See Other", &[("Location", "/gone")], b""),
        response("200 OK", &[], b""),
    ]);
    curlite::delete(&url).send().unwrap();
    let received = server.join().unwrap();
    assert_eq!(received[0].request_line, "DELETE / HTTP/1.1");
    assert_eq!(received[1].request_line, "GET /gone HTTP/1.1");
}

#[test]
fn redirect_307_resends_the_body_with_the_same_method() {
    let (url, server) = serve(vec![
        response("307 Temporary Redirect", &[("Location", "/v2")], b""),
        response("201 Created", &[], b""),
    ]);
    let payload = vec![b'y'; 100_000];
    curlite::put(&url)
        .body(mime::APPLICATION_OCTET_STREAM, payload.clone())
        .send()
        .unwrap();
    let received = server.join().unwrap();
    assert_eq!(received[0].request_line, "PUT / HTTP/1.1");
    assert_eq!(received[0].body, payload);
    assert_eq!(received[1].request_line, "PUT /v2 HTTP/1.1");
    assert_eq!(received[1].body, payload);
    assert_eq!(
        received[1].header("content-type"),
        Some("application/octet-stream")
    );
}

#[test]
fn too_many_redirects() {
    let (url, server) = serve(vec![
        response("302 Found", &[("Location", "/1")], b""),
        response("302 Found", &[("Location", "/2")], b""),
    ]);
    let err = curlite::get(&url).max_redirects(1).send().unwrap_err();
    assert!(
        matches!(err, curlite::Error::TooManyRedirects(1)),
        "{err:?}"
    );
    assert_eq!(server.join().unwrap().len(), 2);

    let (url, server) = serve(vec![response("302 Found", &[("Location", "/1")], b"")]);
    let err = curlite::get(&url).max_redirects(0).send().unwrap_err();
    assert!(
        matches!(err, curlite::Error::TooManyRedirects(0)),
        "{err:?}"
    );
    server.join().unwrap();
}

#[test]
fn send_to_streams_the_final_body_only() {
    let (url, server) = serve(vec![
        response("302 Found", &[("Location", "/file")], b"not this"),
        response("200 OK", &[], b"streamed"),
    ]);
    let mut out = Vec::new();
    let resp = curlite::get(&url).send_to(&mut out).unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.body().is_empty());
    assert_eq!(out, b"streamed");
    server.join().unwrap();

    // a redirect that is not followed is the final response
    let (url, server) = serve(vec![response("302 Found", &[("Location", "/x")], b"here")]);
    let mut out = Vec::new();
    let resp = curlite::get(&url)
        .follow_redirects(false)
        .send_to(&mut out)
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert!(resp.body().is_empty());
    assert_eq!(out, b"here");
    server.join().unwrap();

    // so is a 3xx without Location
    let (url, server) = serve(vec![response("304 Not Modified", &[], b"")]);
    let resp = curlite::get(&url).send_to(&mut out).unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    server.join().unwrap();
}

#[test]
fn send_to_reports_writer_errors() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (url, server) = serve(vec![response("200 OK", &[], b"data")]);
    let err = curlite::get(&url).send_to(&mut Broken).unwrap_err();
    assert!(
        matches!(&err, curlite::Error::Io(e) if e.to_string() == "disk full"),
        "{err:?}"
    );
    server.join().unwrap();
}

#[test]
fn body_reader_streams_with_content_length() {
    let (url, server) = serve(vec![response("200 OK", &[], b"")]);
    let payload = vec![b'r'; 200_000];
    curlite::post(&url)
        .body_reader(mime::TEXT_PLAIN, std::io::Cursor::new(payload.clone()))
        .send()
        .unwrap();
    let received = server.join().unwrap().remove(0);
    assert_eq!(received.header("content-length"), Some("200000"));
    assert_eq!(received.header("content-type"), Some("text/plain"));
    assert_eq!(received.body, payload);
}

#[test]
fn default_user_agent_and_client_headers() {
    let (url, server) = serve(vec![
        response("200 OK", &[], b""),
        response("200 OK", &[], b""),
    ]);
    curlite::get(&url).send().unwrap();
    let client = curlite::Client::new().header(
        curlite::header::USER_AGENT,
        curlite::HeaderValue::from_static("mine/1"),
    );
    client.get(&url).send().unwrap();
    let received = server.join().unwrap();
    assert_eq!(
        received[0].header("user-agent"),
        Some(concat!("curlite/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(received[1].header("user-agent"), Some("mine/1"));
}

#[test]
fn nested_request_from_a_writer_does_not_panic() {
    struct Nested(String, Vec<u8>);
    impl Write for Nested {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
            let inner = curlite::get(&self.0)
                .send()
                .map_err(std::io::Error::other)?;
            self.1.extend_from_slice(inner.body());
            self.1.extend_from_slice(chunk);
            Ok(chunk.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (outer_url, outer) = serve(vec![response("200 OK", &[], b"outer")]);
    let (inner_url, inner) = serve(vec![response("200 OK", &[], b"inner")]);
    let mut sink = Nested(inner_url, Vec::new());
    curlite::get(&outer_url).send_to(&mut sink).unwrap();
    assert_eq!(sink.1, b"innerouter");
    outer.join().unwrap();
    inner.join().unwrap();
}

#[test]
fn send_reader_pulls_a_large_body_with_backpressure() {
    // well above the 128 KiB pause threshold, so the transfer pauses and
    // resumes several times while the reader drains it in small pieces
    let payload: Vec<u8> = (0..2_000_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let (url, server) = serve(vec![response(
        "200 OK",
        &[("Content-Type", "application/octet-stream")],
        &payload,
    )]);
    let mut reader = curlite::get(&url).send_reader().unwrap();
    assert_eq!(reader.status(), StatusCode::OK);
    assert_eq!(
        reader.content_type().unwrap().unwrap(),
        mime::APPLICATION_OCTET_STREAM
    );
    let mut out = Vec::new();
    let mut buf = [0u8; 7_000];
    loop {
        let n = reader.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out.len(), payload.len());
    assert!(out == payload);
    server.join().unwrap();
}

#[test]
fn send_reader_into_response_and_early_drop() {
    let (url, server) = serve(vec![response("200 OK", &[], b"abcdef")]);
    let mut reader = curlite::get(&url).send_reader().unwrap();
    let mut first = [0u8; 2];
    reader.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"ab");
    let resp = reader.into_response().unwrap();
    assert_eq!(resp.text().unwrap(), "cdef");
    server.join().unwrap();

    // dropping a reader mid-body cancels the transfer; the next request on
    // the thread still works
    let big = vec![b'z'; 1_000_000];
    let (url, server) = serve(vec![response("200 OK", &[], &big)]);
    let reader = curlite::get(&url).send_reader().unwrap();
    assert_eq!(reader.status(), StatusCode::OK);
    drop(reader);
    drop(server);
    let (url, server) = serve(vec![response("200 OK", &[], b"after")]);
    assert_eq!(curlite::get(&url).send().unwrap().text().unwrap(), "after");
    server.join().unwrap();
}

#[test]
fn send_to_large_body_streams_in_pieces() {
    let payload = vec![b'q'; 1_500_000];
    let (url, server) = serve(vec![response("200 OK", &[], &payload)]);
    struct Counting(usize, usize);
    impl Write for Counting {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
            self.0 += chunk.len();
            self.1 += 1;
            Ok(chunk.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Counting(0, 0);
    curlite::get(&url).send_to(&mut sink).unwrap();
    assert_eq!(sink.0, payload.len());
    assert!(sink.1 > 1, "expected several writes, got {}", sink.1);
    server.join().unwrap();
}

#[test]
fn error_for_status_keeps_the_response() {
    let (url, server) = serve(vec![
        response("200 OK", &[], b"fine"),
        response("503 Service Unavailable", &[], b"later"),
    ]);
    let ok = curlite::get(&url)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(ok.text().unwrap(), "fine");
    let err = curlite::get(&url)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap_err();
    assert_eq!(err.to_string(), "HTTP 503 Service Unavailable");
    let curlite::Error::Status(resp) = err else {
        panic!("{err:?}");
    };
    assert_eq!(resp.text().unwrap(), "later");
    server.join().unwrap();
}

#[test]
fn compression_is_advertised_unless_disabled() {
    let (url, server) = serve(vec![
        response("200 OK", &[], b""),
        response("200 OK", &[], b""),
    ]);
    curlite::get(&url).send().unwrap();
    curlite::get(&url).compression(false).send().unwrap();
    let received = server.join().unwrap();
    assert!(
        received[0]
            .header("accept-encoding")
            .is_some_and(|v| v.contains("gzip")),
        "{:?}",
        received[0].headers
    );
    assert_eq!(received[1].header("accept-encoding"), None);
}

#[cfg(unix)]
#[test]
fn unix_socket_requests() {
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("curlite-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("api.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&mut conn);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut host = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(h) = line.strip_prefix("Host: ") {
                host = Some(h.to_owned());
            }
        }
        conn.write_all(response("200 OK", &[], b"via socket").as_bytes())
            .unwrap();
        (request_line.trim_end().to_owned(), host)
    });
    let resp = curlite::get("http://docker/v1/info")
        .unix_socket(&path)
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "via socket");
    let (request_line, host) = server.join().unwrap();
    assert_eq!(request_line, "GET /v1/info HTTP/1.1");
    assert_eq!(host.as_deref(), Some("docker"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(feature = "json")]
#[test]
fn json_round_trip() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Item {
        id: u32,
        name: String,
    }
    let (url, server) = serve(vec![response(
        "200 OK",
        &[("Content-Type", "application/json")],
        br#"{"id":7,"name":"seven"}"#,
    )]);
    let item: Item = curlite::post(&url)
        .json(&Item {
            id: 1,
            name: "one".to_owned(),
        })
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(item.id, 7);
    assert_eq!(item.name, "seven");
    let received = server.join().unwrap().remove(0);
    assert_eq!(received.header("content-type"), Some("application/json"));
    assert_eq!(received.body, br#"{"id":1,"name":"one"}"#);
}

#[test]
fn two_transfers_on_one_thread_complete_independently() {
    // an open reader (paused, body > 128 KiB) must not stall or truncate a
    // second request on the same thread, and vice versa
    let big = vec![b'r'; 600_000];
    let (url_a, server_a) = serve(vec![response("200 OK", &[], &big)]);
    let (url_b, server_b) = serve(vec![response("200 OK", &[], b"other")]);
    let mut reader = curlite::get(&url_a).send_reader().unwrap();
    let started = std::time::Instant::now();
    let other = curlite::get(&url_b)
        .timeout(Duration::from_secs(3))
        .send()
        .unwrap();
    assert_eq!(other.text().unwrap(), "other");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let mut out = Vec::new();
    reader.read_to_end(&mut out).unwrap();
    assert_eq!(out.len(), big.len());
    server_a.join().unwrap();
    server_b.join().unwrap();

    // a short response finishing while another request runs keeps its own
    // result: the truncation below is still reported
    let partial = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: 400000\r\nConnection: close\r\n\r\n{}",
        "p".repeat(200_000)
    );
    let (url_a, server_a) = serve(vec![partial]);
    let (url_b, server_b) = serve(vec![response("200 OK", &[], b"x")]);
    let mut reader = curlite::get(&url_a).send_reader().unwrap();
    drop(server_a);
    thread::sleep(Duration::from_millis(100));
    curlite::get(&url_b).send().unwrap();
    server_b.join().unwrap();
    let mut out = Vec::new();
    let err = reader.read_to_end(&mut out).unwrap_err();
    assert!(err.to_string().contains("curl"), "{err}");
}

#[test]
fn large_redirect_body_is_discarded_in_stream_mode() {
    let interstitial = vec![b'<'; 300_000];
    let (url, server) = serve(vec![
        response("302 Found", &[("Location", "/x")], &interstitial),
        response("200 OK", &[], b"landed"),
    ]);
    let mut out = Vec::new();
    let resp = curlite::get(&url)
        .timeout(Duration::from_secs(3))
        .send_to(&mut out)
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(out, b"landed");
    server.join().unwrap();
}

#[test]
fn body_reader_may_send_requests_on_every_read() {
    struct Lazy {
        url: String,
        pos: u64,
        chunks: u64,
    }
    impl Read for Lazy {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.chunks {
                return Ok(0);
            }
            let r = curlite::get(&self.url)
                .send()
                .map_err(std::io::Error::other)?;
            let piece = r.body();
            buf[..piece.len()].copy_from_slice(piece);
            self.pos += 1;
            Ok(piece.len())
        }
    }
    impl std::io::Seek for Lazy {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            match pos {
                std::io::SeekFrom::End(0) => Ok(self.chunks * 5),
                std::io::SeekFrom::Start(0) => {
                    self.pos = 0;
                    Ok(0)
                }
                _ => Err(std::io::Error::other("unsupported")),
            }
        }
    }
    let (inner_url, inner) = serve(vec![response("200 OK", &[], b"piece"); 3]);
    let (url, server) = serve(vec![response("201 Created", &[], b"")]);
    curlite::put(&url)
        .body_reader(
            mime::TEXT_PLAIN,
            Lazy {
                url: inner_url,
                pos: 0,
                chunks: 3,
            },
        )
        .send()
        .unwrap();
    let received = server.join().unwrap().remove(0);
    assert_eq!(received.body, b"piecepiecepiece");
    assert_eq!(inner.join().unwrap().len(), 3);
}

#[test]
fn panic_in_body_reader_propagates_and_the_thread_recovers() {
    struct Boom;
    impl Read for Boom {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("reader exploded");
        }
    }
    impl std::io::Seek for Boom {
        fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
            Ok(100)
        }
    }
    let (url, server) = serve(vec![response("200 OK", &[], b"")]);
    let result = std::panic::catch_unwind(|| {
        curlite::put(&url)
            .body_reader(mime::TEXT_PLAIN, Boom)
            .send()
    });
    let payload = result.unwrap_err();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"reader exploded"));
    drop(server);
    let (url, server) = serve(vec![response("200 OK", &[], b"fine")]);
    assert_eq!(curlite::get(&url).send().unwrap().text().unwrap(), "fine");
    server.join().unwrap();
}

#[test]
fn streamed_chunks_arrive_without_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        read_request(&mut conn).unwrap();
        conn.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap();
        for i in 0..3 {
            thread::sleep(Duration::from_millis(150));
            conn.write_all(format!("6\r\nchunk{i}\r\n").as_bytes())
                .unwrap();
        }
        conn.write_all(b"0\r\n\r\n").unwrap();
    });
    let mut reader = curlite::get(&url).send_reader().unwrap();
    let mut buf = [0u8; 64];
    let t = std::time::Instant::now();
    let n = reader.read(&mut buf).unwrap();
    let elapsed = t.elapsed();
    assert_eq!(&buf[..n], b"chunk0");
    assert!(
        elapsed < Duration::from_millis(400),
        "first chunk took {elapsed:?}"
    );
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"chunk1chunk2");
    server.join().unwrap();
}

