# Security Model And Engineering Constraints

This document describes the implementation observed in the repository. It is not a security audit or a guarantee about every runtime dependency or the user's environment. Passvault is early v1 and the README states it has not received an independent security audit.

## Implemented Protections

- The local vault file is encrypted with XChaCha20-Poly1305. A 32-byte key is derived from the master password using Argon2id v1.3 with 65,536 KiB memory, 3 iterations, and parallelism 1. Salt and nonce values are generated with the operating system random source.
- The vault payload is encrypted as a whole and stored under Tauri's app-local-data directory. Writes use a temporary file, call `sync_all`, and rename it to the vault path.
- The backend stores an optional unlocked vault behind a `Mutex`; vault entry reads and mutations require unlocked state. Locking drops this backend state.
- Optional persistent unlock stores only the derived vault key after protecting it with Windows DPAPI for the current Windows user. On startup, Passvault tries that key against the encrypted vault; disabling the option or manually locking removes the protected key.
- Selected Rust key, password, plaintext, and entry buffers use `Zeroizing` or `zeroize` cleanup.
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
- The configured CSP and source architecture support local-first operation, but do not by themselves prove absence of all runtime traffic from every dependency or environment.

## Required Care For Changes

- Keep secrets out of logs, diagnostics, analytics, error strings, test output, examples, and screenshots. Use obviously fake test data only.
- Keep any vault-data operation behind unlocked-state checks. Validate IPC inputs in Rust as well as in the UI where appropriate.
- Do not write plaintext entries, CSV contents, master passwords, or derived keys to disk. Backups must remain encrypted vault-file copies unless a separately designed export feature is explicitly requested.
- Do not add network access, telemetry, cloud APIs, or remote logging without an explicit product decision. Such a change changes the threat model and requires review of CSP, capabilities, dependencies, consent, and documentation.
- Keep Tauri permissions minimal. Review `src-tauri/capabilities/default.json`, the CSP in `src-tauri/tauri.conf.json`, and command registration whenever adding native capabilities or IPC.
- Treat the vault format, serialization, KDF settings, cipher, and write path as compatibility/security-sensitive. Add tests for correct-password unlock, wrong-password rejection, ciphertext not containing known plaintext, and any migration/round-trip behavior changed.
- Preserve the user's understanding of destructive operations. Restore overwrites the current vault; deletion is permanent; there is no password recovery.
- Document security claims narrowly. Say "encrypted at rest" or describe the specific code path; do not claim full end-to-end security, guaranteed clipboard wiping, comprehensive memory erasure, formal audit, or recovery support.

## Relevant Tests

Rust unit tests in `src-tauri/src/lib.rs` exercise CSV field mapping and duplicate behavior, TOTP values/URI parsing, password generation rules, encrypted-vault round trips and wrong-password rejection, and defaults for older entry fields. Run them with:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
```

These are unit tests, not integration coverage or a security assessment.
