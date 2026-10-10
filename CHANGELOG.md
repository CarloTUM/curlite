# Changelog

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
