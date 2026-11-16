# API — a SimpleEdit plugin

A light Postman in [SimpleEdit](https://github.com/simple-edit-app/simpleedit): requests in tabs (params, headers, JSON/text/form/multipart/file bodies, Bearer/Basic/API key, time limit, redirects, TLS), collections and folders, environments with `{{variables}}` (secret values in the keychain), the history, tests of the answers and values kept from them, the run of a whole collection, and imports and exports of Postman, OpenAPI 3 / Swagger 2 and cURL.

Its settings (Preferences › Plugins) say which certificates to trust and the proxy to go through.

## Build

```sh
cargo test
cargo build --release && mkdir -p bin && cp target/release/api bin/
```

## License

MIT
