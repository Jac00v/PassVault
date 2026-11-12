use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::Manager;
use totp_rs::{Algorithm as TotpAlgorithm, TOTP};
use url::Url;
use uuid::Uuid;
#[cfg(windows)]
use windows::Win32::Foundation::{LocalFree, HLOCAL};
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
#[cfg(windows)]
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use zeroize::{Zeroize, Zeroizing};

const VAULT_VERSION: u32 = 1;
const KDF_MEMORY_KIB: u32 = 65_536;
const KDF_ITERATIONS: u32 = 3;
const KDF_PARALLELISM: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct VaultData {
    entries: Vec<VaultEntry>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEntry {
    id: String,
    title: String,
    username: String,
    #[serde(default)]
    email: String,
    password: String,
    url: String,
    notes: String,
    totp_secret: String,
    favorite: bool,
    kind: String,
    #[serde(default)]
    source_vault: String,
    #[serde(default)]
    source_created_at: String,
    #[serde(default)]
    source_modified_at: String,
}

impl Drop for VaultEntry {
    fn drop(&mut self) {
        self.title.zeroize();
        self.username.zeroize();
        self.email.zeroize();
        self.password.zeroize();
        self.url.zeroize();
        self.notes.zeroize();
        self.totp_secret.zeroize();
        self.source_vault.zeroize();
        self.source_created_at.zeroize();
        self.source_modified_at.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

struct UnlockedVault {
    key: Zeroizing<[u8; 32]>,
    data: VaultData,
}

struct AppState {
    vault_path: PathBuf,
    remembered_key_path: PathBuf,
    unlocked: Mutex<Option<UnlockedVault>>,
}

#[cfg(windows)]
fn use_rounded_window_corners(window: &tauri::WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&preference) as u32,
        );
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportPreview {
    entries: Vec<VaultEntry>,
    duplicate_ids: Vec<String>,
    duplicate_matches: Vec<DuplicateMatch>,
    warnings: Vec<String>,
    skipped: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateMatch {
    incoming_id: String,
    existing_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResult {
    added: usize,
    updated: usize,
    skipped: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TotpCode {
    code: String,
    remaining: u64,
    step: u64,
}

impl VaultFile {
    fn read(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|_| "Could not read the vault file".to_string())?;
        serde_json::from_slice(&bytes)
            .map_err(|_| "The vault file is invalid or damaged".to_string())
    }

    fn from_data(data: &VaultData, key: &[u8; 32], salt: &[u8; 16]) -> Result<Self, String> {
        let mut nonce = [0_u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|_| "Could not initialize vault encryption".to_string())?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(data)
                .map_err(|_| "Could not serialize vault contents".to_string())?,
        );
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext.as_slice())
            .map_err(|_| "Could not encrypt the vault".to_string())?;

        Ok(Self {
            version: VAULT_VERSION,
            salt: BASE64.encode(salt),
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(ciphertext),
        })
    }

    fn decrypt(&self, password: &str) -> Result<UnlockedVault, String> {
        let salt = decode_fixed::<16>(&self.salt)?;
        let key = derive_key(password, &salt)?;
        self.decrypt_with_key(key)
    }

    fn decrypt_with_key(&self, key: Zeroizing<[u8; 32]>) -> Result<UnlockedVault, String> {
        if self.version != VAULT_VERSION {
            return Err("This vault version is not supported".to_string());
        }
        let nonce = decode_fixed::<24>(&self.nonce)?;
        let ciphertext = BASE64
            .decode(&self.ciphertext)
            .map_err(|_| "The vault file is invalid or damaged".to_string())?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key[..])
            .map_err(|_| "Could not initialize vault encryption".to_string())?;
        let mut plaintext = cipher
            .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| "Incorrect master password or damaged vault".to_string())?;
        let data = serde_json::from_slice(&plaintext)
            .map_err(|_| "The decrypted vault contents are invalid".to_string())?;
        plaintext.zeroize();

        Ok(UnlockedVault { key, data })
    }

    fn persist(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "Could not serialize encrypted vault".to_string())?;
        let temp_path = path.with_extension("vault-tmp");
        let mut file = fs::File::create(&temp_path)
            .map_err(|_| "Could not create a temporary vault file".to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Could not write encrypted vault data".to_string())?;
        fs::rename(&temp_path, path).map_err(|_| {
            let _ = fs::remove_file(&temp_path);
            "Could not safely replace the vault file".to_string()
        })
    }
}

fn decode_fixed<const N: usize>(encoded: &str) -> Result<[u8; N], String> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "The vault file is invalid or damaged".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "The vault file is invalid or damaged".to_string())
}

fn derive_key(password: &str, salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(KDF_MEMORY_KIB, KDF_ITERATIONS, KDF_PARALLELISM, Some(32))
        .map_err(|_| "Could not initialize password-based key derivation".to_string())?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; 32]);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key[..])
        .map_err(|_| "Could not derive the vault encryption key".to_string())?;
    Ok(key)
}

fn lock_state(
    state: &AppState,
) -> Result<std::sync::MutexGuard<'_, Option<UnlockedVault>>, String> {
    state
        .unlocked
        .lock()
        .map_err(|_| "Vault state is unavailable; restart the app".to_string())
}

#[cfg(windows)]
fn protect_remembered_key(key: &[u8; 32]) -> Result<Vec<u8>, String> {
    const ENTROPY: &[u8] = b"Passvault persistent unlock key v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: key.len() as u32,
        pbData: key.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| "Windows could not protect the remembered unlock key".to_string())?;
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Ok(protected)
}

#[cfg(windows)]
fn unprotect_remembered_key(protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, String> {
    const ENTROPY: &[u8] = b"Passvault persistent unlock key v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: protected.len() as u32,
        pbData: protected.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| "Windows could not access the remembered unlock key".to_string())?;
    if output.pbData.is_null() || output.cbData != 32 {
        if !output.pbData.is_null() {
            unsafe {
                std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).zeroize();
                LocalFree(Some(HLOCAL(output.pbData.cast())));
            }
        }
        return Err("The remembered unlock key is invalid".to_string());
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    unsafe {
        std::ptr::copy_nonoverlapping(output.pbData, key.as_mut_ptr(), key.len());
        std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).zeroize();
        LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    Ok(key)
}

fn save_remembered_key(path: &Path, key: &[u8; 32]) -> Result<(), String> {
    #[cfg(windows)]
    let protected = protect_remembered_key(key)?;
    #[cfg(not(windows))]
    let protected = {
        let _ = (path, key);
        return Err("Persistent unlock is available only on Windows".to_string());
    };

    let temp_path = path.with_extension("key-tmp");
    let mut file = fs::File::create(&temp_path)
        .map_err(|_| "Could not create the protected unlock-key file".to_string())?;
    file.write_all(&protected)
        .and_then(|_| file.sync_all())
        .map_err(|_| {
            let _ = fs::remove_file(&temp_path);
            "Could not save the protected unlock key".to_string()
        })?;
    remove_remembered_key(path)?;
    fs::rename(&temp_path, path).map_err(|_| {
        let _ = fs::remove_file(&temp_path);
        "Could not safely save the protected unlock key".to_string()
    })
}

fn remove_remembered_key(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Could not remove the protected unlock key".to_string()),
    }
}

#[cfg(windows)]
fn load_remembered_key(path: &Path) -> Result<Zeroizing<[u8; 32]>, String> {
    let protected =
        fs::read(path).map_err(|_| "Could not read the protected unlock key".to_string())?;
    unprotect_remembered_key(&protected)
}

fn persist_unlocked(state: &AppState, vault: &UnlockedVault) -> Result<(), String> {
    let file = VaultFile::from_data(
        &vault.data,
        &vault.key,
        &decode_fixed::<16>(&VaultFile::read(&state.vault_path)?.salt)?,
    )?;
    file.persist(&state.vault_path)
}

#[tauri::command]
fn vault_status(state: tauri::State<'_, AppState>) -> bool {
    state.vault_path.exists()
}

#[tauri::command]
fn create_vault(master_password: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let master_password = Zeroizing::new(master_password);
    if master_password.chars().count() < 12 {
        return Err("Use a master password with at least 12 characters".to_string());
    }
    if state.vault_path.exists() {
        return Err("A vault already exists on this device".to_string());
    }

    let mut salt = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let key = derive_key(&master_password, &salt)?;
    let data = VaultData::default();
    let file = VaultFile::from_data(&data, &key, &salt)?;
    file.persist(&state.vault_path)?;
    let new_vault = UnlockedVault { key, data };
    *lock_state(&state)? = Some(new_vault);
    Ok(())
}

#[tauri::command]
fn unlock_vault(master_password: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let master_password = Zeroizing::new(master_password);
    let file = VaultFile::read(&state.vault_path)?;
    let unlocked = file.decrypt(&master_password)?;
    *lock_state(&state)? = Some(unlocked);
    Ok(())
}

#[tauri::command]
fn try_auto_unlock_vault(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    if !state.remembered_key_path.exists() {
        return Ok(false);
    }

    #[cfg(windows)]
    {
        let key = match load_remembered_key(&state.remembered_key_path) {
            Ok(key) => key,
            Err(_) => {
                remove_remembered_key(&state.remembered_key_path)?;
                return Ok(false);
            }
        };
        let file = match VaultFile::read(&state.vault_path) {
            Ok(file) => file,
            Err(_) => {
                remove_remembered_key(&state.remembered_key_path)?;
                return Ok(false);
            }
        };
        let unlocked = match file.decrypt_with_key(key) {
            Ok(unlocked) => unlocked,
            Err(_) => {
                remove_remembered_key(&state.remembered_key_path)?;
                return Ok(false);
            }
        };
        *lock_state(&state)? = Some(unlocked);
        Ok(true)
    }

    #[cfg(not(windows))]
    {
        remove_remembered_key(&state.remembered_key_path)?;
        Ok(false)
    }
}

#[tauri::command]
fn set_keep_unlocked(enabled: bool, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if !enabled {
        return remove_remembered_key(&state.remembered_key_path);
    }

    #[cfg(windows)]
    {
        let guard = lock_state(&state)?;
        let vault = guard
            .as_ref()
            .ok_or_else(|| "Unlock the vault before enabling persistent unlock".to_string())?;
        save_remembered_key(&state.remembered_key_path, &vault.key)
    }

    #[cfg(not(windows))]
    {
        Err("Persistent unlock is available only on Windows".to_string())
    }
}

#[tauri::command]
fn lock_vault(state: tauri::State<'_, AppState>) -> Result<(), String> {
    remove_remembered_key(&state.remembered_key_path)?;
    *lock_state(&state)? = None;
    Ok(())
}

#[tauri::command]
fn get_entries(state: tauri::State<'_, AppState>) -> Result<Vec<VaultEntry>, String> {
    let guard = lock_state(&state)?;
    let vault = guard
        .as_ref()
        .ok_or_else(|| "Unlock the vault first".to_string())?;
    Ok(vault.data.entries.clone())
}

#[tauri::command]
fn save_entry(
    mut entry: VaultEntry,
    state: tauri::State<'_, AppState>,
) -> Result<VaultEntry, String> {
    if entry.title.trim().is_empty() {
        return Err("Give this item a name first".to_string());
    }
    if entry.id.is_empty() {
        entry.id = Uuid::new_v4().to_string();
    }
    if entry.kind.is_empty() {
        entry.kind = "login".to_string();
    }

    let mut guard = lock_state(&state)?;
    let vault = guard
        .as_mut()
        .ok_or_else(|| "Unlock the vault first".to_string())?;
    if let Some(index) = vault
        .data
        .entries
        .iter()
        .position(|saved| saved.id == entry.id)
    {
        vault.data.entries[index] = entry.clone();
    } else {
        vault.data.entries.push(entry.clone());
    }
    persist_unlocked(&state, vault)?;
    Ok(entry)
}

#[tauri::command]
fn delete_entry(entry_id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = lock_state(&state)?;
    let vault = guard
        .as_mut()
        .ok_or_else(|| "Unlock the vault first".to_string())?;
    vault.data.entries.retain(|entry| entry.id != entry_id);
    persist_unlocked(&state, vault)
}

#[tauri::command]
fn preview_csv(
    contents: String,
    existing_entries: Vec<VaultEntry>,
) -> Result<ImportPreview, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(contents.as_bytes());
    let headers = reader
        .headers()
        .map_err(|_| "Could not read CSV column headers".to_string())?
        .clone();
    let columns: HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(index, name)| (normalize_header(name), index))
        .collect();
    let title_column = find_column(&columns, &["name", "title", "item"]);
    if title_column.is_none() {
        return Err("This CSV does not have a recognizable name or title column".to_string());
    }

    let known_columns = [
        "name",
        "title",
        "item",
        "username",
        "user",
        "login",
        "email",
        "emailaddress",
        "password",
        "pass",
        "url",
        "website",
        "webaddress",
        "note",
        "notes",
        "description",
        "totp",
        "totpsecret",
        "otp",
        "authenticator",
        "favorite",
        "favourite",
        "type",
        "itemtype",
        "vault",
        "folder",
        "id",
        "itemid",
        "createtime",
        "modifytime",
        "createdat",
        "updatedat",
    ];
    let unsupported: Vec<String> = columns
        .keys()
        .filter(|header| !known_columns.contains(&header.as_str()))
        .cloned()
        .collect();
    let mut warnings = Vec::new();
    if !unsupported.is_empty() {
        warnings.push(format!(
            "Unrecognized columns will not be imported: {}",
            unsupported.join(", ")
        ));
    }
    let mut known_signatures = HashSet::new();
    let mut existing_by_signature = HashMap::new();
    for entry in &existing_entries {
        for signature in entry_signatures(entry) {
            known_signatures.insert(signature.clone());
            let matching_ids = existing_by_signature
                .entry(signature)
                .or_insert_with(Vec::new);
            if !matching_ids.contains(&entry.id) {
                matching_ids.push(entry.id.clone());
            }
        }
    }
    let mut entries = Vec::new();
    let mut duplicate_ids = Vec::new();
    let mut duplicate_matches = Vec::new();
    let mut skipped = 0;
    for (row_index, result) in reader.records().enumerate() {
        let record = match result {
            Ok(record) => record,
            Err(_) => {
                skipped += 1;
                warnings.push(format!(
                    "Row {} could not be read and was skipped",
                    row_index + 2
                ));
                continue;
            }
        };
        let value = |aliases: &[&str]| -> String {
            find_column(&columns, aliases)
                .and_then(|index| record.get(index))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let title = value(&["name", "title", "item"]);
        let username = value(&["username", "user", "login"]);
        let email = value(&["email", "emailaddress"]);
        let password = value(&["password", "pass"]);
        let url = value(&["url", "website", "webaddress"]);
        let notes = value(&["note", "notes", "description"]);
        let totp_secret = value(&["totp", "totpsecret", "otp", "authenticator"]);
        let kind_value = value(&["type", "itemtype"])
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        let source_vault = value(&["vault"]);
        let source_created_at = value(&["createtime", "createdat"]);
        let source_modified_at = value(&["modifytime", "modifiedat", "updatedat"]);
        let favorite = matches!(
            value(&["favorite", "favourite"])
                .to_ascii_lowercase()
                .as_str(),
            "true" | "1" | "yes"
        );

        if title.is_empty()
            && username.is_empty()
            && email.is_empty()
            && password.is_empty()
            && url.is_empty()
            && notes.is_empty()
            && totp_secret.is_empty()
        {
            skipped += 1;
            continue;
        }
        if title.is_empty() {
            warnings.push(format!(
                "Row {} has no name and will be called Untitled",
                row_index + 2
            ));
        }
        let kind = match kind_value.as_str() {
            "note" => "note",
            "alias" | "email" => "alias",
            "creditcard" => "creditCard",
            "identity" => "identity",
            "custom" => "custom",
            _ => "login",
        };
        let notes = if matches!(kind, "creditCard" | "identity" | "custom") {
            serde_json::from_str::<serde_json::Value>(&notes)
                .ok()
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or(notes)
        } else {
            notes
        };
        let entry = VaultEntry {
            id: Uuid::new_v4().to_string(),
            title: if title.is_empty() {
                "Untitled".to_string()
            } else {
                title
            },
            username,
            email,
            password,
            url,
            notes,
            totp_secret,
            favorite,
            kind: kind.to_string(),
            source_vault,
            source_created_at,
            source_modified_at,
        };
        let signatures = entry_signatures(&entry);
        if signatures
            .iter()
            .any(|signature| known_signatures.contains(signature))
        {
            duplicate_ids.push(entry.id.clone());
            if let Some(existing_id) = signatures.iter().find_map(|signature| {
                existing_by_signature
                    .get(signature)
                    .filter(|matching_ids| matching_ids.len() == 1)
                    .and_then(|matching_ids| matching_ids.first())
            }) {
                duplicate_matches.push(DuplicateMatch {
                    incoming_id: entry.id.clone(),
                    existing_id: existing_id.clone(),
                });
            }
        }
        known_signatures.extend(signatures);
        entries.push(entry);
    }

    Ok(ImportPreview {
        entries,
        duplicate_ids,
        duplicate_matches,
        warnings,
        skipped,
    })
}

fn entry_signatures(entry: &VaultEntry) -> Vec<String> {
    let username = if entry.username.trim().is_empty() {
        entry.email.trim()
    } else {
        entry.username.trim()
    }
    .to_ascii_lowercase();
    let title = entry.title.trim().to_ascii_lowercase();
    let url = entry.url.trim().trim_end_matches('/').to_ascii_lowercase();
    let mut signatures = Vec::new();
    if !username.is_empty() && !title.is_empty() {
        signatures.push(format!("title:{title}|username:{username}"));
    }
    if !title.is_empty() {
        signatures.push(format!("item:{title}|url:{url}"));
    }
    if !url.is_empty() {
        signatures.push(if username.is_empty() {
            format!("url:{url}")
        } else {
            format!("url:{url}|username:{username}")
        });
    }
    signatures
}

fn normalize_header(header: &str) -> String {
    header
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn find_column(columns: &HashMap<String, usize>, aliases: &[&str]) -> Option<usize> {
    aliases
        .iter()
        .find_map(|alias| columns.get(*alias).copied())
}

#[tauri::command]
fn import_entries(
    entries: Vec<VaultEntry>,
    duplicate_ids: Vec<String>,
    duplicate_matches: Vec<DuplicateMatch>,
    mode: String,
    state: tauri::State<'_, AppState>,
) -> Result<ImportResult, String> {
    if !matches!(mode.as_str(), "all" | "skip" | "update") {
        return Err("Invalid import mode".to_string());
    }
    let mut guard = lock_state(&state)?;
    let vault = guard
        .as_mut()
        .ok_or_else(|| "Unlock the vault first".to_string())?;
    let duplicate_ids: HashSet<String> = duplicate_ids.into_iter().collect();
    let matches: HashMap<String, String> = duplicate_matches
        .into_iter()
        .map(|matched| (matched.incoming_id, matched.existing_id))
        .collect();
    let mut result = ImportResult {
        added: 0,
        updated: 0,
        skipped: 0,
    };
    for incoming in entries {
        let is_duplicate = duplicate_ids.contains(&incoming.id);
        if mode == "skip" && is_duplicate {
            result.skipped += 1;
            continue;
        }
        if mode == "update" && is_duplicate {
            if let Some(existing_id) = matches.get(&incoming.id) {
                if let Some(existing) = vault
                    .data
                    .entries
                    .iter_mut()
                    .find(|entry| entry.id == *existing_id)
                {
                    merge_entry(existing, &incoming);
                    result.updated += 1;
                    continue;
                }
            }
            result.skipped += 1;
            continue;
        }
        vault.data.entries.push(incoming);
        result.added += 1;
    }
    persist_unlocked(&state, vault)?;
    Ok(result)
}

fn merge_entry(existing: &mut VaultEntry, incoming: &VaultEntry) {
    if !incoming.title.is_empty() && incoming.title != "Untitled" {
        existing.title.clone_from(&incoming.title);
    }
    for (target, value) in [
        (&mut existing.username, &incoming.username),
        (&mut existing.email, &incoming.email),
        (&mut existing.password, &incoming.password),
        (&mut existing.url, &incoming.url),
        (&mut existing.notes, &incoming.notes),
        (&mut existing.totp_secret, &incoming.totp_secret),
        (&mut existing.source_vault, &incoming.source_vault),
        (&mut existing.source_created_at, &incoming.source_created_at),
        (
            &mut existing.source_modified_at,
            &incoming.source_modified_at,
        ),
    ] {
        if !value.is_empty() {
            target.clone_from(value);
        }
    }
    if !incoming.kind.is_empty() {
        existing.kind.clone_from(&incoming.kind);
    }
    existing.favorite |= incoming.favorite;
}

#[tauri::command]
fn current_totp(secret: String) -> Result<TotpCode, String> {
    let secret = Zeroizing::new(secret);
    let totp = parse_totp(secret.trim())?;
    let code = totp
        .generate_current()
        .map_err(|_| "Could not generate the authenticator code".to_string())?;
    let remaining = totp
        .ttl()
        .map_err(|_| "Could not read authenticator time".to_string())?;
    Ok(TotpCode {
        code,
        remaining,
        step: totp.step,
    })
}

fn parse_totp(value: &str) -> Result<TOTP, String> {
    let value = value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'');
    let mut secret = Zeroizing::new(value.to_string());
    let mut algorithm = TotpAlgorithm::SHA1;
    let mut digits = 6_usize;
    let mut step = 30_u64;

    if let Ok(uri) = Url::parse(value) {
        if uri.scheme() != "otpauth"
            || uri.host_str() != Some("totp")
            || uri.port().is_some()
            || !uri.username().is_empty()
            || uri.password().is_some()
        {
            return Err("Only otpauth://totp authenticator URLs are supported".to_string());
        }

        let mut has_secret = false;
        for (key, parameter) in uri.query_pairs() {
            match key.to_ascii_lowercase().as_str() {
                "secret" => {
                    secret = Zeroizing::new(parameter.into_owned());
                    has_secret = true;
                }
                "algorithm" if parameter != "null" => {
                    algorithm = match parameter.to_ascii_uppercase().as_str() {
                        "SHA1" => TotpAlgorithm::SHA1,
                        "SHA256" => TotpAlgorithm::SHA256,
                        "SHA512" => TotpAlgorithm::SHA512,
                        _ => {
                            return Err(
                                "Authenticator URL uses an unsupported algorithm".to_string()
                            )
                        }
                    };
                }
                "digits" if parameter != "null" => {
                    digits = parameter
                        .parse::<u8>()
                        .map_err(|_| "Authenticator URL has an invalid digit count".to_string())?
                        as usize;
                }
                "period" if parameter != "null" => {
                    step = parameter
                        .parse::<u16>()
                        .map_err(|_| "Authenticator URL has an invalid period".to_string())?
                        as u64;
                }
                _ => {}
            }
        }
        if !has_secret {
            return Err("Authenticator URL is missing its secret parameter".to_string());
        }
    }

    if !(1..=9).contains(&digits) {
        return Err("Authenticator URL digit count must be between 1 and 9".to_string());
    }
    if step == 0 || step > u16::MAX as u64 {
        return Err("Authenticator URL period must be between 1 and 65535 seconds".to_string());
    }
    let decoded = decode_totp_secret(&secret)?;
    Ok(TOTP::new_unchecked(
        algorithm,
        digits,
        1,
        step,
        decoded.to_vec(),
        None,
        "Passvault".to_string(),
    ))
}

fn decode_totp_secret(value: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut normalized = Zeroizing::new(String::with_capacity(value.len()));
    for character in value.trim().chars() {
        if !character.is_whitespace() && character != '-' && character != '_' {
            normalized.push(character.to_ascii_uppercase());
        }
    }
    while normalized.ends_with('=') {
        normalized.pop();
    }
    if normalized.is_empty() {
        return Err("Authenticator setup does not contain a secret".to_string());
    }
    let decoded = data_encoding::BASE32_NOPAD.decode(normalized.as_bytes());
    Ok(Zeroizing::new(
        decoded.unwrap_or_else(|_| normalized.as_bytes().to_vec()),
    ))
}

#[tauri::command]
fn generate_password(length: usize, symbols: bool) -> Result<String, String> {
    if !(12..=128).contains(&length) {
        return Err("Password length must be between 12 and 128".to_string());
    }
    const LOWERCASE: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
    const UPPERCASE: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ";
    const NUMBERS: &[u8] = b"0123456789";
    const SYMBOLS: &[u8] = b"!@#$%^&*";

    let mut alphabet = [LOWERCASE, UPPERCASE, NUMBERS].concat();
    if symbols {
        alphabet.extend_from_slice(SYMBOLS);
    }

    let mut password = Vec::with_capacity(length);
    let mut previous = None;
    let mut required = vec![LOWERCASE, UPPERCASE, NUMBERS];
    if symbols {
        required.push(SYMBOLS);
    }

    for _ in 0..(length - required.len()) {
        let character = random_character(&alphabet, previous)?;
        password.push(character);
        previous = Some(character);
    }
    for category in required {
        let choices = if password
            .iter()
            .any(|character| category.contains(character))
        {
            alphabet.as_slice()
        } else {
            category
        };
        let character = random_character(choices, previous)?;
        password.push(character);
        previous = Some(character);
    }

    String::from_utf8(password).map_err(|_| "Could not build generated password".to_string())
}

fn random_character(characters: &[u8], previous: Option<u8>) -> Result<u8, String> {
    let candidates: Vec<u8> = characters
        .iter()
        .copied()
        .filter(|character| Some(*character) != previous)
        .collect();
    if candidates.is_empty() {
        return Err("Password character rules cannot be satisfied".to_string());
    }
    let index = rand::rngs::OsRng.gen_range(0..candidates.len());
    Ok(candidates[index])
}

#[tauri::command]
fn export_backup(state: tauri::State<'_, AppState>) -> Result<Option<String>, String> {
    let guard = lock_state(&state)?;
    if guard.is_none() {
        return Err("Unlock the vault before making a backup".to_string());
    }
    let Some(destination) = rfd::FileDialog::new()
        .add_filter("Passvault encrypted backup", &["passvault"])
        .set_file_name("passvault-backup.passvault")
        .save_file()
    else {
        return Ok(None);
    };
    fs::copy(&state.vault_path, &destination)
        .map_err(|_| "Could not write the encrypted backup".to_string())?;
    Ok(Some(destination.display().to_string()))
}

#[tauri::command]
fn restore_backup(
    master_password: String,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let master_password = Zeroizing::new(master_password);
    let Some(source) = rfd::FileDialog::new()
        .add_filter("Passvault encrypted backup", &["passvault"])
        .pick_file()
    else {
        return Ok(false);
    };
    let backup = VaultFile::read(&source)?;
    let unlocked = backup.decrypt(&master_password)?;
    remove_remembered_key(&state.remembered_key_path)?;
    backup.persist(&state.vault_path)?;
    *lock_state(&state)? = Some(unlocked);
    Ok(true)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data = app.path().app_local_data_dir()?;
            fs::create_dir_all(&app_data)?;
            app.manage(AppState {
                vault_path: app_data.join("vault.passvault"),
                remembered_key_path: app_data.join("remembered-unlock-key.bin"),
                unlocked: Mutex::new(None),
            });
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                use_rounded_window_corners(&window);
                let corner_window = window.clone();
                window.on_window_event(move |event| {
                    if matches!(
                        event,
                        tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Resized(_)
                    ) {
                        use_rounded_window_corners(&corner_window);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_status,
            create_vault,
            unlock_vault,
            try_auto_unlock_vault,
            set_keep_unlocked,
            lock_vault,
            get_entries,
            save_entry,
            delete_entry,
            preview_csv,
            import_entries,
            current_totp,
            generate_password,
            export_backup,
            restore_backup
        ])
        .run(tauri::generate_context!())
        .expect("Passvault failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn remembered_key_is_protected_for_the_current_windows_user() {
        let key = [0x5a; 32];
        let protected = protect_remembered_key(&key).unwrap();

        assert_ne!(protected, key);
        assert_eq!(*unprotect_remembered_key(&protected).unwrap(), key);
    }

    #[test]
    fn proton_csv_maps_common_login_fields() {
        let preview = preview_csv(
            "name,url,username,password,note,totp,favorite\nExample,https://example.com,me@example.com,p4ss,hello,JBSWY3DPEHPK3PXP,true\n".to_string(),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(preview.entries.len(), 1);
        assert_eq!(preview.entries[0].title, "Example");
        assert_eq!(preview.entries[0].username, "me@example.com");
        assert_eq!(preview.entries[0].password, "p4ss");
        assert_eq!(preview.entries[0].totp_secret, "JBSWY3DPEHPK3PXP");
        assert!(preview.entries[0].favorite);
    }

    #[test]
    fn proton_csv_marks_existing_logins_as_possible_duplicates() {
        let mut existing = VaultEntry::default();
        existing.id = "saved-id".to_string();
        existing.title = "Example".to_string();
        existing.username = "me@example.com".to_string();
        existing.url = "https://example.com".to_string();
        let preview = preview_csv(
            "name,url,username,password\nExample,https://example.com,me@example.com,p4ss\n"
                .to_string(),
            vec![existing],
        )
        .unwrap();

        assert_eq!(preview.duplicate_ids, vec![preview.entries[0].id.clone()]);
        assert_eq!(preview.duplicate_matches.len(), 1);
        assert_eq!(preview.duplicate_matches[0].existing_id, "saved-id");
    }

    #[test]
    fn proton_export_schema_keeps_email_alias_and_other_item_types() {
        let csv = concat!(
            "type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\n",
            "login,Main login,https://example.com,backup@example.com,handle,secret,Login note,JBSWY3DPEHPK3PXP,1700000000,1700000001,Personal\n",
            "alias,Mail alias,,alias@example.com,,,,,1700000000,1700000001,Personal\n",
            "creditCard,Travel card,,,,,\"{\"\"number\"\":\"\"1234\"\"}\",,1700000000,1700000001,Personal\n",
            "note,WiFi,,,,,Router details,,1700000000,1700000001,Personal\n",
        );
        let preview = preview_csv(csv.to_string(), Vec::new()).unwrap();

        assert_eq!(preview.entries.len(), 4);
        let login = &preview.entries[0];
        assert_eq!(login.username, "handle");
        assert_eq!(login.email, "backup@example.com");
        assert_eq!(login.notes, "Login note");
        assert_eq!(login.totp_secret, "JBSWY3DPEHPK3PXP");
        assert_eq!(login.source_vault, "Personal");
        assert_eq!(login.source_created_at, "1700000000");
        assert_eq!(login.source_modified_at, "1700000001");
        assert_eq!(preview.entries[1].kind, "alias");
        assert_eq!(preview.entries[1].email, "alias@example.com");
        assert_eq!(preview.entries[2].kind, "creditCard");
        assert!(preview.entries[2].notes.contains("1234"));
        assert_eq!(preview.entries[3].kind, "note");
        assert_eq!(preview.entries[3].notes, "Router details");
    }

    #[test]
    fn totp_matches_rfc_6238_sha1_vector() {
        let totp = TOTP::new(
            TotpAlgorithm::SHA1,
            6,
            1,
            30,
            b"12345678901234567890".to_vec(),
            None,
            "test".to_string(),
        )
        .unwrap();
        assert_eq!(totp.generate(59), "287082");
    }

    #[test]
    fn totp_accepts_common_short_proton_secrets_and_authenticator_uris() {
        let manual = current_totp("JBSWY3DPEHPK3PXP".to_string()).unwrap();
        let uri = current_totp("otpauth://totp/Example:user?secret=JBSWY3DPEHPK3PXP".to_string())
            .unwrap();
        let custom_uri = current_totp(
            "otpauth://totp/Example:user?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=60"
                .to_string(),
        )
        .unwrap();
        let formatted_secret = current_totp("JBSW-Y3DPEHPK3PX_P".to_string()).unwrap();
        let raw_secret = current_totp("raw-secret_with spaces!".to_string()).unwrap();

        assert_eq!(manual.code.len(), 6);
        assert_eq!(uri.code.len(), 6);
        assert_eq!(custom_uri.code.len(), 8);
        assert_eq!(custom_uri.step, 60);
        assert_eq!(formatted_secret.code.len(), 6);
        assert_eq!(raw_secret.code.len(), 6);
    }

    #[test]
    fn malformed_authenticator_urls_return_a_specific_safe_error() {
        let result = current_totp("otpauth://totp/Example:user?issuer=Example".to_string());

        assert!(matches!(
            result,
            Err(message) if message == "Authenticator URL is missing its secret parameter"
        ));
    }

    #[test]
    fn generated_password_contains_requested_classes_without_adjacent_repeats() {
        for symbols in [false, true] {
            let password = generate_password(48, symbols).unwrap();
            let characters: Vec<char> = password.chars().collect();

            assert_eq!(characters.len(), 48);
            assert!(characters
                .iter()
                .any(|character| character.is_ascii_lowercase()));
            assert!(characters
                .iter()
                .any(|character| character.is_ascii_uppercase()));
            assert!(characters
                .iter()
                .any(|character| character.is_ascii_digit()));
            assert!(characters.windows(2).all(|pair| pair[0] != pair[1]));
            assert_eq!(
                characters
                    .iter()
                    .any(|character| "!@#$%^&*".contains(*character)),
                symbols
            );
        }
    }

    #[test]
    fn encrypted_vault_round_trips_and_rejects_wrong_password() {
        let mut salt = [0_u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let mut entry = VaultEntry::default();
        entry.title = "Example".to_string();
        entry.password = "not-plaintext-on-disk".to_string();
        let data = VaultData {
            entries: vec![entry],
        };
        let file = VaultFile::from_data(&data, &key, &salt).unwrap();
        let serialized = serde_json::to_string(&file).unwrap();

        assert!(!serialized.contains("not-plaintext-on-disk"));
        let unlocked = file.decrypt("correct horse battery staple").unwrap();
        assert_eq!(unlocked.data.entries[0].password, "not-plaintext-on-disk");
        let unlocked_with_key = file.decrypt_with_key(Zeroizing::new(*key)).unwrap();
        assert_eq!(
            unlocked_with_key.data.entries[0].password,
            "not-plaintext-on-disk"
        );
        assert!(file.decrypt("incorrect password").is_err());
    }

    #[test]
    fn older_vault_entries_default_new_import_metadata() {
        let entry: VaultEntry = serde_json::from_str(
            r#"{"id":"old","title":"Old entry","username":"","password":"","url":"","notes":"","totpSecret":"","favorite":false,"kind":"login"}"#,
        )
        .unwrap();

        assert!(entry.email.is_empty());
        assert!(entry.source_vault.is_empty());
        assert!(entry.source_created_at.is_empty());
        assert!(entry.source_modified_at.is_empty());
    }

    #[test]
    fn imported_duplicate_can_refresh_fields_missing_from_an_older_import() {
        let mut existing = VaultEntry::default();
        existing.id = "existing".to_string();
        existing.title = "Mail".to_string();
        existing.url = "https://mail.example".to_string();
        existing.password = "existing-password".to_string();
        let preview = preview_csv(
            "type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\nlogin,Mail,https://mail.example,me@example.com,,,,JBSWY3DPEHPK3PXP,1700000000,1700000001,Personal\n".to_string(),
            vec![existing.clone()],
        )
        .unwrap();

        assert_eq!(preview.duplicate_matches[0].existing_id, "existing");
        let incoming = &preview.entries[0];
        merge_entry(&mut existing, incoming);
        assert_eq!(existing.email, "me@example.com");
        assert_eq!(existing.password, "existing-password");
        assert_eq!(existing.totp_secret, "JBSWY3DPEHPK3PXP");
        assert_eq!(existing.source_vault, "Personal");
    }

    #[test]
    fn ambiguous_duplicate_is_flagged_but_not_auto_matched() {
        let mut first = VaultEntry::default();
        first.id = "first".to_string();
        first.title = "Mail".to_string();
        first.url = "https://mail.example".to_string();
        first.username = "first@example.com".to_string();
        let mut second = first.clone();
        second.id = "second".to_string();
        second.username = "second@example.com".to_string();
        let preview = preview_csv(
            "type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\nlogin,Mail,https://mail.example,new@example.com,,,,JBSWY3DPEHPK3PXP,1700000000,1700000001,Personal\n".to_string(),
            vec![first, second],
        )
        .unwrap();

        assert_eq!(preview.duplicate_ids.len(), 1);
        assert!(preview.duplicate_matches.is_empty());
    }
}
