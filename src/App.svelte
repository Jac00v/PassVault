<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { openUrl } from '@tauri-apps/plugin-opener';

  type Entry = {
    id: string;
    title: string;
    username: string;
    email: string;
    password: string;
    url: string;
    notes: string;
    totpSecret: string;
    favorite: boolean;
    kind: string;
    sourceVault: string;
    sourceCreatedAt: string;
    sourceModifiedAt: string;
  };

  type Preview = { entries: Entry[]; duplicateIds: string[]; duplicateMatches: { incomingId: string; existingId: string }[]; warnings: string[]; skipped: number };
  type ImportResult = { added: number; updated: number; skipped: number };
  type TotpCode = { code: string; remaining: number; step: number };
  type View = 'all' | 'favorites' | 'authenticator' | 'settings';
  type Filter = Exclude<View, 'settings'>;
  type Theme = 'light' | 'dark' | 'oled';
  type SortOrder = 'alphabetical' | 'latest-used';

  const usageStorageKey = 'passvault-entry-usage';
  const filterStorageKey = 'passvault-filter';
  const sortStorageKey = 'passvault-sort-order';

  function readEntryUsage(): Record<string, number> {
    try {
      const stored: unknown = JSON.parse(localStorage.getItem(usageStorageKey) ?? '{}');
      if (!stored || typeof stored !== 'object' || Array.isArray(stored)) return {};
      return Object.fromEntries(Object.entries(stored).filter(([, time]) => typeof time === 'number' && Number.isFinite(time)));
    } catch {
      return {};
    }
  }

  function readTheme(): Theme {
    try {
      const stored = localStorage.getItem('passvault-theme');
      return stored === 'dark' || stored === 'oled' ? stored : 'light';
    }
    catch { return 'light'; }
  }

  function readFilter(): Filter {
    try {
      const stored = localStorage.getItem(filterStorageKey);
      return stored === 'favorites' || stored === 'authenticator' ? stored : 'all';
    } catch {
      return 'all';
    }
  }

  function readSortOrder(): SortOrder {
    try { return localStorage.getItem(sortStorageKey) === 'latest-used' ? 'latest-used' : 'alphabetical'; }
    catch { return 'alphabetical'; }
  }

  let mode: 'loading' | 'create' | 'unlock' | 'ready' = 'loading';
  let theme: Theme = readTheme();
  let keepUnlocked = false;
  let sortOrder: SortOrder = readSortOrder();
  let entryUsage = readEntryUsage();
  let masterPassword = '';
  let confirmPassword = '';
  let error = '';
  let entries: Entry[] = [];
  let selectedId = '';
  let search = '';
  let view: View = readFilter();
  let showPassword = false;
  let copied = '';
  let clipboardValue = '';
  let totpCode = '';
  let totpError = '';
  let remaining = 30;
  let totpStep = 30;
  let editorOpen = false;
  let editing: Entry = blankEntry();
  let editPasswordVisible = false;
  let showMoreLoginFields = false;
  let importPreview: Preview | null = null;
  let importBusy = false;
  let toast = '';
  let toastCentered = false;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let clipboardTimer: ReturnType<typeof setTimeout> | undefined;
  let lastActivity = Date.now();
  let selected: Entry | undefined;
  let filteredEntries: Entry[] = [];
  let emailSuggestions: string[] = [];

  $: if (typeof document !== 'undefined') document.documentElement.dataset.theme = theme;
  $: selected = entries.find((entry) => entry.id === selectedId);
  $: emailSuggestions = [...new Map(entries
    .flatMap((entry) => [entry.email, entry.username])
    .map((email) => email.trim())
    .filter((email) => email.includes('@'))
    .map((email) => [email.toLowerCase(), email])).values()];
  $: filteredEntries = entries
    .filter((entry) => view === 'settings' ? false : view === 'favorites' ? entry.favorite : view === 'authenticator' ? !!entry.totpSecret : true)
    .filter((entry) => `${entry.title} ${entry.username} ${entry.email} ${entry.url} ${entry.sourceVault}`.toLowerCase().includes(search.toLowerCase()))
    .sort((a, b) => Number(b.favorite) - Number(a.favorite) || (sortOrder === 'latest-used' ? (entryUsage[b.id] ?? 0) - (entryUsage[a.id] ?? 0) : 0) || a.title.localeCompare(b.title));

  function blankEntry(): Entry {
    return { id: '', title: '', username: '', email: '', password: '', url: '', notes: '', totpSecret: '', favorite: false, kind: 'login', sourceVault: '', sourceCreatedAt: '', sourceModifiedAt: '' };
  }

  onMount(() => {
    void checkVault();
    let unlistenClose: (() => void) | undefined;
    void getCurrentWindow().onCloseRequested(() => {
      void clearSecretClipboard();
    }).then((unlisten) => unlistenClose = unlisten);
    const tick = window.setInterval(() => {
      remaining = totpStep - (Math.floor(Date.now() / 1000) % totpStep);
      if (remaining === totpStep && selected?.totpSecret) void refreshTotp(selected.totpSecret);
      if (mode === 'ready' && !keepUnlocked && Date.now() - lastActivity > 15 * 60 * 1000) void lockNow();
    }, 1000);
    const activity = () => { lastActivity = Date.now(); };
    window.addEventListener('pointerdown', activity);
    window.addEventListener('keydown', activity);
    return () => {
      window.clearInterval(tick);
      window.removeEventListener('pointerdown', activity);
      window.removeEventListener('keydown', activity);
      unlistenClose?.();
      if (clipboardTimer) clearTimeout(clipboardTimer);
      if (toastTimer) clearTimeout(toastTimer);
    };
  });

  async function checkVault() {
    try {
      if (!await invoke<boolean>('vault_status')) {
        mode = 'create';
        return;
      }
      if (await invoke<boolean>('try_auto_unlock_vault')) {
        await loadEntries();
        keepUnlocked = true;
        mode = 'ready';
        lastActivity = Date.now();
        return;
      }
      mode = 'unlock';
    } catch (cause) {
      error = errorMessage(cause);
      mode = 'unlock';
    }
  }

  async function submitVault(event: SubmitEvent) {
    event.preventDefault();
    error = '';
    if (mode === 'create') {
      if (masterPassword.length < 12) {
        error = 'Use at least 12 characters for your master password.';
        return;
      }
      if (masterPassword !== confirmPassword) {
        error = 'Those master passwords do not match.';
        return;
      }
    }
    try {
      await invoke(mode === 'create' ? 'create_vault' : 'unlock_vault', { masterPassword });
      masterPassword = '';
      confirmPassword = '';
      await loadEntries();
      mode = 'ready';
      error = '';
      lastActivity = Date.now();
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function loadEntries() {
    entries = await invoke<Entry[]>('get_entries');
    selectedId = entries[0]?.id ?? '';
    if (entries[0]?.totpSecret) await refreshTotp(entries[0].totpSecret);
  }

  async function selectEntry(entry: Entry) {
    selectedId = entry.id;
    showPassword = false;
    totpError = '';
    totpCode = '';
    if (entry.totpSecret) await refreshTotp(entry.totpSecret);
  }

  function backToList() {
    selectedId = '';
    totpCode = '';
    showPassword = false;
  }

  function setTheme(nextTheme: Theme) {
    theme = nextTheme;
    try { localStorage.setItem('passvault-theme', nextTheme); } catch { /* Theme preference is optional. */ }
  }

  async function setKeepUnlocked(enabled: boolean) {
    error = '';
    try {
      await invoke('set_keep_unlocked', { enabled });
      keepUnlocked = enabled;
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  function setView(nextView: View) {
    view = nextView;
    if (nextView !== 'settings') {
      try { localStorage.setItem(filterStorageKey, nextView); } catch { /* Filter preference is optional. */ }
    }
  }

  function setSortOrder(nextSortOrder: SortOrder) {
    sortOrder = nextSortOrder;
    try { localStorage.setItem(sortStorageKey, nextSortOrder); } catch { /* Sort preference is optional. */ }
  }

  async function refreshTotp(secret: string) {
    try {
      const result = await invoke<TotpCode>('current_totp', { secret });
      totpCode = result.code;
      remaining = result.remaining;
      totpStep = result.step;
      totpError = '';
    } catch (cause) {
      totpCode = '';
      totpError = errorMessage(cause);
    }
  }

  async function lockNow() {
    if (mode !== 'ready') return;
    await clearSecretClipboard();
    try {
      await invoke('lock_vault');
      keepUnlocked = false;
    } catch (cause) {
      error = errorMessage(cause);
      return;
    }
    entries = [];
    selectedId = '';
    editing = blankEntry();
    editorOpen = false;
    importPreview = null;
    totpCode = '';
    masterPassword = '';
    mode = 'unlock';
  }

  function openNewEntry() {
    editing = blankEntry();
    editPasswordVisible = false;
    showMoreLoginFields = false;
    editorOpen = true;
    error = '';
  }

  function openEditor(entry: Entry) {
    editing = { ...entry };
    editPasswordVisible = false;
    showMoreLoginFields = true;
    editorOpen = true;
    error = '';
  }

  function setLoginIdentifier(value: string) {
    if (editing.email && !editing.username) {
      editing.email = value;
    } else if (!editing.username && !editing.email && value.includes('@')) {
      editing.email = value;
    } else {
      editing.username = value;
    }
  }

  async function saveEntry(event: SubmitEvent) {
    event.preventDefault();
    error = '';
    try {
      const wasEditing = !!editing.id;
      const saved = await invoke<Entry>('save_entry', { entry: editing });
      entries = [...entries.filter((entry) => entry.id !== saved.id), saved];
      if (wasEditing) markEntryUsed(saved.id);
      selectedId = saved.id;
      editorOpen = false;
      editPasswordVisible = false;
      showPassword = false;
      if (saved.totpSecret) await refreshTotp(saved.totpSecret);
      showToast('Item saved');
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function removeEntry(entry: Entry) {
    if (!window.confirm(`Delete ${entry.title}? This cannot be undone.`)) return;
    try {
      await invoke('delete_entry', { entryId: entry.id });
      entries = entries.filter((saved) => saved.id !== entry.id);
      const remainingUsage = { ...entryUsage };
      delete remainingUsage[entry.id];
      entryUsage = remainingUsage;
      try { localStorage.setItem(usageStorageKey, JSON.stringify(entryUsage)); } catch { /* Usage sorting is optional. */ }
      selectedId = entries[0]?.id ?? '';
      showToast('Item deleted');
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function generateIntoEntry() {
    try {
      editing.password = await invoke<string>('generate_password', { length: 20, symbols: true });
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function toggleFavorite(entry: Entry) {
    try {
      const changed = await invoke<Entry>('save_entry', { entry: { ...entry, favorite: !entry.favorite } });
      entries = entries.map((saved) => saved.id === changed.id ? changed : saved);
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  function markEntryUsed(entryId: string) {
    entryUsage = { ...entryUsage, [entryId]: Math.max(Date.now(), (entryUsage[entryId] ?? 0) + 1) };
    try { localStorage.setItem(usageStorageKey, JSON.stringify(entryUsage)); } catch { /* Usage sorting is optional. */ }
  }

  async function copySecret(value: string, label: string) {
    const entryId = selected?.id;
    try {
      await navigator.clipboard.writeText(value);
      if (entryId) markEntryUsed(entryId);
      copied = label;
      clipboardValue = value;
      if (clipboardTimer) clearTimeout(clipboardTimer);
      clipboardTimer = setTimeout(() => void clearSecretClipboard(value), 30_000);
      showToast('Copied to clipboard', true);
    } catch {
      showToast('Clipboard access was blocked');
    }
  }

  async function clearSecretClipboard(expected = clipboardValue) {
    if (clipboardTimer) clearTimeout(clipboardTimer);
    clipboardTimer = undefined;
    if (!expected) return;
    try {
      if (await navigator.clipboard.readText() === expected) await navigator.clipboard.writeText('');
    } catch {
      // Clipboard access may be denied after the initial user gesture.
    }
    if (clipboardValue === expected) {
      clipboardValue = '';
      copied = '';
    }
  }

  async function handleCsv(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    error = '';
    try {
      const contents = await file.text();
      importPreview = await invoke<Preview>('preview_csv', { contents, existingEntries: entries });
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function confirmImport(mode: 'all' | 'skip' | 'update' = 'all') {
    if (!importPreview) return;
    importBusy = true;
    try {
      const result = await invoke<ImportResult>('import_entries', {
        entries: importPreview.entries,
        duplicateIds: importPreview.duplicateIds,
        duplicateMatches: importPreview.duplicateMatches,
        mode,
      });
      await loadEntries();
      importPreview = null;
      showToast(`Added ${result.added}, updated ${result.updated}, skipped ${result.skipped}`);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      importBusy = false;
    }
  }

  async function exportBackup() {
    try {
      const path = await invoke<string | null>('export_backup');
      if (path) showToast('Encrypted backup saved');
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function restoreBackup() {
    const password = window.prompt('Enter the master password used for this backup. Restoring replaces this device’s current vault.');
    if (!password) return;
    try {
      const restored = await invoke<boolean>('restore_backup', { masterPassword: password });
      if (restored) {
        keepUnlocked = false;
        await loadEntries();
        showToast('Backup restored');
      }
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  function showToast(message: string, centered = false) {
    toast = message;
    toastCentered = centered;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toast = '', centered ? 3000 : 2600);
  }

  function errorMessage(cause: unknown): string {
    return typeof cause === 'string' ? cause : cause instanceof Error ? cause.message : 'Something went wrong.';
  }

  function initials(title: string): string {
    return title.trim().slice(0, 1).toUpperCase() || '•';
  }

  function itemKindLabel(kind: string, hasTotp = false): string {
    const labels: Record<string, string> = {
      login: 'Login',
      note: 'Secure note',
      alias: 'Email alias',
      creditCard: 'Credit card',
      identity: 'Identity',
      custom: 'Custom item',
    };
    const label = labels[kind] ?? 'Imported item';
    return kind === 'login' && hasTotp ? `${label} · Authenticator enabled` : label;
  }

  function itemIcon(entry: Entry): string {
    if (entry.kind === 'note') return '▧';
    if (entry.kind === 'alias') return '@';
    if (entry.kind === 'creditCard') return '$';
    if (entry.kind === 'identity') return 'ID';
    if (entry.totpSecret) return '◷';
    return initials(entry.title);
  }

  function protonDate(value: string): string {
    if (!value) return '';
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return value;
    const milliseconds = numeric > 1_000_000_000_000 ? numeric : numeric * 1000;
    return new Date(milliseconds).toLocaleString();
  }

  function externalUrl(value: string): string | null {
    try {
      const url = new URL(value.includes('://') ? value : `https://${value}`);
      return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null;
    } catch {
      return null;
    }
  }
</script>

<svg class="svg-definitions" aria-hidden="true" focusable="false">
  <filter id="glass-refraction" x="-12%" y="-12%" width="124%" height="124%" color-interpolation-filters="sRGB">
    <feTurbulence type="fractalNoise" baseFrequency="0.012 0.018" numOctaves="1" seed="8" result="surface" />
    <feDisplacementMap in="SourceGraphic" in2="surface" scale="2.2" xChannelSelector="R" yChannelSelector="G" />
  </filter>
</svg>

<div class="app-root">
{#if mode === 'loading'}
  <main class="gate"><div class="brand-mark">p</div><p class="muted">Opening your vault…</p></main>
{:else if mode === 'create' || mode === 'unlock'}
  <main class="gate">
    <div class="gate-card">
      <div class="brand-lockup"><span class="brand-mark">p</span><span>passvault</span></div>
      <span class="eyebrow">PRIVATE BY DESIGN</span>
      <h1>{mode === 'create' ? 'Your passwords, under your control.' : 'Welcome back.'}</h1>
      <p class="gate-copy">{mode === 'create' ? 'Create a strong master password. It encrypts your vault and is never sent anywhere.' : 'Unlock your encrypted vault. Your data stays on this device.'}</p>
      <form onsubmit={submitVault} class="gate-form">
        <label for="master-password">Master password</label>
        <input id="master-password" type="password" bind:value={masterPassword} autocomplete={mode === 'create' ? 'new-password' : 'current-password'} required />
        {#if mode === 'create'}
          <label for="confirm-password">Confirm master password</label>
          <input id="confirm-password" type="password" bind:value={confirmPassword} autocomplete="new-password" required />
          <p class="field-hint">At least 12 characters. There is no password reset.</p>
        {/if}
        {#if error}<p class="error-message">{error}</p>{/if}
        <button class="primary-button full-width" type="submit">{mode === 'create' ? 'Create encrypted vault' : 'Unlock vault'} <span aria-hidden="true">↗</span></button>
      </form>
      <div class="local-note"><span class="status-dot"></span>Stored only on this Windows device</div>
    </div>
    <div class="gate-foot">Local vault · End-to-end encrypted at rest · No account required</div>
  </main>
{:else}
  <main class="app-shell" class:settings-mode={view === 'settings'}>
    <aside class="sidebar">
      <div class="brand-lockup sidebar-brand"><span class="brand-mark">p</span><span class="brand-name">passvault</span></div>
      <div class="search-box">
        <span class="search-icon">⌕</span>
        <input bind:value={search} placeholder="Search vault" aria-label="Search vault" />
        <span class="search-shortcut">/</span>
      </div>
      <div class="nav-label">YOUR VAULT</div>
      <nav class="primary-nav" aria-label="Vault views">
        <button class:active={view === 'all'} title="All items" onclick={() => setView('all')}><span class="nav-icon">▤</span><span class="nav-text">All items</span><span class="nav-count">{entries.length}</span></button>
        <button class:active={view === 'favorites'} title="Favorites" onclick={() => setView('favorites')}><span class="nav-icon">☆</span><span class="nav-text">Favorites</span></button>
        <button class:active={view === 'authenticator'} title="Authenticator" onclick={() => setView('authenticator')}><span class="nav-icon">◷</span><span class="nav-text">Authenticator</span></button>
        <button class:active={view === 'settings'} title="Settings" onclick={() => setView('settings')}><span class="nav-icon">⚙</span><span class="nav-text">Settings</span></button>
      </nav>
      <div class="sidebar-bottom">
        <button class="side-action" title="Export encrypted backup" onclick={exportBackup}><span>↓</span><span class="side-text">Export encrypted backup</span></button>
        <button class="side-action" title="Restore from backup" onclick={restoreBackup}><span>↥</span><span class="side-text">Restore from backup</span></button>
        <div class="sidebar-footer">
          <span class="avatar">P</span>
          <span class="footer-copy"><strong>On this device</strong><small>Vault is unlocked</small></span>
          <button class="icon-button lock-button" aria-label="Lock vault" title="Lock vault" onclick={lockNow}>⌑</button>
        </div>
      </div>
    </aside>

    <section class="list-pane" aria-label="Vault items">
      <header class="list-header">
        <div><span class="eyebrow">YOUR PASSWORDS</span><h1>{view === 'favorites' ? 'Favorites' : view === 'authenticator' ? 'Authenticator' : 'All items'}</h1></div>
        <button class="add-button" aria-label="Add item" title="Add item" onclick={openNewEntry}>+</button>
      </header>
      <div class="list-caption">
        <span>{filteredEntries.length} {filteredEntries.length === 1 ? 'ITEM' : 'ITEMS'}</span>
        <div class="sort-toggle" role="group" aria-label="Sort items">
          <button class:active={sortOrder === 'alphabetical'} aria-pressed={sortOrder === 'alphabetical'} onclick={() => setSortOrder('alphabetical')}>A-Z</button>
          <button class:active={sortOrder === 'latest-used'} aria-pressed={sortOrder === 'latest-used'} onclick={() => setSortOrder('latest-used')}>Latest used</button>
        </div>
      </div>
      <div class="entry-list">
        {#each filteredEntries as entry (entry.id)}
          <button class="entry-row" class:selected={selectedId === entry.id} onclick={() => selectEntry(entry)}>
            <span class="entry-icon" class:note-icon={entry.kind === 'note' || entry.kind === 'alias'}>{itemIcon(entry)}</span>
            <span class="entry-copy"><strong>{entry.title}</strong><small>{entry.username || entry.email || entry.url || itemKindLabel(entry.kind, !!entry.totpSecret)}</small></span>
            {#if entry.favorite}<span class="favorite-star" aria-label="Favorite">★</span>{/if}
          </button>
        {:else}
          <div class="empty-list"><span class="empty-symbol">◇</span><strong>{search ? 'No matches found' : 'Your vault is ready'}</strong><p>{search ? 'Try a different search.' : 'Import a Proton Pass CSV from Settings or add your first login.'}</p></div>
        {/each}
      </div>
      <div class="list-bottom"><span class="status-dot"></span>Encrypted on this device</div>
    </section>

    <section class="detail-pane">
      {#if view === 'settings'}
        <div class="settings-screen">
          <header class="settings-heading">
            <span class="eyebrow">PERSONALIZE YOUR VAULT</span>
            <h2>Settings</h2>
            <p>Make Passvault feel like yours. Your appearance preference stays on this device.</p>
          </header>
          <section class="settings-section" aria-labelledby="appearance-title">
            <div class="settings-section-heading">
              <div><h3 id="appearance-title">Appearance</h3><p>Choose a theme for the vault interface.</p></div>
              <span class="settings-section-icon">◐</span>
            </div>
            <div class="theme-options" role="group" aria-label="Appearance theme">
              <button class="theme-choice" class:chosen={theme === 'light'} aria-pressed={theme === 'light'} onclick={() => setTheme('light')}>
                <span class="theme-preview theme-preview-light"><i></i><i></i><i></i></span>
                <span class="theme-choice-copy"><strong>Light</strong><small>Soft, bright glass</small></span>
                <span class="theme-check">{theme === 'light' ? '✓' : ''}</span>
              </button>
              <button class="theme-choice" class:chosen={theme === 'dark'} aria-pressed={theme === 'dark'} onclick={() => setTheme('dark')}>
                <span class="theme-preview theme-preview-dark"><i></i><i></i><i></i></span>
                <span class="theme-choice-copy"><strong>Dark</strong><small>Low-light glass</small></span>
                <span class="theme-check">{theme === 'dark' ? '✓' : ''}</span>
              </button>
              <button class="theme-choice" class:chosen={theme === 'oled'} aria-pressed={theme === 'oled'} onclick={() => setTheme('oled')}>
                <span class="theme-preview theme-preview-oled"><i></i><i></i><i></i></span>
                <span class="theme-choice-copy"><strong>OLED</strong><small>Pure black surfaces</small></span>
                <span class="theme-check">{theme === 'oled' ? '✓' : ''}</span>
              </button>
            </div>
          </section>
          <section class="settings-section" aria-labelledby="unlock-title">
            <div class="settings-section-heading">
              <div><h3 id="unlock-title">Unlock behavior</h3><p>Choose when Passvault asks for your master password.</p></div>
              <span class="settings-section-icon">⌑</span>
            </div>
            <label class="keep-unlocked-choice">
              <input type="checkbox" checked={keepUnlocked} onchange={(event) => void setKeepUnlocked(event.currentTarget.checked)} />
              <span class="keep-unlocked-copy"><strong>Keep unlocked between launches</strong><small>Use your Windows account to protect the vault key and unlock automatically at startup.</small></span>
            </label>
            <p class="unlock-note">While enabled, inactivity will not lock the vault. Anyone with access to your Windows account can open it. Use the Lock button to lock now and turn this setting off.</p>
          </section>
          <section class="settings-section" aria-labelledby="import-title">
            <div class="settings-section-heading">
              <div><h3 id="import-title">Import data</h3><p>Bring your saved logins into Passvault.</p></div>
              <span class="settings-section-icon">↥</span>
            </div>
            <label class="import-button refractive-glass">
              <span class="import-icon">↥</span>
              <span><strong>Import from Proton Pass</strong><small>Select your Proton Pass CSV export</small></span>
              <span class="import-arrow">→</span>
              <input type="file" accept=".csv,text/csv" onchange={handleCsv} />
            </label>
          </section>
          <p class="settings-note"><span class="status-dot"></span>Theme changes never affect vault encryption or stored passwords.</p>
        </div>
      {:else if selected}
        <header class="detail-toolbar">
          <button class="mobile-back" aria-label="Back to item list" onclick={backToList}>‹</button>
          <div class="breadcrumb">All items <span>/</span> <strong>{selected.title}</strong></div>
          <div class="toolbar-actions">
            <button class="icon-button" title="Edit item" aria-label="Edit item" onclick={() => openEditor(selected!)}>✎</button>
            <button class="icon-button danger-hover" title="Delete item" aria-label="Delete item" onclick={() => removeEntry(selected!)}>⌫</button>
          </div>
        </header>
        <div class="detail-content">
          <div class="item-heading">
            <span class="detail-item-icon">{itemIcon(selected)}</span>
            <div><h2>{selected.title}</h2><p>{itemKindLabel(selected.kind, !!selected.totpSecret)}</p></div>
            <button class="favorite-toggle" class:marked={selected.favorite} aria-label={selected.favorite ? 'Remove favorite' : 'Add favorite'} title="Toggle favorite" onclick={() => toggleFavorite(selected!)}>{selected.favorite ? '★' : '☆'}</button>
          </div>

          {#if selected.totpSecret}
            <div class="totp-card refractive-glass">
              <span class="totp-icon">◷</span>
              <div class="totp-content">
                <div class="totp-heading"><span class="totp-label">2FA TOKEN (TOTP)</span><span class="totp-live"><i></i>LIVE</span></div>
                {#if totpCode}
                  <div class="totp-code-row"><strong class="totp-code">{totpCode.slice(0, Math.floor(totpCode.length / 2))} {totpCode.slice(Math.floor(totpCode.length / 2))}</strong><button class="copy-code" onclick={() => copySecret(totpCode, 'code')}>{copied === 'code' ? 'Copied' : 'Copy'}</button></div>
                  <p class="totp-caption">Refreshes in {remaining}s</p>
                {:else}<p class="totp-error">{totpError || 'Reading authenticator…'}</p>{/if}
              </div>
              <span class="totp-ring" style={`--progress: ${(remaining / totpStep) * 100}%`} aria-label={`Code refreshes in ${remaining} seconds`}><span>{remaining}</span></span>
            </div>
          {/if}

          <div class="detail-fields">
            {#if selected.username}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy username" onclick={() => copySecret(selected!.username, 'username')}></button><div class="field-top"><span>USERNAME</span></div><p class="detail-value">{selected.username}</p></div>{/if}
            {#if selected.email}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy email" onclick={() => copySecret(selected!.email, 'email')}></button><div class="field-top"><span>EMAIL</span></div><p class="detail-value">{selected.email}</p></div>{/if}
            {#if selected.password}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy password" onclick={() => copySecret(selected!.password, 'password')}></button><div class="field-top"><span>PASSWORD</span><div class="field-actions"><button onclick={() => showPassword = !showPassword}>{showPassword ? 'Hide' : 'Reveal'}</button></div></div><p class="detail-value secret-value">{showPassword ? selected.password : '••••••••••••••••'}</p></div>{/if}
            {#if selected.url}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy website address" onclick={() => copySecret(selected!.url, 'url')}></button><div class="field-top"><span>WEBSITE</span><button title="Open website" onclick={() => { const url = externalUrl(selected!.url); if (url) void openUrl(url); }}>Open ↗</button></div><p class="detail-value">{selected.url}</p></div>{/if}
            {#if selected.notes}<div class="detail-field notes-field"><button class="field-copy-surface" aria-label="Copy notes" onclick={() => copySecret(selected!.notes, 'notes')}></button><div class="field-top"><span>NOTES</span></div><p class="detail-value">{selected.notes}</p></div>{/if}
            {#if selected.sourceVault}<div class="detail-field"><div class="field-top"><span>PROTON VAULT</span></div><p>{selected.sourceVault}</p></div>{/if}
            {#if selected.sourceCreatedAt}<div class="detail-field"><div class="field-top"><span>ORIGINAL CREATION DATE</span></div><p>{protonDate(selected.sourceCreatedAt)}</p></div>{/if}
            {#if selected.sourceModifiedAt}<div class="detail-field"><div class="field-top"><span>LAST MODIFIED IN PROTON</span></div><p>{protonDate(selected.sourceModifiedAt)}</p></div>{/if}
            {#if !selected.username && !selected.email && !selected.password && !selected.url && !selected.notes && !selected.totpSecret && !selected.sourceVault && !selected.sourceCreatedAt && !selected.sourceModifiedAt}<div class="detail-empty">Nothing saved in this item yet.</div>{/if}
          </div>
          <div class="detail-security"><span class="security-check">✓</span><span>Encrypted and stored locally</span><span class="detail-separator">·</span><span>Auto-locks after 15 min</span></div>
        </div>
      {:else}
        <div class="welcome-detail"><div class="welcome-glyph">p</div><span class="eyebrow">YOUR DIGITAL KEYS, ORGANIZED</span><h2>A calmer place<br />for your passwords.</h2><p>Select an item to view it, or start by adding your first login.</p><button class="primary-button" onclick={openNewEntry}>Add your first item <span>↗</span></button></div>
      {/if}
    </section>

    {#if error}<div class="app-error" role="alert"><span>{error}</span><button aria-label="Dismiss error" onclick={() => error = ''}>×</button></div>{/if}
    {#if toast}<div class="toast" class:copy-toast={toastCentered} role="status">{toast}</div>{/if}
  </main>
{/if}

{#if editorOpen}
  <div class="modal-backdrop" class:login-backdrop={editing.kind === 'login'} role="presentation" onclick={(event) => { if (event.target === event.currentTarget) editorOpen = false; }}>
    <datalist id="saved-email-suggestions">
      {#each emailSuggestions as email}<option value={email}></option>{/each}
    </datalist>
    {#if editing.kind === 'login'}
      <div class="modal editor-modal login-modal" role="dialog" aria-modal="true" aria-labelledby="editor-title" tabindex="-1">
        <form onsubmit={saveEntry} class="editor-form login-composer">
          <header class="login-topbar">
            <button type="button" class="login-close" aria-label="Close" onclick={() => editorOpen = false}>×</button>
            <button type="submit" class="primary-button login-submit">{editing.id ? 'Save login' : 'Create login'}</button>
          </header>
          <label class="login-title-field"><span id="editor-title">Title</span><input bind:value={editing.title} placeholder="Untitled" required /></label>
          <section class="login-credentials" aria-label="Login credentials">
            <label class="login-row"><span class="login-row-icon" aria-hidden="true">♙</span><span class="login-row-content"><strong>Email or username</strong><input value={editing.username || editing.email} oninput={(event) => setLoginIdentifier(event.currentTarget.value)} autocomplete="username" list="saved-email-suggestions" placeholder="Enter email or username" /></span></label>
            <label class="login-row"><span class="login-row-icon" aria-hidden="true">⚿</span><span class="login-row-content"><strong>Password</strong><span class="login-password-control"><input bind:value={editing.password} type={editPasswordVisible ? 'text' : 'password'} autocomplete="new-password" placeholder="Enter password" /><button type="button" class="login-round-action" aria-label="Generate password" title="Generate password" onclick={generateIntoEntry}>⟳</button><button type="button" class="login-inline-action" onclick={() => editPasswordVisible = !editPasswordVisible}>{editPasswordVisible ? 'Hide' : 'Show'}</button></span></span></label>
            <label class="login-row"><span class="login-row-icon" aria-hidden="true">♙</span><span class="login-row-content"><strong>2FA secret key (TOTP)</strong><input bind:value={editing.totpSecret} autocomplete="off" placeholder="Add 2FA secret key" /><small>Codes are generated locally from this key.</small></span></label>
          </section>
          <section class="login-detail-card">
            <label class="login-row login-card-row"><span class="login-row-icon" aria-hidden="true">◎</span><span class="login-row-content"><strong>Websites</strong><input bind:value={editing.url} placeholder="https://" autocomplete="url" /></span></label>
          </section>
          <section class="login-detail-card login-note-card">
            <label class="login-row login-card-row"><span class="login-row-icon" aria-hidden="true">▱</span><span class="login-row-content"><strong>Note</strong><textarea bind:value={editing.notes} rows="2" placeholder="Add note"></textarea></span></label>
          </section>
          <button type="button" class="login-add-more" aria-expanded={showMoreLoginFields} onclick={() => showMoreLoginFields = !showMoreLoginFields}>{showMoreLoginFields ? '− Show less' : '+ Add more'}</button>
          {#if showMoreLoginFields}
            <section class="login-more-fields">
              <label>Item type<select bind:value={editing.kind}><option value="login">Login</option><option value="note">Secure note</option><option value="alias">Email alias</option><option value="creditCard">Credit card</option><option value="identity">Identity</option><option value="custom">Custom item</option></select></label>
              <label class="favorite-check"><input type="checkbox" bind:checked={editing.favorite} /> Add to favorites</label>
              {#if editing.sourceVault}<label>Source Proton vault<input bind:value={editing.sourceVault} /></label>{/if}
            </section>
          {/if}
          {#if error}<p class="error-message">{error}</p>{/if}
        </form>
      </div>
    {:else}
      <div class="modal editor-modal" role="dialog" aria-modal="true" aria-labelledby="editor-title" tabindex="-1">
        <header class="modal-header"><div><span class="eyebrow">{editing.id ? 'EDIT VAULT ITEM' : 'NEW VAULT ITEM'}</span><h2 id="editor-title">{editing.id ? 'Edit item' : 'Add an item'}</h2><p class="editor-subtitle">Keep important details together in your vault.</p></div><button class="icon-button" aria-label="Close" onclick={() => editorOpen = false}>×</button></header>
        <form onsubmit={saveEntry} class="editor-form">
          <div class="editor-basics">
            <label>Item name<input bind:value={editing.title} placeholder="e.g. Personal email" required /></label>
            <label>Item type<select bind:value={editing.kind}><option value="login">Login</option><option value="note">Secure note</option><option value="alias">Email alias</option><option value="creditCard">Credit card</option><option value="identity">Identity</option><option value="custom">Custom item</option></select></label>
          </div>
          {#if editing.kind === 'alias'}
            <label>Email address<input bind:value={editing.email} autocomplete="off" list="saved-email-suggestions" /></label>
            <label>Website<input bind:value={editing.url} placeholder="https://" /></label>
          {:else if editing.kind !== 'note'}
            <p class="field-hint">Imported fields are preserved in the notes below.</p>
          {/if}
          <label class="editor-notes">Notes<textarea bind:value={editing.notes} rows="3" placeholder="Add any helpful details…"></textarea></label>
          {#if editing.sourceVault}<label>Source Proton vault<input bind:value={editing.sourceVault} /></label>{/if}
          <label class="favorite-check"><input type="checkbox" bind:checked={editing.favorite} /> Add to favorites</label>
          {#if error}<p class="error-message">{error}</p>{/if}
          <footer class="modal-footer"><button type="button" class="secondary-button" onclick={() => editorOpen = false}>Cancel</button><button class="primary-button" type="submit">Save item <span>↗</span></button></footer>
        </form>
      </div>
    {/if}
  </div>
{/if}

{#if importPreview}
  <div class="modal-backdrop" role="presentation" onclick={(event) => { if (event.target === event.currentTarget) importPreview = null; }}>
    <div class="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-title" tabindex="-1">
      <header class="modal-header"><div><span class="eyebrow">LOCAL CSV IMPORT</span><h2 id="import-title">Review your import</h2></div><button class="icon-button" aria-label="Close" onclick={() => importPreview = null}>×</button></header>
      <div class="import-summary"><span class="import-summary-icon">↥</span><div><strong>{importPreview.entries.length} {importPreview.entries.length === 1 ? 'item found' : 'items found'}</strong><p>These entries will be encrypted in your local vault.</p></div></div>
      <div class="preview-list">
        {#each importPreview.entries.slice(0, 8) as entry}
          <div class="preview-entry"><span class="entry-icon">{itemIcon(entry)}</span><div><strong>{entry.title}</strong><small>{entry.username || entry.email || entry.url || itemKindLabel(entry.kind)}</small></div><span class="preview-fields">{importPreview.duplicateIds.includes(entry.id) ? 'Possible duplicate' : [entry.email && 'email', entry.password && 'password', entry.notes && 'notes', entry.totpSecret && '2FA'].filter(Boolean).join(' · ')}</span></div>
        {/each}
        {#if importPreview.entries.length > 8}<p class="more-items">and {importPreview.entries.length - 8} more…</p>{/if}
      </div>
      {#if importPreview.skipped}<p class="import-warning">{importPreview.skipped} empty or unreadable rows will be skipped.</p>{/if}
      {#if importPreview.duplicateIds.length}<p class="import-warning">{importPreview.duplicateIds.length} possible duplicate {importPreview.duplicateIds.length === 1 ? 'item' : 'items'} found. You can refresh matching items with the newly imported fields.</p>{/if}
      {#each importPreview.warnings as warning}<p class="import-warning">{warning}</p>{/each}
      <p class="privacy-note"><span>✓</span> The CSV is processed on this device and is not retained.</p>
      <footer class="modal-footer"><button class="secondary-button" onclick={() => importPreview = null}>Cancel</button>{#if importPreview.duplicateIds.length}<button class="secondary-button" disabled={importBusy} onclick={() => confirmImport('skip')}>{importBusy ? 'Importing…' : 'Skip duplicates'}</button>{#if importPreview.duplicateMatches.length}<button class="secondary-button" disabled={importBusy} onclick={() => confirmImport('update')}>{importBusy ? 'Importing…' : 'Update matches'}</button>{/if}{/if}<button class="primary-button" disabled={importBusy || importPreview.entries.length === 0} onclick={() => confirmImport('all')}>{importBusy ? 'Importing…' : importPreview.duplicateIds.length ? 'Import all' : 'Import items'} <span>↗</span></button></footer>
    </div>
  </div>
{/if}

</div>

<svelte:head><meta name="description" content="A local-first encrypted password manager" /></svelte:head>
