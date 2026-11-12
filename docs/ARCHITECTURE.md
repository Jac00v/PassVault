# Architecture

## Overview

Passvault is a single-window Tauri 2 desktop application. Vite serves/builds the Svelte frontend, and Tauri exposes selected Rust functions to that frontend over IPC. There is no application server, database, frontend router, feature-component tree, or separate Rust service layer in the current codebase.

```text
index.html -> src/main.ts -> src/App.svelte <-> Tauri IPC -> src-tauri/src/lib.rs
                                  |                          |
                                  +-> src/styles.css          +-> app-local-data/vault.passvault
```

## Important Files

- `index.html`: page shell and `#app` mount point.
- `src/main.ts`: mounts `App.svelte` and imports global CSS.
- `src/App.svelte`: all current screens, local state, derived entry list, event handlers, clipboard flow, timers, and Tauri command calls. Svelte 5 is used in legacy mode with `$:` reactive declarations.
- `src/styles.css`: visual tokens, glass-style surfaces, responsive breakpoints, focus styling, reduced motion, and dark theme overrides.
- `src-tauri/src/lib.rs`: backend state, serializable data types, registered commands, encrypted persistence, CSV logic, TOTP/password helpers, backup handling, and unit tests.
- `src-tauri/src/main.rs`: calls the library's `run()` entry point.
- `src-tauri/tauri.conf.json`: development/build hooks, window configuration, CSP, and Windows NSIS/MSI targets.
- `src-tauri/capabilities/default.json`: core/window/opener permissions assigned to the `main` webview.

## Frontend State And Flows

`App.svelte` owns the application state. The vault gate mode is `loading`, `create`, `unlock`, or `ready`. On mount, `vault_status` selects create or unlock. Creating/unlocking calls the corresponding Rust command, then `get_entries` loads decrypted entries into frontend memory. Locking attempts clipboard cleanup, calls `lock_vault`, clears frontend entry/editor/import/TOTP state, and returns to unlock mode.

Entries are rendered in a three-pane workspace: navigation, filtered list, and details. `filteredEntries` filters by view and a case-insensitive search across title, username, email, URL, and source vault, then sorts favorites first and title alphabetically. It deliberately does not search passwords or notes. Appearance is stored in `localStorage` under `passvault-theme`. The optional keep-unlocked setting is represented by a Windows DPAPI-protected derived key in app-local data, not by a stored master password. When enabled, startup attempts auto-unlock and the inactivity auto-lock is skipped; manual lock removes the protected key and turns the option off.

Creating/editing and favoriting use `save_entry`; deleting uses `delete_entry`. These operations update the encrypted file on the Rust side, then the UI updates its in-memory list. Keep selection and TOTP refresh behavior coherent when changing these flows.

The inactivity timer runs once per second and locks after 15 minutes since the last `pointerdown` or `keydown`. This is UI-event-based, not a system-wide idle detector. TOTP refreshes at the current code-step boundary. Copy operations use the browser clipboard and schedule a conditional clear attempt after 30 seconds.

The window uses native Windows decorations and requests Windows 11 rounded corners through DWM, with an opaque native/WebView background color. A close-request listener starts best-effort clipboard cleanup without cancelling the close.

CSV import reads the selected file as text in the frontend, asks Rust for a preview, presents warnings/possible duplicates, then submits the selected import mode. Backups use native Rust file dialogs and copy/restore the encrypted vault file. Restore replaces the current local vault.

## IPC Command Surface

The definitive command registration is the `tauri::generate_handler!` list in `src-tauri/src/lib.rs`:

| Command | Purpose | Unlocked vault required |
| --- | --- | --- |
| `vault_status` | Report whether the local vault file exists | No |
| `create_vault` | Create and unlock an empty vault | No |
| `unlock_vault` | Decrypt an existing vault | No |
| `try_auto_unlock_vault` | Unlock using the optional Windows-protected key | No |
| `set_keep_unlocked` | Add or remove the optional protected key | Enabling requires unlocked state |
| `lock_vault` | Remove the protected key and drop unlocked state | No |
| `get_entries` | Return a clone of all entries | Yes |
| `save_entry` | Insert or replace an entry and persist | Yes |
| `delete_entry` | Remove an entry and persist | Yes |
| `preview_csv` | Parse CSV and produce import preview | No |
| `import_entries` | Apply preview entries and persist | Yes |
| `current_totp` | Generate a TOTP code for the provided secret | No |
| `generate_password` | Generate a password | No |
| `export_backup` | Copy encrypted vault file to chosen path | Yes |
| `restore_backup` | Decrypt selected backup, replace vault, unlock | No |

Rust serde uses camelCase for entry and result fields exchanged with TypeScript. If a command's arguments or return type change, update the corresponding `invoke` calls and frontend TypeScript types as part of the same change.

## Vault Data And Persistence

The vault lives at `app.path().app_local_data_dir()?.join("vault.passvault")`. The outer file is JSON containing a version, salt, nonce, and base64 ciphertext. The encrypted payload is JSON `VaultData`, currently a vector of `VaultEntry` records. Each write serializes and encrypts the full vault, writes a sibling `.vault-tmp` file, syncs it, and renames it into place.

Current vault format version is `1`. A few newer entry metadata fields have serde defaults to read older entries, but there is no general migration framework. Treat changes to the serialized model and crypto parameters as compatibility-sensitive.

`VaultEntry` is serialized with camelCase field names and contains `id`, `title`, `username`, `email`, `password`, `url`, `notes`, `totpSecret`, `favorite`, `kind`, `sourceVault`, `sourceCreatedAt`, and `sourceModifiedAt`. Current UI kinds include `login`, `note`, `alias`, `creditCard`, `identity`, and `custom`. The backend's `kind` is a string rather than a strict enum. For imported card, identity, and custom items, structured CSV note payloads are retained as formatted JSON text in `notes`.

## Import And Utility Semantics

- CSV headers are normalized by removing non-ASCII-alphanumeric characters and lowercasing. Rust maps known aliases for name/title, username, email, password, URL, notes, TOTP, favorite, item type, source vault, and timestamps. Unknown columns produce a warning rather than being imported.
- Empty rows are skipped. Rows without a title but with other content become `Untitled` and produce a warning. Unrecognized item types default to `login`; imported entries receive new UUIDs.
- Duplicate detection is heuristic, using normalized title with username/email, title with URL, and URL with username/email signatures. Ambiguous matches are flagged as possible duplicates but do not get an automatic update target.
- Import mode `all` adds every preview entry. `skip` omits flagged possible duplicates. `update` merges only nonempty incoming fields into a uniquely matched existing entry; the existing favorite is preserved (`existing.favorite OR incoming.favorite`). An unmatched/ambiguous duplicate is skipped in update mode. The commit command trusts duplicate metadata returned to it by the frontend; it does not rerun preview matching.
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
