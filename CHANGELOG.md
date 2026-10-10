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

Added:

- `Request::send_to(&mut writer)` streams the response body.
- `Request::body_reader(mime, reader)` streams a request body from a
  `Read + Seek`.
- `Request::connect_timeout`.
- `Client`: headers, timeouts and redirect settings shared by requests.
- Re-export of `http::header`.

Changed:

- `multipart/form-data` requests are encoded by this crate and streamed part
  by part; part contents are no longer copied (libcurl's form API copied
  twice). `Response::parts` returns slices of the response buffer instead of
  copies. The `multipart_ng` dependency is gone.
- The response buffer is sized from `Content-Length` before the first chunk.
- A request sent from inside a `send_to` writer or `body_reader` reader, while
  the thread's handle is busy, runs on a throwaway handle instead of panicking.
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
