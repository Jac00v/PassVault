# Security Model And Engineering Constraints

This document describes the implementation observed in the repository. It is not a security audit or a guarantee about every runtime dependency or the user's environment. Passvault is early v1 and the README states it has not received an independent security audit.

## Implemented Protections

- The local vault file is encrypted with XChaCha20-Poly1305. A 32-byte key is derived from the master password using Argon2id v1.3 with 65,536 KiB memory, 3 iterations, and parallelism 1. Salt and nonce values are generated with the operating system random source.
- The vault payload is encrypted as a whole and stored under Tauri's app-local-data directory. Writes use a temporary file, call `sync_all`, and rename it to the vault path.
- The backend stores an optional unlocked vault behind a `Mutex`; vault entry reads and mutations require unlocked state. Locking drops this backend state.
- Optional persistent unlock stores only the derived vault key after protecting it with Windows DPAPI for the current Windows user. On startup, Passvault tries that key against the encrypted vault; disabling the option or manually locking removes the protected key.
- Selected Rust key, password, plaintext, and entry buffers use `Zeroizing` or `zeroize` cleanup.
- A TOTP screen scan captures monitor pixels into process memory only, never writes a screenshot to disk, and adds no network request. See [Screen QR Scanning](#screen-qr-scanning) for the exact lifetime and limits.
- CSP `connect-src` is limited to Tauri IPC endpoints. The current source has no application network client or sync implementation. The app may open a saved HTTP(S) URL in the system browser through the opener plugin; the capability scope allows only `http` and `https` URLs.
- The UI previews a CSV before importing it, and backup export copies the encrypted file rather than exporting plaintext entries.

## Important Limits

- While unlocked, decrypted entries are held in Rust and cloned/serialized over IPC into JavaScript state. JavaScript strings, webview state, and all cloned/transient copies cannot be reliably zeroized by this application.
- Clipboard cleanup after 30 seconds is best effort. It clears only when the clipboard still contains the value Passvault copied, and clipboard access can fail. The close handler waits for cleanup before the window closes, but a denied clipboard permission can still leave the value in place.
- Auto-lock is triggered by 15 minutes without `pointerdown` or `keydown` in this webview. It is not an operating-system idle monitor.
- When persistent unlock is enabled, the 15-minute inactivity auto-lock is disabled so the vault stays unlocked until the user manually locks it or turns the option off. Anyone with access to the same Windows user account can launch Passvault and access the vault while the protected key is present. DPAPI protection is not a substitute for requiring the master password at every launch.
- Losing the master password means losing access; there is no password reset or recovery mechanism. Restore replaces the current local vault. Backups should be protected separately.
- Import duplicate checks are heuristic and preview metadata is supplied back by the frontend at commit time. The backend validates incoming entry IDs, titles, and kinds but does not recompute duplicate matching, so do not represent duplicate matching as exact.
- `save_entry` and `import_entries` perform limited validation (notably titles, missing IDs/kinds, and ID collisions). Do not assume every field is deeply validated by Rust.
- The implementation's zeroization does not erase every plaintext copy. For example, strings can be cloned for IPC, and ordinary frontend strings are garbage collected.
- Screen-scan overlay windows have no capability entry, which denies core and plugin commands but does not restrict app-defined Tauri commands; treat that as a narrow boundary and read [Screen QR Scanning](#screen-qr-scanning) before relying on it.
- The configured CSP and source architecture support local-first operation, but do not by themselves prove absence of all runtime traffic from every dependency or environment.

## Screen QR Scanning

### Capture and lifetime

- A scan starts only from the vault window's TOTP row, which invokes `scan_qr_from_screen`. Rust hides the `main` window, waits 200 ms, and captures every monitor with `xcap` into process memory. No screenshot or decoded payload is written to the vault, app data, a temporary file, the clipboard, or a log; the feature adds no network access.
- Frames are stored per monitor as `Zeroizing<Vec<u8>>` inside `QrScanState`. Crops, inverted/thresholded/upscaled luma buffers, decoded payload strings, and the extracted and normalized Base32 secret are also `Zeroizing`. The session is dropped when the scan ends, and the decoded `ScannedOtp` zeroizes its `value` and `label` fields in `Drop`.
- Zeroization is best effort at the Rust buffer level and is not guaranteed erasure:
  - `rqrr`'s `PreparedImage` owns a binarized copy of the crop that the crate does not expose for zeroization; it is dropped normally after decoding.
  - `qr_overlay_frame` builds a serialized copy of the frame for the IPC response. That copy is not zeroized by application code.
  - The overlay webview holds an `ArrayBuffer`, `ImageData`, and canvas pixels decoded from that frame. Rust cannot reach or erase those copies, and the overlay does not attempt to zeroize them.
  - The decoded setup code is serialized to JSON for the IPC response to the main window and becomes a JavaScript string in `App.svelte`. Those copies follow the normal webview memory model.
  - The OS, GPU driver, and WebView2 may retain additional copies outside the application's control.
- The decoded secret string is never sent to an overlay window. Overlays receive only `{ok, label, message}` and the frozen frame, which can itself contain the QR image. Labels are sanitized in Rust by removing Unicode categories Cc and Cf, collapsing whitespace, and truncating each part to 64 characters, and the overlay renders them through Svelte's escaped interpolation only.

### Overlay isolation

- Overlay windows are created at runtime with per-session labels beginning with `qr-overlay-` (the session token is part of the label) and load the bundled `qr.html` page from the application's own build output. They have no entry in `src-tauri/capabilities/default.json`, so core and plugin commands (window, event, opener, and similar) are denied to them.
- They are not isolated from application-defined commands. In Tauri 2, commands registered in `tauri::generate_handler!` are not gated by the capability ACL, so an overlay webview can invoke any registered command, including vault commands such as `get_entries`. The actual boundary is:
  - the overlay page runs only first-party local code shipped in the bundle; it loads no remote content under the CSP;
  - `scan_qr_from_screen` rejects callers whose window label is not `main`;
  - every overlay command requires the caller label to match a frame in the active scan session, overlay labels include the session token, and the window-event cancel path also checks the token and frame label;
  - untrusted issuer/account strings are stripped and escaped, and the overlay uses no `{@html}`.
- Do not describe the overlay as sandboxed or capability-isolated from the vault. A compromised overlay page could call vault commands. The mitigation is that the page cannot be reached by untrusted content, not that the command surface is restricted.

### Display and capture limits

- Capture uses `xcap`'s Windows capture path. Windows protected by hardware DRM or protected composition can capture as black or blank, and some GPU-composited surfaces may not be captured reliably. A QR code displayed in such a window will not decode, and the scan reports a generic "no QR code found" error. There is no fallback capture backend.
- The frozen frame shows whatever was on that monitor at capture time, not only the QR code. It is dimmed by the overlay, but the full frame is visible in the overlay window and was sent to that webview. Select a region containing only the QR code, and use a test or throwaway QR code when demonstrating or recording the feature.
- The vault window is hidden before capture, but other windows and notifications remain in the frame.
- If any monitor fails to capture, the whole session aborts and the vault window is restored; there is no partial single-monitor mode.
- Sessions end on submit, cancel, a closed or destroyed overlay, a 10-second ready watchdog, 90 seconds without overlay input, or a 3-minute hard cap. The vault window is restored and refocused by a `Drop` guard on every exit path.
- Rust unit tests cover decode passes, payload validation, label sanitization, and the storage rule. The window lifecycle and multi-monitor capture behavior are not covered by automated tests and require manual verification on Windows with the target display configuration.

## Required Care For Changes

- Keep secrets out of logs, diagnostics, analytics, error strings, test output, examples, and screenshots. Use obviously fake test data only.
- Keep any vault-data operation behind unlocked-state checks. Validate IPC inputs in Rust as well as in the UI where appropriate.
- Do not write plaintext entries, CSV contents, master passwords, or derived keys to disk. Backups must remain encrypted vault-file copies unless a separately designed export feature is explicitly requested.
- Do not add network access, telemetry, cloud APIs, or remote logging without an explicit product decision. Such a change changes the threat model and requires review of CSP, capabilities, dependencies, consent, and documentation.
- Keep Tauri permissions minimal. Review `src-tauri/capabilities/default.json`, the CSP in `src-tauri/tauri.conf.json`, and command registration whenever adding native capabilities or IPC.
- Keep the screen-scan overlay restricted to first-party bundled content, keep the caller-label checks in `qr_scan.rs`, and keep rendering QR-derived labels through escaped interpolation. Do not add remote content, `{@html}`, network requests, or overlay-facing commands that return vault data.
- Treat the vault format, serialization, KDF settings, cipher, and write path as compatibility/security-sensitive. Add tests for correct-password unlock, wrong-password rejection, ciphertext not containing known plaintext, and any migration/round-trip behavior changed.
- Preserve the user's understanding of destructive operations. Restore overwrites the current vault; deletion is permanent; there is no password recovery.
- Document security claims narrowly. Say "encrypted at rest" or describe the specific code path; do not claim full end-to-end security, guaranteed clipboard wiping, comprehensive memory erasure, formal audit, or recovery support.

## Relevant Tests

Rust unit tests in `src-tauri/src/lib.rs` exercise CSV field mapping and duplicate behavior, TOTP values/URI parsing, password generation rules, encrypted-vault round trips and wrong-password rejection, and defaults for older entry fields. `src-tauri/src/qr_scan.rs` adds tests for synthetic QR decoding, region validation, payload rejection, label sanitization, and the scanned-secret storage rule. Run them with:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
```

These are unit tests, not integration coverage or a security assessment.
