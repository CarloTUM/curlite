//! TLS client authentication against `openssl s_server`, with certificates
//! made on the spot. Skipped when the `openssl` binary is missing.
//!
//! The certificates are X.509 v3: libcurl's rustls backend rejects a v1
//! certificate (what `openssl x509 -req` makes without extensions), OpenSSL
//! accepts it. Worth knowing when a hand-made client certificate fails only
//! on a rustls build.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn openssl(dir: &Path, args: &[&str]) {
    let status = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "openssl {args:?}");
}

#[test]
fn client_certificate_is_presented() {
    if Command::new("openssl").arg("version").output().is_err() {
        eprintln!("no openssl binary, skipped");
        return;
    }
    match curl::Version::get().ssl_version() {
        None => {
            eprintln!("libcurl without TLS, skipped");
            return;
        }
        // Schannel takes certificates from the Windows store or a PKCS#12
        // file, not from PEM files
        Some(ssl) if ssl.starts_with("Schannel") => {
            eprintln!("Schannel does not load PEM client certificates, skipped");
            return;
        }
        Some(_) => {}
    }
    let dir = std::env::temp_dir().join(format!("curlite-mtls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.as_path();
    {
        openssl(
            d,
            &[
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.pem",
                "-subj",
                "/CN=curlite test CA",
                "-days",
                "1",
            ],
        );
        openssl(
            d,
            &[
                "req",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
            ],
        );
        openssl(
            d,
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "server.pem",
                "-days",
                "1",
                "-copy_extensions",
                "copy",
            ],
        );
        openssl(
            d,
            &[
                "req",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-keyout",
                "client.key",
                "-out",
                "client.csr",
                "-subj",
                "/CN=curlite client",
                "-addext",
                "basicConstraints=CA:FALSE",
            ],
        );
        // with an extension the certificate is X.509 v3; rustls rejects v1
        openssl(
            d,
            &[
                "x509",
                "-req",
                "-in",
                "client.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "client.pem",
                "-days",
                "1",
                "-copy_extensions",
                "copy",
            ],
        );
    }

    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Command::new("openssl")
        .args([
            "s_server",
            "-accept",
            &port.to_string(),
            "-www",
            "-cert",
            "server.pem",
        ])
        .args([
            "-key",
            "server.key",
            "-CAfile",
            "ca.pem",
            "-Verify",
            "1",
            "-quiet",
        ])
        .current_dir(d)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "s_server did not come up"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let url = format!("https://localhost:{port}/");
    let response = curlite::get(&url)
        .ca_bundle(d.join("ca.pem"))
        .client_cert(d.join("client.pem"), d.join("client.key"))
        .send();
    let without = curlite::get(&url).ca_bundle(d.join("ca.pem")).send();
    server.kill().unwrap();
    server.wait().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();

    let response = response.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.text().unwrap().contains("curlite client"),
        "{}",
        response.text().unwrap()
    );
    let err = without.unwrap_err();
    assert!(matches!(err, curlite::Error::Curl(_)), "{err:?}");
}
