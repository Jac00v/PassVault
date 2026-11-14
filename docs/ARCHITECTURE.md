# Architecture

## Overview

Passvault is a Tauri 2 desktop application with one persistent vault window. Vite serves/builds the Svelte frontend, and Tauri exposes selected Rust functions to that frontend over IPC. A TOTP screen scan temporarily creates one borderless overlay window per monitor; these load a separate Vite page and close when the scan ends. There is no application server, database, frontend router, feature-component tree, or separate Rust service layer in the current codebase.

```text
index.html -> src/main.ts -> src/App.svelte <-> Tauri IPC -> src-tauri/src/lib.rs
                                  |                          |
                                  +-> src/styles.css          +-> app-local-data/vault.passvault

qr.html -> src/qr-overlay.ts -> src/QrOverlay.svelte -> Tauri IPC -> src-tauri/src/qr_scan.rs
```

## Important Files

- `index.html`: page shell and `#app` mount point.
- `src/main.ts`: mounts `App.svelte` and imports global CSS.
- `src/App.svelte`: all current screens, local state, derived entry list, event handlers, clipboard flow, timers, and Tauri command calls. Svelte 5 is used in legacy mode with `$:` reactive declarations.
- `src/styles.css`: visual tokens, glass-style surfaces, responsive breakpoints, focus styling, reduced motion, and dark theme overrides.
- `src-tauri/src/lib.rs`: backend state, serializable data types, registered commands, encrypted persistence, CSV logic, TOTP/password helpers, backup handling, and unit tests.
- `src-tauri/src/qr_scan.rs`: TOTP screen-scan subsystem: per-monitor capture, overlay window lifecycle, region decoding with `rqrr`, `otpauth` payload parsing and label sanitization, scan timeouts, and unit tests.
- `src-tauri/src/main.rs`: calls the library's `run()` entry point.
- `qr.html`: page shell for the screen-scan overlay windows.
- `src/qr-overlay.ts`: mounts `QrOverlay.svelte` into `qr.html`.
- `src/QrOverlay.svelte`: frozen-frame canvas, drag/keyboard region selection, confirmation and cancellation UI. It has scoped styles and does not import `styles.css`.
- `src-tauri/tauri.conf.json`: development/build hooks, window configuration, CSP, and Windows NSIS/MSI targets.
- `src-tauri/capabilities/default.json`: core/window/opener permissions assigned to the `main` webview. Overlay windows have no capability entry; see `docs/SECURITY.md` for what that does and does not restrict.
- `vite.config.ts`: Svelte plugin, dev-server port, and the multi-page build input (`index.html` and `qr.html`).

## Frontend State And Flows

`App.svelte` owns the application state. The vault gate mode is `loading`, `create`, `unlock`, or `ready`. On mount, `vault_status` selects create or unlock. Creating/unlocking calls the corresponding Rust command, then `get_entries` loads decrypted entries into frontend memory. Locking attempts clipboard cleanup, calls `lock_vault`, clears frontend entry/editor/import/TOTP state even if key removal reports an error, and returns to unlock mode.

Entries are rendered in a three-pane workspace: navigation, filtered list, and details. `filteredEntries` filters by view and a case-insensitive search across title, username, email, URL, and source vault, then sorts favorites first and title alphabetically. It deliberately does not search passwords or notes. Appearance is stored in `localStorage` under `passvault-theme`. The optional keep-unlocked setting is represented by a Windows DPAPI-protected derived key in app-local data, not by a stored master password. When enabled, startup attempts auto-unlock and the inactivity auto-lock is skipped; manual lock removes the protected key and turns the option off.

Creating/editing and favoriting use `save_entry`; deleting uses `delete_entry`. These operations update the encrypted file on the Rust side, then the UI updates its in-memory list. Keep selection and TOTP refresh behavior coherent when changing these flows.

The inactivity timer runs once per second while the vault is ready and locks after 15 minutes since the last `pointerdown` or `keydown`. This is UI-event-based, not a system-wide idle detector. TOTP refreshes when the current code-step counter changes, rather than requiring a tick to land on an exact boundary second. Copy operations use the browser clipboard and schedule a conditional clear attempt after 30 seconds.

The window uses native Windows decorations and requests Windows 11 rounded corners through DWM, with an opaque native/WebView background color. A close-request listener awaits best-effort clipboard cleanup before the default close proceeds.

CSV import reads the selected file as text in the frontend, asks Rust for a preview, presents warnings/possible duplicates, then submits the selected import mode. Backups use native Rust file dialogs and copy/restore the encrypted vault file. Restore replaces the current local vault and is reachable from the sidebar and from the unlock screen using a password field.

## IPC Command Surface

The definitive command registration is the `tauri::generate_handler!` list in `src-tauri/src/lib.rs`:

| Command | Purpose | Unlocked vault required |
| --- | --- | --- |
| `vault_status` | Report whether the local vault file exists | No |
| `create_vault` | Create and unlock an empty vault | No |
| `unlock_vault` | Decrypt an existing vault | No |
| `try_auto_unlock_vault` | Unlock using the optional Windows-protected key | No |
| `set_keep_unlocked` | Add or remove the optional protected key | Enabling requires unlocked state |
| `lock_vault` | Drop unlocked state, then remove the protected key | No |
| `get_entries` | Return a clone of all entries | Yes |
| `save_entry` | Insert or replace an entry and persist | Yes |
| `delete_entry` | Remove an entry and persist | Yes |
| `preview_csv` | Parse CSV and produce import preview | No |
| `import_entries` | Apply preview entries and persist | Yes |
| `current_totp` | Generate a TOTP code for the provided secret | No |
| `generate_password` | Generate a password | No |
| `export_backup` | Copy encrypted vault file to chosen path | Yes |
| `restore_backup` | Decrypt selected backup, replace vault, unlock | No |
| `scan_qr_from_screen` | Hide the vault window, capture every monitor, run an overlay scan session, return the decoded setup code or `null` | No |
| `qr_overlay_frame` | Overlay only: return that monitor's frozen frame as raw bytes | No |
| `qr_overlay_ready` | Overlay only: record that the overlay is laid out, then show it and focus it when it is the active overlay | No |
| `qr_overlay_activity` | Overlay only: report input activity for the idle timeout | No |
| `qr_scan_region` | Overlay only: validate a selection rectangle, decode it, store the result in the session | No |
| `qr_scan_submit` | Overlay only: resolve the scan with the stored result | No |
| `qr_scan_cancel` | Overlay only: resolve the scan with no result | No |

Rust serde uses camelCase for entry and result fields exchanged with TypeScript. If a command's arguments or return type change, update the corresponding `invoke` calls and frontend TypeScript types as part of the same change.

The scan commands are registered from `qr_scan.rs` rather than `lib.rs`, and none of them reads or mutates vault data. `scan_qr_from_screen` checks that the caller is the `main` window; each `qr_overlay_*`/`qr_scan_*` command resolves the caller label against the frames of the active session, so a window outside that session cannot obtain a frame or submit a result. These checks are enforced in Rust; the rule that a result is applied only while the editor is open and the vault is ready is enforced in `App.svelte`.

## Screen QR Scan Flow

The magnifier action in a login composer's TOTP row (`src/App.svelte:1287`, handler `scanTotpFromScreen` at `src/App.svelte:548`) calls `scan_qr_from_screen`. The action is rendered only for `login` entries, and the handler does nothing unless the vault is `ready` and the editor is open.

```text
App.svelte: invoke scan_qr_from_screen
  -> qr_scan.rs: reject re-entrant session, hide "main", wait 200 ms
                 capture every monitor into RAM (xcap)
                 create one hidden overlay per monitor -> qr.html
     -> overlay: qr_overlay_frame  (8-byte LE width/height header + RGBA)
     -> overlay: qr_overlay_ready  (show + focus)
     -> user selects a region
     -> overlay: qr_scan_region    (frame-relative pixel rect)
          -> crop to luma, decode passes (rqrr), parse otpauth
          -> session stores the decoded value; overlay receives {ok,label,message}
     -> overlay: qr_scan_submit | qr_scan_cancel
  -> qr_scan.rs: close overlays, zeroize/drop frames, restore and focus "main"
  -> promise resolves with {value,label} or null
```

On success the frontend writes `result.value` into the form field and shows a toast containing only the sanitized label. Nothing is persisted until the composer is submitted through `save_entry`.

### Frozen-frame overlay design

The overlay displays a screenshot taken after the vault window hides, not live screen content. The canvas is display-only (`pointer-events: none`) and the dimming around the selection is drawn with CSS, so the overlay never recomposites the desktop. Because the frame is frozen, the image stays what it was at capture time even if windows behind it change.

Each overlay is a borderless, always-on-top, non-resizable, taskbar-skipping window created at the monitor's physical geometry and pointed at `WebviewUrl::App("qr.html")`. Its inner size is the physical size divided by the monitor scale factor; pointer positions are mapped through the canvas backing size, so mixed-DPI layouts need no device-pixel-ratio assumptions. The overlay reports a selection in frozen-frame pixel coordinates and Rust performs the crop, so decode results do not depend on window CSS size.

Selection starts as a centred box sized to 60% of the smaller frame dimension. Arrow keys move it, `Shift`+arrows resize it, `Enter` scans or confirms, and `Esc` cancels (or steps back from confirmation). Input activity is reported to `qr_overlay_activity`, throttled to one call every two seconds in the overlay.

The session ends on submit, cancel, a closed/destroyed overlay, or a timeout. A ready watchdog aborts if no overlay becomes ready within 10 seconds, an idle timeout aborts after 90 seconds without overlay input, and a hard cap ends the session after 3 minutes. A `ScanCleanup` `Drop` guard closes overlays and restores/focuses the vault window on every exit path, including capture errors. Overlay windows also get a Rust-side `on_window_event` cancel handler, because a destroyed webview cannot call `qr_scan_cancel`. A region decode is single-flight per session; a second request while one is running returns "Still reading the last selection".

### Scanned Payload And Storage Rule

- Accepted input: `otpauth://totp/...` URLs and plain Base32 secrets. Plain secrets must strictly decode after normalization (uppercase, whitespace/`-`/`_` removed, padding only at the end); the manual field's raw-bytes fallback in `parse_totp` is deliberately not used for scans.
- Rejected input: payloads over 4 KiB, payloads containing control characters, non-`otpauth` schemes, `otpauth://hotp/...`, and `otpauth-migration://` exports. The last two return specific messages; everything else returns a generic "not a TOTP setup code" message. The `image=` parameter is never fetched.
- Storage rule: a default setup (SHA-1, 6 digits, 30-second period) is stored as the bare normalized Base32 secret. Any non-default algorithm, digit count, or period is stored as a canonical `otpauth://totp/...` URI containing only the non-default parameters, so generated codes stay correct. Both forms are re-validated through `parse_totp` before being returned.
- Label rule: issuer and account are used only for the display label and toast. Control and format characters (Unicode categories Cc and Cf, which includes bidi overrides) are stripped, whitespace is collapsed, each part is capped at 64 characters with an ellipsis, and the parts are joined as `Issuer · account`. The frontend receives only `value` and `label`; the secret never passes through the overlay webview.


## Vault Data And Persistence

The vault lives at `app.path().app_local_data_dir()?.join("vault.passvault")`. The outer file is JSON containing a version, salt, nonce, and base64 ciphertext. The encrypted payload is JSON `VaultData`, currently a vector of `VaultEntry` records. Each write serializes and encrypts the full vault, writes a sibling `.vault-tmp` file, syncs it, and renames it into place.

Current vault format version is `1`. A few newer entry metadata fields have serde defaults to read older entries, but there is no general migration framework. Treat changes to the serialized model and crypto parameters as compatibility-sensitive.

`VaultEntry` is serialized with camelCase field names and contains `id`, `title`, `username`, `email`, `password`, `url`, `notes`, `totpSecret`, `cardNumber`, `cardHolder`, `cardExpiry`, `cardCvv`, `cardPin`, `favorite`, `kind`, `sourceVault`, `sourceCreatedAt`, and `sourceModifiedAt`. Current UI kinds include `login`, `note`, `alias`, `creditCard`, `identity`, and `custom`. The backend's `kind` is a string rather than a strict enum. Card fields have serde defaults for older entries. Imported card note payloads are mapped into the card fields when their keys are recognized; identity and custom-item note payloads are retained as formatted JSON text in `notes`.

## Import And Utility Semantics

- CSV headers are normalized by removing non-ASCII-alphanumeric characters and lowercasing. Rust maps known aliases for name/title, username, email, password, URL, notes, TOTP, card number, cardholder, expiration, security code, PIN, favorite, item type, source vault, and timestamps. Unknown columns produce a warning rather than being imported.
- Empty rows are skipped. Rows without a title but with other content become `Untitled` and produce a warning. Unrecognized item types default to `login`; imported entries receive new UUIDs.
- Duplicate detection is heuristic, using normalized title with username/email, title with URL, and URL with username/email signatures. Ambiguous matches are flagged as possible duplicates but do not get an automatic update target.
- Import mode `all` adds every preview entry. `skip` omits flagged possible duplicates. `update` merges only nonempty incoming fields into a uniquely matched existing entry; the existing favorite is preserved (`existing.favorite OR incoming.favorite`). An unmatched/ambiguous duplicate is skipped in update mode. The commit command trusts duplicate metadata returned to it by the frontend; it does not rerun preview matching, but it does validate entry IDs (generating replacements for empty or colliding values), titles, and empty kinds.
- TOTP accepts raw secrets or `otpauth://totp/` URLs. Defaults are SHA-1, six digits, and a 30-second step; URL parameters can select SHA-1/SHA-256/SHA-512, 1-9 digits, and a period from 1 through 65,535 seconds. Secret normalization removes whitespace, hyphens, underscores, and trailing padding; if Base32 decoding fails, the normalized value is treated as raw bytes.
- Password generation accepts lengths 12-128. It includes lowercase, uppercase, and digits, optionally includes symbols from `!@#$%^&*`, and avoids adjacent repeated characters. The UI currently requests length 20 with symbols enabled.

## Development Commands

Run at the repository root:

```powershell
npm install
npm run tauri dev
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build
```

The package build command is `svelte-check && vite build`. Tauri invokes `npm run dev` before development and `npm run build` before a production build. The Vite server is pinned to `127.0.0.1:1420`. The configured installer targets are NSIS and MSI.
