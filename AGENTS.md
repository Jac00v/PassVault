# AI Contributor Guide

This file is the working guide for AI coding agents in this repository. Read it before making changes. For deeper context, see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/SECURITY.md`](docs/SECURITY.md).

## Project

Passvault is a Windows-first Tauri 2 desktop password manager. The Svelte frontend communicates with a Rust backend through Tauri commands. The backend encrypts a single local vault file; there is no account, sync service, or application server in the current implementation. Do not describe the project as independently audited.

## Source Map

- `src/App.svelte`: application state, user flows, Tauri IPC calls, and nearly all markup. This uses Svelte 5 with legacy reactive declarations (`$:`), not runes.
- `src/styles.css`: global styles, light/dark palettes, responsive layout, and the visual system.
- `src/main.ts`: Svelte mount and global stylesheet import.
- `src-tauri/src/lib.rs`: Rust app state, every Tauri command, vault format/crypto, persistence, CSV import, TOTP, password generation, backups, and Rust unit tests.
- `src-tauri/src/qr_scan.rs`: TOTP screen-scan subsystem: per-monitor capture, overlay window lifecycle, `rqrr` decoding, `otpauth` parsing and label sanitization, scan timeouts, and unit tests.
- `src-tauri/src/main.rs`: binary entry point.
- `qr.html`: page shell for the screen-scan overlay windows.
- `src/qr-overlay.ts`: mounts `QrOverlay.svelte` into `qr.html`.
- `src/QrOverlay.svelte`: frozen-frame canvas, drag/keyboard region selection, automatic submit on a successful decode, and cancellation UI. Uses scoped styles only and does not import `styles.css`.
- `src-tauri/tauri.conf.json`: window, CSP, frontend build commands, and installer targets.
- `src-tauri/capabilities/default.json`: permissions for the `main` window. Custom Rust command registration is in `lib.rs`, not this capability file. Overlay windows have no capability entry; see `docs/SECURITY.md` for what that does and does not restrict.
- `package.json`, `vite.config.ts`, `tsconfig.json`, `src-tauri/Cargo.toml`: toolchain and build configuration. `vite.config.ts` declares a multi-page build input (`index.html` and `qr.html`).

Keep changes localized to these existing files unless there is a clear need to introduce a new module or component. Avoid unnecessary architectural layers.

## Development And Checks

Run commands from the repository root using PowerShell:

```powershell
npm install
npm run tauri dev
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build
```

`npm run build` runs `svelte-check` and then Vite. There is no npm test script. Rust unit tests live in `src-tauri/src/lib.rs`. A Windows packaged build requires the prerequisites documented in `README.md` (Rust MSVC toolchain, Visual Studio C++ build tools/Windows SDK, Node/npm, and WebView2).

Run the checks relevant to the change. For changes to both frontend and backend, run both `npm run build` and `cargo test --manifest-path src-tauri/Cargo.toml` when the environment supports them. Be explicit about checks that could not be run.

## Implementation Rules

- Preserve the local-first architecture. Do not add network calls, telemetry, remote logging, cloud sync, or third-party services without an explicit product request and a security review.
- Treat vault contents, master passwords, TOTP secrets, CSV imports, clipboard values, and backups as sensitive. Never put real secrets in source, fixtures, logs, error messages, screenshots, or documentation examples.
- Any new backend operation that reads or mutates unlocked vault data must require unlocked state. Keep command arguments/results minimal and validate untrusted input at the Rust boundary, not only in the UI.
- When adding or changing Tauri commands, update the frontend call site/types and the `tauri::generate_handler!` registration. Review `tauri.conf.json` CSP and `capabilities/default.json`; grant only the permissions needed.
- Persist vault changes through the existing encrypted `VaultFile` path. Never write plaintext vault data to disk or backups. Changes to the persisted schema/crypto require compatibility tests and careful migration planning; do not silently change the version or KDF/cipher parameters.
- Keep import preview and commit behavior aligned. Duplicate detection is heuristic; do not claim exact matching. Preserve the existing merge behavior unless a product requirement says otherwise.
- Continue using the existing Svelte component conventions and TypeScript types. Keep frontend-only preferences (currently the theme) separate from encrypted vault data.
- Preserve the glass-style visual language, light/dark theme support, keyboard focus indicators, reduced-motion support, and responsive behavior when changing UI/CSS.
- Do not edit generated files under `src-tauri/gen/` or build outputs under `dist/` and `src-tauri/target*/` unless the task specifically concerns generated output.
- Do not make unrelated cleanup changes. Never overwrite or revert user changes you did not make.

## Documentation Accuracy

Use source code as the authority when it differs from documentation. Describe encryption at rest and local processing precisely; do not imply that all plaintext copies are zeroized, clipboard clearing is guaranteed, the app is formally audited, or all runtime activity has been independently verified. The known limitations are summarized in [`docs/SECURITY.md`](docs/SECURITY.md).
