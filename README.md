# PassVault

Passvault is a Windows-first, local-only password manager. The vault is encrypted on this device; there is no account, server, telemetry, or sync in v1.

## Features

- Create and unlock a vault protected by a master password.
- Store logins, secure notes, and TOTP authenticator keys.
- Use a Proton-inspired glass workspace in a rounded, frameless Windows window, with optional SVG refraction accents and a CSS blur fallback.
- Switch between persistent light and dark themes in Settings.
- Import Proton Pass CSV exports with a preview, unsupported-column warnings, and possible-duplicate detection. Login email, username, TOTP, notes, item type, source vault, and original timestamps are retained; alias, card, identity, and custom-item note payloads are preserved.
- Generate passwords with guaranteed selected character classes and no adjacent repeated characters, search and favorite items, and copy secrets with a 30-second clipboard clear attempt.
- Automatically lock after 15 minutes of inactivity.
- Export and restore an encrypted `.passvault` backup.

## Windows Development Setup

Install:

- Rust stable with the MSVC target (`rustup default stable-x86_64-pc-windows-msvc`).
- Visual Studio 2022 Build Tools with the Desktop development with C++ workload and Windows SDK.
- Node.js LTS and npm.
- Microsoft Edge WebView2 Runtime.

Then run from this folder:

```powershell
npm install
npm run tauri dev
```

Build Windows installers with:

```powershell
npm run tauri build
```

The NSIS and MSI installers are written under `src-tauri/target/release/bundle/`.

## Storage and Security Notes

- Vault data is kept under the Windows **Local** application data directory and encrypted with Argon2id-derived keys and XChaCha20-Poly1305 authenticated encryption.
- Losing the master password means losing access to the vault. Keep encrypted backups somewhere safe and separate from this device.
- Restoring a backup replaces the current vault and requires the backup's master password.
- TOTP codes are generated locally with SHA-1/6-digit/30-second defaults, while explicit `otpauth://` parameters are honored. Secret normalization accepts Base32 formatting and raw-secret fallback. Treat imported CSV files, screenshots, clipboard contents, and exported backups as sensitive.
- This is an early v1 and has not received an independent security audit. Do not make it the only copy of important credentials until you have verified import, restore, and recovery behavior yourself.
