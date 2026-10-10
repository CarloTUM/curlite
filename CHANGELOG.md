# Changelog

## 0.2.0

Breaking:

- `Request::url` returns `&str`. URLs are parsed by libcurl when the request
  is sent, a malformed one is an `Error::Curl` (`is_url_malformed()`). The
  `url` crate and its 40 transitive dependencies are gone.
- `Error`: `Url` and `CurlForm` are removed, `Multipart` carries a message
  instead of an `io::Error`; new variants `Io` and `TooManyRedirects`.
- Redirects are followed by `curlite`, not libcurl, with the verb handling of
  RFC 9110 (`303` to `GET` for every method, `301`/`302` to `GET` for `POST`,
  `307`/`308` resend the body). `Authorization` is dropped on redirects that
  leave the origin. Exceeding `max_redirects` is `Error::TooManyRedirects`.
- Every request sends `User-Agent: curlite/<version>` unless one is set.
- No default `timeout` any more. Transfers fail when no data moves for 30
  seconds (`stall_timeout`) or the connection takes more than 30 seconds
  (`connect_timeout`, previously libcurl's 300); `timeout` is opt-in. Both
  connect and stall timeouts are plain `Duration`s, zero disables.
- The `params` module and the `serde` feature are gone. The CPEE parameter
  model was specific to one service and is not part of a general HTTP
  client; `Response::parts` and `ResponsePart::parts` give its building
  blocks.

Added:

- `Request::send_to(&mut writer)` streams the response body to a writer,
  `Request::send_reader()` returns it as an `impl Read` with backpressure.
- `Request::body_reader(mime, reader)` streams a request body from a
  `Read + Seek`.
- `Request::connect_timeout`, `http_version`, `proxy`, `unix_socket`,
  `ca_bundle`, `danger_accept_invalid_certs`, `compression` (on by default:
  libcurl asks for and decodes compressed responses).
- `Response::error_for_status` and `Error::Status`.
- `Response::url` and `ResponseReader::url`: where the response came from,
  after redirects.
- `Request::to_curl`: the request as a `curl` command line. Secrets are
  printed as shell variables (`-u "user:$PASSWORD"`), never as values.
- `Request::auth` with `Auth::{Basic, Bearer, Digest, Negotiate, Ntlm}`,
  `basic_auth`, `bearer_auth`, and `netrc` for credentials from `~/.netrc`;
  all also on `Client`.
- `Request::client_cert` for TLS client certificates.
- `Request::on_progress`: byte counts in both directions, reported between
  libcurl's steps, never from inside a callback.
- `ResponsePart::parts` splits a nested multipart part.
- `Client`: headers, timeouts, redirect and connection settings shared by
  requests.
- Features `json` (`Request::json`, `Response::json`), `log` (libcurl's
  verbose output as `log::trace!`) and `http2` (nghttp2 for static builds).
- Feature `ssl` (default) carries the curl crate's OpenSSL backend; with
  `default-features = false` and `rustls` nothing OpenSSL is built.
- Re-export of `http::header`.

Changed:

- Transfers run on a per-thread libcurl multi handle, driven step by step;
  the easy handle is reused across requests. Needs libcurl 7.68 or newer.
- `multipart/form-data` requests are encoded by this crate and streamed part
  by part; part contents are no longer copied (libcurl's form API copied
  twice). `Response::parts` returns slices of the response buffer instead of
  copies. The `multipart_ng` dependency is gone.
- The response buffer is sized from `Content-Length` before the first chunk.
- On a libcurl built on OpenSSL with a hashed certificate directory
  (`/etc/ssl/certs/<hash>.0`), CA certificates are looked up by hash on
  demand instead of parsing the whole bundle per process, unless
  `ca_bundle` is set. About a third less CPU per HTTPS process on x86.
- Writers and readers run between libcurl's steps, never inside its
  callbacks, and may send requests of their own.
- Part names and filenames in multipart responses may be non-ASCII and may
  contain `;` inside quotes.
- CI runs on `master`.

## 0.1.1

- README and crate docs reworked; no code changes.

## 0.1.0

Initial release. `curlite` grew out of `http_helper`, the HTTP client of the
[WEEL](https://github.com/CarloTUM/rusty_weel) process engine.

Compared to `http_helper`:

- Request builder (`curlite::get(url).query(..).header(..).send()`) with
  explicit `body`, `form` and `multipart` bodies instead of `Client` +
  `add_parameter`.
- `http::Method` and `http::StatusCode` instead of crate-local types.
- `Response::parts` splits multipart responses without the parameter model.
- The parameter model lives in `curlite::params`; `Parameter::ComplexParameter`
  is now `Parameter::Complex { content_type, .. }`, `SimpleParameter` is
  `Simple`. `serde` support is behind the `serde` feature.
- Multipart parts can carry a filename.
- Form encoding uses `+` for spaces (`form_urlencoded`) instead of `%20`.
