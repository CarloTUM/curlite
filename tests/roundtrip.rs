//! End-to-end tests against a minimal HTTP/1.1 server on a loopback port.

use curlite::params::Parameter;
use curlite::{Multipart, StatusCode};

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

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
fn parameters_end_to_end() {
    let (url, server) = serve(vec![response(
        "200 OK",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        b"answer=42&more=a+b",
    )]);
    let resp = curlite::put(&format!("{url}/svc"))
        .parameters([
            Parameter::query("debug", "1"),
            Parameter::body("x", "1"),
            Parameter::complex("doc", mime::APPLICATION_JSON, &b"{}"[..]),
        ])
        .send()
        .unwrap();
    let params = resp.parameters().unwrap();
    assert_eq!(
        params,
        vec![
            Parameter::body("answer", "42"),
            Parameter::body("more", "a b")
        ]
    );

    let received = server.join().unwrap().remove(0);
    assert_eq!(received.request_line, "PUT /svc?debug=1 HTTP/1.1");
    assert!(
        received
            .header("content-type")
            .unwrap()
            .starts_with("multipart/form-data")
    );
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
