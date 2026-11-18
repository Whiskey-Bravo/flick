# Build from source

Building Flick yourself.

[Back to the overview](../README.md)

```
npm install
npm run tauri dev
npm run tauri build
```

The build makes an NSIS installer at `src-tauri/target/release/bundle/nsis/Flick_<version>_x64-setup.exe`.

Tests:

```
cargo test --manifest-path src-tauri/Cargo.toml
npx vitest run
```

Do not run a bare `cargo update`: `Cargo.lock` pins `notify-types` to a version that still builds with Rust 1.90.
