<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { fade, fly } from 'svelte/transition';
  import { flip } from 'svelte/animate';
  import { cubicOut } from 'svelte/easing';
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
    cardNumber: string;
    cardHolder: string;
    cardExpiry: string;
    cardCvv: string;
    cardPin: string;
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
  type PaletteCommand = { id: string; label: string; hint: string; icon: string; run: () => void | Promise<void> };

  const motion = { fast: 150, base: 240, slow: 320 };
  const easeOut = cubicOut;
  const easeSpring = (t: number): number => {
    const c1 = 1.70158;
    const c3 = c1 + 1;
    return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
  };
  const autoLockMs = 15 * 60 * 1000;
  const autoLockWarningMs = 60 * 1000;
  const usageStorageKey = 'passvault-entry-usage';
  const filterStorageKey = 'passvault-filter';
  const sortStorageKey = 'passvault-sort-order';
  const kindLabels: Record<string, string> = {
    login: 'Login',
    note: 'Secure note',
    alias: 'Email alias',
    creditCard: 'Credit card',
    identity: 'Identity',
    custom: 'Custom item',
  };
  const titleCollator = new Intl.Collator(undefined, { sensitivity: 'base' });

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
  let reducedMotion = false;
  let listLoading = false;
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
  let showCardNumber = false;
  let showCardSecrets = false;
  let copied = '';
  let clipboardValue = '';
  let totpCode = '';
  let totpError = '';
  let remaining = 30;
  let totpStep = 30;
  let lastTotpCounter = -1;
  let editorOpen = false;
  let editing: Entry = blankEntry();
  let editPasswordVisible = false;
  let showMoreLoginFields = false;
  let loginIdentifier = '';
  let importPreview: Preview | null = null;
  let importBusy = false;
  let restoreOpen = false;
  let restorePassword = '';
  let restoreBusy = false;
  let locking = false;
  let toast = '';
  let toastCentered = false;
  let toastCheck = false;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let clipboardTimer: ReturnType<typeof setTimeout> | undefined;
  let clipboardDeadline = 0;
  let clipboardRemaining = 0;
  let copyNotice = false;
  let passwordRevealTimer: ReturnType<typeof setTimeout> | undefined;
  let autoLockRemaining = autoLockMs;
  let autoLockPaused = false;
  let autoLockSeconds = 0;
  let lockWarning = false;
  let nativeDialogOpen = false;
  let lastActivity = Date.now();
  let selected: Entry | undefined;
  let filteredEntries: Entry[] = [];
  let emailSuggestions: string[] = [];
  let searchInput: HTMLInputElement | undefined;
  let detailView: HTMLElement | undefined;
  let detailScrollPositions = new Map<string, number>();
  let commandOpen = false;
  let commandQuery = '';
  let commandIndex = 0;
  let commandInput: HTMLInputElement | undefined;
  let commandList: HTMLElement | undefined;
  let commandResults: PaletteCommand[] = [];

  $: if (typeof document !== 'undefined') document.documentElement.dataset.theme = theme;
  $: selected = entries.find((entry) => entry.id === selectedId);
  $: autoLockPaused = editorOpen || importPreview !== null || restoreOpen || commandOpen || nativeDialogOpen || importBusy;
  $: autoLockSeconds = Math.ceil(autoLockRemaining / 1000);
  $: lockWarning = !keepUnlocked && !autoLockPaused && mode === 'ready' && autoLockRemaining > 0 && autoLockRemaining <= autoLockWarningMs;
  $: if (commandOpen) commandResults = buildPaletteCommands(commandQuery);
  $: emailSuggestions = [...new Map(entries
    .flatMap((entry) => [entry.email, entry.username])
    .map((email) => email.trim())
    .filter((email) => email.includes('@'))
    .map((email) => [email.toLowerCase(), email])).values()];
  $: {
    if (view === 'settings') {
      filteredEntries = [];
    } else {
      const query = search.trim().toLowerCase();
      filteredEntries = entries
        .filter((entry) => view === 'favorites' ? entry.favorite : view === 'authenticator' ? !!entry.totpSecret : true)
        .filter((entry) => `${entry.title} ${entry.username} ${entry.email} ${entry.url} ${entry.cardHolder} ${entry.cardNumber} ${entry.cardExpiry} ${entry.sourceVault}`.toLowerCase().includes(query))
        .sort((a, b) => Number(b.favorite) - Number(a.favorite) || (sortOrder === 'latest-used' ? (entryUsage[b.id] ?? 0) - (entryUsage[a.id] ?? 0) : 0) || titleCollator.compare(a.title, b.title));
    }
  }

  function blankEntry(): Entry {
    return { id: '', title: '', username: '', email: '', password: '', url: '', notes: '', totpSecret: '', cardNumber: '', cardHolder: '', cardExpiry: '', cardCvv: '', cardPin: '', favorite: false, kind: 'login', sourceVault: '', sourceCreatedAt: '', sourceModifiedAt: '' };
  }

  onMount(() => {
    void checkVault();
    let unlistenClose: (() => void) | undefined;
    void getCurrentWindow().onCloseRequested(async () => {
      await clearSecretClipboard();
    }).then((unlisten) => unlistenClose = unlisten);
    const motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
    const updateMotion = () => { reducedMotion = motionQuery.matches; };
    updateMotion();
    motionQuery.addEventListener('change', updateMotion);
    const tick = window.setInterval(() => {
      const now = Date.now();
      if (clipboardDeadline) {
        clipboardRemaining = Math.max(0, Math.ceil((clipboardDeadline - now) / 1000));
        if (clipboardRemaining === 0) void clearSecretClipboard();
      }
      if (mode !== 'ready') return;
      if (selected?.totpSecret) {
        const seconds = Math.floor(now / 1000);
        remaining = totpStep - (seconds % totpStep);
        const counter = Math.floor(seconds / totpStep);
        if (counter !== lastTotpCounter) {
          void refreshTotp(selected.totpSecret, selected.id);
        }
      }
      if (!keepUnlocked) {
        if (autoLockPaused) {
          lastActivity = now - (autoLockMs - autoLockRemaining);
        } else {
          autoLockRemaining = Math.max(0, autoLockMs - (now - lastActivity));
          if (autoLockRemaining === 0) void lockNow();
        }
      }
    }, 1000);
    const activity = () => {
      lastActivity = Date.now();
      if (mode === 'ready' && !keepUnlocked) autoLockRemaining = autoLockMs;
    };
    const releaseNativeDialog = () => { nativeDialogOpen = false; };
    const shortcut = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = !!target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable);
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        if (commandOpen) {
          event.preventDefault();
          closeCommandPalette();
        } else if (mode === 'ready' && !editorOpen && !importPreview && !restoreOpen) {
          event.preventDefault();
          openCommandPalette();
        }
        return;
      }
      if (event.key === '/' && mode === 'ready' && !typing && !editorOpen && !importPreview && !restoreOpen && !commandOpen) {
        event.preventDefault();
        searchInput?.focus();
      }
    };
    window.addEventListener('pointerdown', activity);
    window.addEventListener('keydown', activity);
    window.addEventListener('keydown', shortcut);
    window.addEventListener('focus', releaseNativeDialog);
    return () => {
      window.clearInterval(tick);
      window.removeEventListener('pointerdown', activity);
      window.removeEventListener('keydown', activity);
      window.removeEventListener('keydown', shortcut);
      window.removeEventListener('focus', releaseNativeDialog);
      motionQuery.removeEventListener('change', updateMotion);
      unlistenClose?.();
      if (clipboardTimer) clearTimeout(clipboardTimer);
      if (toastTimer) clearTimeout(toastTimer);
      if (passwordRevealTimer) clearTimeout(passwordRevealTimer);
    };
  });

  async function checkVault() {
    try {
      if (!await invoke<boolean>('vault_status')) {
        mode = 'create';
        return;
      }
      if (await invoke<boolean>('try_auto_unlock_vault')) {
        keepUnlocked = true;
        mode = 'ready';
        lastActivity = Date.now();
        autoLockRemaining = autoLockMs;
        await loadEntries();
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
      mode = 'ready';
      error = '';
      lastActivity = Date.now();
      autoLockRemaining = autoLockMs;
      await loadEntries();
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function loadEntries() {
    const previousId = selectedId;
    listLoading = true;
    try {
      entries = await invoke<Entry[]>('get_entries');
      const next = entries.find((entry) => entry.id === previousId) ?? entries[0];
      if (next) {
        await selectEntry(next);
      } else {
        selectedId = '';
        hideRevealedPassword();
        totpCode = '';
        totpError = '';
      }
    } finally {
      listLoading = false;
    }
  }

  async function selectEntry(entry: Entry) {
    rememberDetailScroll();
    selectedId = entry.id;
    hideRevealedPassword();
    totpError = '';
    totpCode = '';
    if (entry.totpSecret) await refreshTotp(entry.totpSecret, entry.id);
    void restoreDetailScroll(entry.id);
  }

  function backToList() {
    rememberDetailScroll();
    selectedId = '';
    totpCode = '';
    hideRevealedPassword();
    void restoreDetailScroll('welcome');
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
      if (!enabled) {
        lastActivity = Date.now();
        autoLockRemaining = autoLockMs;
      }
    } catch (cause) {
      keepUnlocked = !enabled;
      error = errorMessage(cause);
    }
  }

  function setView(nextView: View) {
    rememberDetailScroll();
    view = nextView;
    if (nextView !== 'settings') {
      try { localStorage.setItem(filterStorageKey, nextView); } catch { /* Filter preference is optional. */ }
    }
    void restoreDetailScroll(nextView === 'settings' ? 'settings' : selectedId || 'welcome');
  }

  function setSortOrder(nextSortOrder: SortOrder) {
    sortOrder = nextSortOrder;
    try { localStorage.setItem(sortStorageKey, nextSortOrder); } catch { /* Sort preference is optional. */ }
  }

  async function refreshTotp(secret: string, entryId = selectedId) {
    try {
      const result = await invoke<TotpCode>('current_totp', { secret });
      if (selectedId !== entryId) return;
      totpCode = result.code;
      remaining = result.remaining;
      totpStep = result.step;
      lastTotpCounter = Math.floor(Date.now() / 1000 / result.step);
      totpError = '';
    } catch (cause) {
      if (selectedId !== entryId) return;
      totpCode = '';
      totpError = errorMessage(cause);
    }
  }

  async function lockNow() {
    if (mode !== 'ready' || locking) return;
    locking = true;
    try {
      await clearSecretClipboard();
      try {
        await invoke('lock_vault');
      } catch (cause) {
        error = errorMessage(cause);
      }
    } finally {
      keepUnlocked = false;
      entries = [];
      listLoading = false;
      detailScrollPositions.clear();
      selectedId = '';
      editing = blankEntry();
      editorOpen = false;
      importPreview = null;
      importBusy = false;
      restoreOpen = false;
      restorePassword = '';
      totpCode = '';
      totpError = '';
      masterPassword = '';
      commandOpen = false;
      commandQuery = '';
      commandIndex = 0;
      nativeDialogOpen = false;
      copyNotice = false;
      clipboardDeadline = 0;
      clipboardRemaining = 0;
      hideRevealedPassword();
      autoLockRemaining = autoLockMs;
      mode = 'unlock';
      locking = false;
    }
  }

  function openNewEntry() {
    editing = blankEntry();
    loginIdentifier = '';
    editPasswordVisible = false;
    showMoreLoginFields = false;
    editorOpen = true;
    error = '';
  }

  function openEditor(entry: Entry) {
    editing = { ...entry };
    loginIdentifier = entry.username || entry.email;
    editPasswordVisible = false;
    showMoreLoginFields = true;
    editorOpen = true;
    error = '';
  }

  function setLoginIdentifier(value: string) {
    loginIdentifier = value;
    if (value.includes('@')) {
      editing.email = value;
      editing.username = '';
    } else {
      editing.username = value;
      editing.email = '';
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
      hideRevealedPassword();
      if (saved.totpSecret) await refreshTotp(saved.totpSecret);
      showToast('Item saved', false, true);
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  async function removeEntry(entry: Entry) {
    nativeDialogOpen = true;
    const confirmed = window.confirm(`Delete ${entry.title}? This cannot be undone.`);
    nativeDialogOpen = false;
    if (!confirmed) return;
    try {
      await invoke('delete_entry', { entryId: entry.id });
      entries = entries.filter((saved) => saved.id !== entry.id);
      const remainingUsage = { ...entryUsage };
      delete remainingUsage[entry.id];
      entryUsage = remainingUsage;
      try { localStorage.setItem(usageStorageKey, JSON.stringify(entryUsage)); } catch { /* Usage sorting is optional. */ }
      const next = entries[0];
      selectedId = next?.id ?? '';
      hideRevealedPassword();
      totpCode = '';
      totpError = '';
      if (next?.totpSecret) await refreshTotp(next.totpSecret, next.id);
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

  async function copySecret(value: string, label: string, entry: Entry | undefined = selected) {
    const entryId = entry?.id;
    try {
      await navigator.clipboard.writeText(value);
      if (entryId) markEntryUsed(entryId);
      copied = label;
      clipboardValue = value;
      clipboardDeadline = Date.now() + 30_000;
      clipboardRemaining = 30;
      copyNotice = true;
      if (clipboardTimer) clearTimeout(clipboardTimer);
      clipboardTimer = setTimeout(() => void clearSecretClipboard(value), 30_000);
    } catch {
      showToast('Clipboard access was blocked');
    }
  }

  async function clearSecretClipboard(expected = clipboardValue) {
    if (clipboardTimer) clearTimeout(clipboardTimer);
    clipboardTimer = undefined;
    clipboardDeadline = 0;
    clipboardRemaining = 0;
    copyNotice = false;
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

  function clearClipboardNow() {
    void clearSecretClipboard();
  }

  function togglePasswordReveal() {
    if (showPassword) {
      hideRevealedPassword();
      return;
    }
    showPassword = true;
    if (passwordRevealTimer) clearTimeout(passwordRevealTimer);
    passwordRevealTimer = setTimeout(() => {
      showPassword = false;
      passwordRevealTimer = undefined;
    }, 10_000);
  }

  function hideRevealedPassword() {
    showPassword = false;
    showCardNumber = false;
    showCardSecrets = false;
    if (passwordRevealTimer) clearTimeout(passwordRevealTimer);
    passwordRevealTimer = undefined;
  }

  function toggleCardNumber() {
    showCardNumber = !showCardNumber;
  }

  function toggleCardSecrets() {
    showCardSecrets = !showCardSecrets;
  }

  function maskedCardNumber(value: string): string {
    const compact = value.replace(/\s+/g, '');
    if (compact.length <= 8) return '•••• •••• •••• ••••';
    return `${compact.slice(0, 4)} •••• •••• ${compact.slice(-4)}`;
  }

  function formattedCardNumber(value: string): string {
    return value.replace(/\s+/g, '').replace(/(.{4})/g, '$1 ').trim();
  }

  function detailKey(): string {
    return view === 'settings' ? 'settings' : selectedId || 'welcome';
  }

  function rememberDetailScroll() {
    if (detailView) detailScrollPositions.set(detailKey(), detailView.scrollTop);
  }

  async function restoreDetailScroll(key = detailKey()) {
    await tick();
    requestAnimationFrame(() => {
      if (!detailView) return;
      detailView.scrollTo({ top: detailScrollPositions.get(key) ?? 0, behavior: reducedMotion ? 'auto' : 'smooth' });
    });
  }

  async function handleCsv(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    nativeDialogOpen = false;
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
    nativeDialogOpen = true;
    try {
      const path = await invoke<string | null>('export_backup');
      if (path) showToast('Encrypted backup saved', false, true);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      nativeDialogOpen = false;
    }
  }

  function openRestore() {
    restorePassword = '';
    restoreBusy = false;
    restoreOpen = true;
    error = '';
  }

  async function confirmRestore(event: SubmitEvent) {
    event.preventDefault();
    if (!restorePassword || restoreBusy) return;
    error = '';
    restoreBusy = true;
    try {
      const restored = await invoke<boolean>('restore_backup', { masterPassword: restorePassword });
      restoreOpen = false;
      if (restored) {
        keepUnlocked = false;
        await loadEntries();
        mode = 'ready';
        showToast('Backup restored');
      }
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      restoreBusy = false;
      restorePassword = '';
    }
  }

  function showToast(message: string, centered = false, check = false) {
    toast = message;
    toastCentered = centered;
    toastCheck = check;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      toast = '';
      toastCheck = false;
    }, centered ? 3000 : 2600);
  }

  function autoLockLabel(): string {
    if (keepUnlocked) return 'Unlocks with your Windows account';
    if (autoLockRemaining <= 0) return 'Locking now';
    if (autoLockRemaining <= autoLockWarningMs) return `Locks in ${Math.ceil(autoLockRemaining / 1000)}s`;
    return `Locks in ${Math.ceil(autoLockRemaining / 60000)} min`;
  }

  function stayUnlocked() {
    lastActivity = Date.now();
    autoLockRemaining = autoLockMs;
    showToast('Auto-lock paused for 15 minutes');
  }

  function fuzzyScore(query: string, text: string): number {
    const q = query.toLowerCase();
    const t = text.toLowerCase();
    if (!q) return 0;
    const direct = t.indexOf(q);
    if (direct !== -1) return 1000 - direct + q.length * 10;
    let queryIndex = 0;
    let score = 0;
    let lastMatch = -2;
    for (let index = 0; index < t.length && queryIndex < q.length; index += 1) {
      if (t[index] !== q[queryIndex]) continue;
      score += 1;
      if (index === lastMatch + 1) score += 2;
      if (index === 0 || /[\s\-_/.@]/.test(t[index - 1])) score += 3;
      lastMatch = index;
      queryIndex += 1;
    }
    return queryIndex === q.length ? score : -Infinity;
  }

  function buildPaletteCommands(query: string): PaletteCommand[] {
    const trimmed = query.trim();
    const addCommand: PaletteCommand = {
      id: 'add-item',
      label: 'Add item',
      hint: 'Create a new login or secure item',
      icon: '+',
      run: () => openNewEntry(),
    };
    const lockCommand: PaletteCommand = {
      id: 'lock-vault',
      label: 'Lock vault',
      hint: 'Clear the clipboard and lock immediately',
      icon: '⌑',
      run: () => lockNow(),
    };
    const itemCommands: PaletteCommand[] = [];
    const matches = entries
      .map((entry) => ({
        entry,
        score: trimmed ? fuzzyScore(trimmed, `${entry.title} ${entry.username} ${entry.email} ${entry.url} ${entry.notes} ${entry.sourceVault}`) : 0,
      }))
      .filter(({ score }) => Number.isFinite(score))
      .sort((left, right) => right.score - left.score || titleCollator.compare(left.entry.title, right.entry.title))
      .slice(0, trimmed ? 5 : 4);
    for (const { entry } of matches) {
      itemCommands.push({
        id: `open-${entry.id}`,
        label: entry.title,
        hint: `${itemKindLabel(entry.kind, !!entry.totpSecret)} · Open item`,
        icon: itemIcon(entry),
        run: () => selectEntry(entry),
      });
      if (entry.password) {
        itemCommands.push({
          id: `copy-${entry.id}`,
          label: `Copy password · ${entry.title}`,
          hint: 'Clears from the clipboard after 30 seconds',
          icon: '⧉',
          run: () => copySecret(entry.password, 'password', entry),
        });
      }
      const url = externalUrl(entry.url);
      if (url) {
        itemCommands.push({
          id: `url-${entry.id}`,
          label: `Open URL · ${entry.title}`,
          hint: entry.url,
          icon: '↗',
          run: () => openUrl(url),
        });
      }
    }
    const statics = [addCommand, lockCommand].filter((command) => !trimmed || Number.isFinite(fuzzyScore(trimmed, `${command.label} ${command.hint}`)));
    return trimmed ? [...itemCommands, ...statics] : [...statics, ...itemCommands];
  }

  function openCommandPalette() {
    commandOpen = true;
    commandQuery = '';
    commandIndex = 0;
    void tick().then(() => commandInput?.focus());
  }

  function closeCommandPalette() {
    commandOpen = false;
    commandQuery = '';
    commandIndex = 0;
    commandResults = [];
  }

  function revealActiveCommand() {
    void tick().then(() => {
      commandList?.querySelector<HTMLElement>('.command-item.active')?.scrollIntoView({ block: 'nearest' });
    });
  }

  function handleCommandKeydown(event: KeyboardEvent) {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      if (commandResults.length) commandIndex = (commandIndex + 1) % commandResults.length;
      revealActiveCommand();
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      if (commandResults.length) commandIndex = (commandIndex - 1 + commandResults.length) % commandResults.length;
      revealActiveCommand();
    } else if (event.key === 'Home') {
      event.preventDefault();
      commandIndex = 0;
      revealActiveCommand();
    } else if (event.key === 'End') {
      event.preventDefault();
      commandIndex = Math.max(0, commandResults.length - 1);
      revealActiveCommand();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      const command = commandResults[commandIndex];
      if (command) void runPaletteCommand(command);
    }
  }

  async function runPaletteCommand(command: PaletteCommand) {
    closeCommandPalette();
    await command.run();
  }

  function errorMessage(cause: unknown): string {
    return typeof cause === 'string' ? cause : cause instanceof Error ? cause.message : 'Something went wrong.';
  }

  function initials(title: string): string {
    return title.trim().slice(0, 1).toUpperCase() || '•';
  }

  function itemKindLabel(kind: string, hasTotp = false): string {
    const label = kindLabels[kind] ?? 'Imported item';
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

  function dialogAction(node: HTMLElement, onClose: () => void) {
    const previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusable = () => Array.from(node.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    )).filter((element) => element.offsetParent !== null);
    const initial = node.querySelector<HTMLElement>('input:not([type="hidden"]), textarea, select') ?? focusable()[0] ?? node;
    initial.focus();
    const handleKeydown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== 'Tab') return;
      const items = focusable();
      if (items.length === 0) return;
      const firstItem = items[0];
      const lastItem = items[items.length - 1];
      if (!node.contains(document.activeElement)) {
        event.preventDefault();
        firstItem.focus();
        return;
      }
      if (event.shiftKey && document.activeElement === firstItem) {
        event.preventDefault();
        lastItem.focus();
      } else if (!event.shiftKey && document.activeElement === lastItem) {
        event.preventDefault();
        firstItem.focus();
      }
    };
    node.addEventListener('keydown', handleKeydown);
    return {
      destroy() {
        node.removeEventListener('keydown', handleKeydown);
        previouslyFocused?.focus();
      },
    };
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
  <main class="gate loading-gate" in:fade={{ duration: reducedMotion ? 0 : motion.base }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <div class="vault-loader" role="status" aria-live="polite">
      <div class="loader-orbit" aria-hidden="true"><i></i><i></i><i></i><span class="loader-brand"><span class="brand-glyph">p</span></span></div>
      <p class="loader-label">Opening your vault…</p>
      <p class="loader-sub">Decrypting locally on this device</p>
    </div>
  </main>
{:else if mode === 'create' || mode === 'unlock'}
  <main class="gate" in:fade={{ duration: reducedMotion ? 0 : motion.base }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <div class="gate-card">
      <div class="brand-lockup"><span class="brand-mark"><span class="brand-glyph">p</span></span><span>passvault</span></div>
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
        {#if mode === 'unlock'}<button type="button" class="secondary-button full-width" onclick={openRestore}><span>Restore from backup</span><span aria-hidden="true">↥</span></button>{/if}
      </form>
      <div class="local-note"><span class="status-dot"></span>Stored only on this Windows device</div>
    </div>
    <div class="gate-foot">Local vault · End-to-end encrypted at rest · No account required</div>
  </main>
{:else}
  <main class="app-shell" class:settings-mode={view === 'settings'} in:fade={{ duration: reducedMotion ? 0 : motion.slow }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <aside class="sidebar">
      <div class="brand-lockup sidebar-brand"><span class="brand-mark"><span class="brand-glyph">p</span></span><span class="brand-name">passvault</span></div>
      <div class="search-box">
        <span class="search-icon">⌕</span>
        <input bind:this={searchInput} bind:value={search} placeholder="Search vault" aria-label="Search vault" aria-keyshortcuts="/ Control+K" />
        <button class="search-shortcut" type="button" title="Open command palette (Ctrl+K)" aria-label="Open command palette" onclick={openCommandPalette}>Ctrl K</button>
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
        <button class="side-action" title="Restore from backup" onclick={openRestore}><span>↥</span><span class="side-text">Restore from backup</span></button>
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
        <button class="add-button" aria-label="Add item" title="Add item" onclick={openNewEntry}><span class="add-glyph">+</span></button>
      </header>
      <div class="list-caption">
        <span>{filteredEntries.length} {filteredEntries.length === 1 ? 'ITEM' : 'ITEMS'}</span>
        <div class="sort-toggle" role="group" aria-label="Sort items">
          <button class:active={sortOrder === 'alphabetical'} aria-pressed={sortOrder === 'alphabetical'} onclick={() => setSortOrder('alphabetical')}>A-Z</button>
          <button class:active={sortOrder === 'latest-used'} aria-pressed={sortOrder === 'latest-used'} onclick={() => setSortOrder('latest-used')}>Latest used</button>
        </div>
      </div>
      <div class="entry-list" aria-busy={listLoading}>
        {#if listLoading}
          {#each [0, 1, 2, 3, 4] as row (row)}
            <div class="skeleton-row" aria-hidden="true">
              <span class="skeleton-icon"></span>
              <span class="skeleton-lines"><span class="skeleton-line"></span><span class="skeleton-line short"></span></span>
            </div>
          {/each}
        {:else}
          {#each filteredEntries as entry (entry.id)}
            <button class="entry-row" class:selected={selectedId === entry.id} onclick={() => selectEntry(entry)} animate:flip={{ duration: reducedMotion ? 0 : motion.base, easing: easeOut }}>
              <span class="entry-icon" class:note-icon={entry.kind === 'note' || entry.kind === 'alias'}>{itemIcon(entry)}</span>
              <span class="entry-copy"><strong>{entry.title}</strong><small>{entry.username || entry.email || entry.url || itemKindLabel(entry.kind, !!entry.totpSecret)}</small></span>
              {#if entry.favorite}<span class="favorite-star" aria-label="Favorite">★</span>{/if}
            </button>
          {:else}
            <div class="empty-list">
              <span class="empty-illustration" aria-hidden="true"><i></i><i></i><span>◇</span></span>
              <strong>{search ? 'No matches found' : 'Your vault is ready'}</strong>
              <p>{search ? 'Try a different search.' : 'Import a Proton Pass CSV from Settings or add your first login.'}</p>
            </div>
          {/each}
        {/if}
      </div>
      <div class="list-bottom"><span class="status-dot"></span>Encrypted on this device</div>
    </section>

    <section class="detail-pane" aria-label="Item details">
      {#key `${view}:${selectedId || 'none'}`}
        {#if view === 'settings'}
          <div class="detail-view" in:fly={{ y: reducedMotion ? 0 : 10, duration: reducedMotion ? 0 : motion.base, easing: easeOut }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }} bind:this={detailView}>
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
              <input type="checkbox" bind:checked={keepUnlocked} onchange={(event) => void setKeepUnlocked(event.currentTarget.checked)} />
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
              <input type="file" accept=".csv,text/csv" onclick={() => nativeDialogOpen = true} onchange={handleCsv} />
            </label>
          </section>
          <p class="settings-note"><span class="status-dot"></span>Theme changes never affect vault encryption or stored passwords.</p>
            </div>
          </div>
      {:else if selected}
        <div class="detail-view" in:fly={{ y: reducedMotion ? 0 : 10, duration: reducedMotion ? 0 : motion.base, easing: easeOut }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }} bind:this={detailView}>
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
                  <div class="totp-code-row"><strong class="totp-code">{totpCode.slice(0, Math.floor(totpCode.length / 2))} {totpCode.slice(Math.floor(totpCode.length / 2))}</strong><button class="copy-code" class:done={copied === 'code'} onclick={() => copySecret(totpCode, 'code')}><span>{copied === 'code' ? 'Copied' : 'Copy'}</span><span class="copy-feedback" class:done={copied === 'code'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></button></div>
                  <p class="totp-caption">Refreshes in {remaining}s</p>
                {:else}<p class="totp-error">{totpError || 'Reading authenticator…'}</p>{/if}
              </div>
              <span class="totp-ring" style={`--progress: ${(remaining / totpStep) * 100}%`} role="timer" aria-label={`Code refreshes in ${remaining} seconds`}><span>{remaining}</span></span>
            </div>
          {/if}

          <div class="detail-fields">
            {#if selected.kind === 'creditCard'}
              {#if selected.cardHolder}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy name on card" onclick={() => copySecret(selected!.cardHolder, 'cardHolder')}></button><div class="field-top"><span>NAME ON CARD</span><span class="copy-feedback" class:done={copied === 'cardHolder'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></div><p class="detail-value">{selected.cardHolder}</p></div>{/if}
              {#if selected.cardNumber}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy card number" onclick={() => copySecret(selected!.cardNumber, 'cardNumber')}></button><div class="field-top"><span>CARD NUMBER</span><div class="field-meta"><span class="copy-feedback" class:done={copied === 'cardNumber'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span><div class="field-actions"><button onclick={toggleCardNumber}>{showCardNumber ? 'Hide' : 'Reveal'}</button></div></div></div><p class="detail-value card-number-value">{showCardNumber ? formattedCardNumber(selected.cardNumber) : maskedCardNumber(selected.cardNumber)}</p></div>{/if}
              {#if selected.cardExpiry}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy expiration date" onclick={() => copySecret(selected!.cardExpiry, 'cardExpiry')}></button><div class="field-top"><span>EXPIRATION DATE</span><span class="copy-feedback" class:done={copied === 'cardExpiry'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></div><p class="detail-value">{selected.cardExpiry}</p></div>{/if}
              {#if selected.cardCvv}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy security code" onclick={() => copySecret(selected!.cardCvv, 'cardCvv')}></button><div class="field-top"><span>SECURITY CODE</span><div class="field-meta"><span class="copy-feedback" class:done={copied === 'cardCvv'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span><div class="field-actions"><button onclick={toggleCardSecrets}>{showCardSecrets ? 'Hide' : 'Reveal'}</button></div></div></div><p class="detail-value secret-value" class:revealed={showCardSecrets}>{showCardSecrets ? selected.cardCvv : '••••'}</p></div>{/if}
              {#if selected.cardPin}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy PIN" onclick={() => copySecret(selected!.cardPin, 'cardPin')}></button><div class="field-top"><span>PIN</span><div class="field-meta"><span class="copy-feedback" class:done={copied === 'cardPin'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span><div class="field-actions"><button onclick={toggleCardSecrets}>{showCardSecrets ? 'Hide' : 'Reveal'}</button></div></div></div><p class="detail-value secret-value" class:revealed={showCardSecrets}>{showCardSecrets ? selected.cardPin : '••••'}</p></div>{/if}
            {/if}
            {#if selected.username}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy username" onclick={() => copySecret(selected!.username, 'username')}></button><div class="field-top"><span>USERNAME</span><span class="copy-feedback" class:done={copied === 'username'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></div><p class="detail-value">{selected.username}</p></div>{/if}
            {#if selected.email}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy email" onclick={() => copySecret(selected!.email, 'email')}></button><div class="field-top"><span>EMAIL</span><span class="copy-feedback" class:done={copied === 'email'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></div><p class="detail-value">{selected.email}</p></div>{/if}
            {#if selected.password}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy password" onclick={() => copySecret(selected!.password, 'password')}></button><div class="field-top"><span>PASSWORD</span><div class="field-meta"><span class="copy-feedback" class:done={copied === 'password'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span><div class="field-actions"><button onclick={togglePasswordReveal}>{showPassword ? 'Hide' : 'Reveal'}</button></div></div></div><p class="detail-value secret-value" class:revealed={showPassword}>{showPassword ? selected.password : '••••••••••••••••'}</p>{#if showPassword}<span class="reveal-countdown">Hides automatically</span>{/if}</div>{/if}
            {#if selected.url}<div class="detail-field"><button class="field-copy-surface" aria-label="Copy website address" onclick={() => copySecret(selected!.url, 'url')}></button><div class="field-top"><span>WEBSITE</span><div class="field-meta"><span class="copy-feedback" class:done={copied === 'url'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span><button title="Open website" onclick={() => { const url = externalUrl(selected!.url); if (url) void openUrl(url); }}>Open ↗</button></div></div><p class="detail-value">{selected.url}</p></div>{/if}
            {#if selected.notes}<div class="detail-field notes-field"><button class="field-copy-surface" aria-label="Copy notes" onclick={() => copySecret(selected!.notes, 'notes')}></button><div class="field-top"><span>NOTES</span><span class="copy-feedback" class:done={copied === 'notes'} aria-hidden="true"><svg viewBox="0 0 16 16"><rect class="glyph-copy" x="5.5" y="5.5" width="7.5" height="7.5" rx="2"></rect><path class="glyph-check" d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span></div><p class="detail-value">{selected.notes}</p></div>{/if}
            {#if selected.sourceVault}<div class="detail-field"><div class="field-top"><span>PROTON VAULT</span></div><p>{selected.sourceVault}</p></div>{/if}
            {#if selected.sourceCreatedAt}<div class="detail-field"><div class="field-top"><span>ORIGINAL CREATION DATE</span></div><p>{protonDate(selected.sourceCreatedAt)}</p></div>{/if}
            {#if selected.sourceModifiedAt}<div class="detail-field"><div class="field-top"><span>LAST MODIFIED IN PROTON</span></div><p>{protonDate(selected.sourceModifiedAt)}</p></div>{/if}
            {#if !selected.username && !selected.email && !selected.password && !selected.url && !selected.notes && !selected.totpSecret && !selected.cardNumber && !selected.cardHolder && !selected.cardExpiry && !selected.cardCvv && !selected.cardPin && !selected.sourceVault && !selected.sourceCreatedAt && !selected.sourceModifiedAt}<div class="detail-empty">Nothing saved in this item yet.</div>{/if}
          </div>
          <div class="detail-security"><span class="security-check">✓</span><span>Encrypted and stored locally</span><span class="detail-separator">·</span><span class:security-warning={!keepUnlocked && autoLockRemaining <= autoLockWarningMs}>{autoLockLabel()}</span>{#if !keepUnlocked && !autoLockPaused}<span class="autolock-meter" aria-hidden="true"><span style={`width:${Math.round((autoLockRemaining / autoLockMs) * 100)}%`}></span></span>{/if}</div>
        </div>
        </div>
      {:else}
        <div class="detail-view welcome-view" in:fly={{ y: reducedMotion ? 0 : 10, duration: reducedMotion ? 0 : motion.base, easing: easeOut }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }} bind:this={detailView}>
          <div class="welcome-detail"><div class="welcome-glyph">p</div><span class="eyebrow">YOUR DIGITAL KEYS, ORGANIZED</span><h2>A calmer place<br />for your passwords.</h2><p>Select an item to view it, or start by adding your first login.</p><button class="primary-button" onclick={openNewEntry}>Add your first item <span>↗</span></button></div>
        </div>
      {/if}
      {/key}
    </section>

    {#if error}<div class="app-error" role="alert" transition:fade={{ duration: reducedMotion ? 0 : motion.fast }}><span>{error}</span><button aria-label="Dismiss error" onclick={() => error = ''}>×</button></div>{/if}
    {#if lockWarning}
      <div class="lock-warning" role="status" transition:fly={{ y: reducedMotion ? 0 : 12, duration: reducedMotion ? 0 : motion.base, easing: easeOut }}>
        <span class="lock-warning-dot" aria-hidden="true"></span>
        <span>Vault locks in <strong>{autoLockSeconds}s</strong></span>
        <button type="button" onclick={stayUnlocked}>Stay unlocked</button>
      </div>
    {/if}
    {#if toast}
      <div class="toast" class:copy-toast={toastCentered} role="status" transition:fly={{ y: reducedMotion ? 0 : 12, duration: reducedMotion ? 0 : motion.base, easing: easeOut }}>
        {#if toastCheck}<span class="toast-check" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span>{/if}
        <span>{toast}</span>
      </div>
    {/if}
    {#if copyNotice && clipboardValue}
      <div class="toast copy-toast security-toast" role="status" transition:fly={{ y: reducedMotion ? 0 : 12, duration: reducedMotion ? 0 : motion.base, easing: easeOut }}>
        <span class="toast-check" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="M3.5 8.5 6.5 11.5 12.5 5"></path></svg></span>
        <div class="copy-toast-copy"><strong>Copied to clipboard</strong><small>Cleared in {clipboardRemaining}s</small></div>
        <span class="countdown-ring" style={`--progress: ${(clipboardRemaining / 30) * 100}%`} role="timer" aria-label={`Clipboard clears in ${clipboardRemaining} seconds`}><span>{clipboardRemaining}</span></span>
        <button type="button" class="toast-action" onclick={clearClipboardNow}>Clear now</button>
      </div>
    {/if}
  </main>
{/if}

{#if editorOpen}
  <div class="modal-backdrop" class:login-backdrop={editing.kind === 'login'} role="presentation" onclick={(event) => { if (event.target === event.currentTarget) editorOpen = false; }} in:fade={{ duration: reducedMotion ? 0 : motion.fast }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <datalist id="saved-email-suggestions">
      {#each emailSuggestions as email}<option value={email}></option>{/each}
    </datalist>
    {#if editing.kind === 'login'}
      <div class="modal editor-modal login-modal" role="dialog" aria-modal="true" aria-labelledby="editor-title" tabindex="-1" use:dialogAction={() => editorOpen = false}>
        <form onsubmit={saveEntry} class="editor-form login-composer">
          <header class="login-topbar">
            <button type="button" class="login-close" aria-label="Close" onclick={() => editorOpen = false}>×</button>
            <button type="submit" class="primary-button login-submit">{editing.id ? 'Save login' : 'Create login'}</button>
          </header>
          <h2 id="editor-title" class="sr-only">{editing.id ? 'Edit login' : 'New login'}</h2>
          <label class="login-title-field"><span>Title</span><input bind:value={editing.title} placeholder="Untitled" required /></label>
          <section class="login-credentials" aria-label="Login credentials">
            <label class="login-row"><span class="login-row-icon" aria-hidden="true">♙</span><span class="login-row-content"><strong>Email or username</strong><input value={loginIdentifier} oninput={(event) => setLoginIdentifier(event.currentTarget.value)} autocomplete="username" list="saved-email-suggestions" placeholder="Enter email or username" /></span></label>
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
      <div class="modal editor-modal" role="dialog" aria-modal="true" aria-labelledby="editor-title" tabindex="-1" use:dialogAction={() => editorOpen = false} in:fly={{ y: reducedMotion ? 0 : 18, duration: reducedMotion ? 0 : motion.slow, easing: easeSpring }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
        <header class="modal-header"><div><span class="eyebrow">{editing.id ? 'EDIT VAULT ITEM' : 'NEW VAULT ITEM'}</span><h2 id="editor-title">{editing.id ? 'Edit item' : 'Add an item'}</h2><p class="editor-subtitle">Keep important details together in your vault.</p></div><button class="icon-button" aria-label="Close" onclick={() => editorOpen = false}>×</button></header>
        <form onsubmit={saveEntry} class="editor-form">
          <div class="editor-basics">
            <label>Item name<input bind:value={editing.title} placeholder="e.g. Personal email" required /></label>
            <label>Item type<select bind:value={editing.kind}><option value="login">Login</option><option value="note">Secure note</option><option value="alias">Email alias</option><option value="creditCard">Credit card</option><option value="identity">Identity</option><option value="custom">Custom item</option></select></label>
          </div>
          {#if editing.kind === 'alias'}
            <label>Email address<input bind:value={editing.email} autocomplete="off" list="saved-email-suggestions" /></label>
            <label>Website<input bind:value={editing.url} placeholder="https://" /></label>
          {:else if editing.kind === 'creditCard'}
            <section class="editor-section" aria-label="Card details">
              <div class="editor-section-heading"><span>CARD DETAILS</span><small>Stored encrypted on this device</small></div>
              <div class="editor-grid">
                <label class="editor-field-wide">Name on card<input bind:value={editing.cardHolder} autocomplete="off" spellcheck="false" placeholder="Name as shown on card" /></label>
                <label class="editor-field-wide">Card number<input bind:value={editing.cardNumber} autocomplete="off" inputmode="numeric" spellcheck="false" placeholder="0000 0000 0000 0000" /></label>
                <label>Expiration date<input bind:value={editing.cardExpiry} autocomplete="off" inputmode="numeric" spellcheck="false" placeholder="MM/YY" /></label>
                <label>Security code<input bind:value={editing.cardCvv} autocomplete="off" inputmode="numeric" spellcheck="false" placeholder="CVV" /></label>
                <label>PIN<input bind:value={editing.cardPin} autocomplete="off" inputmode="numeric" spellcheck="false" placeholder="PIN" /></label>
              </div>
            </section>
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
  <div class="modal-backdrop" role="presentation" onclick={(event) => { if (event.target === event.currentTarget && !importBusy) importPreview = null; }} in:fade={{ duration: reducedMotion ? 0 : motion.fast }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <div class="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-modal-title" tabindex="-1" use:dialogAction={() => { if (!importBusy) importPreview = null; }} in:fly={{ y: reducedMotion ? 0 : 18, duration: reducedMotion ? 0 : motion.slow, easing: easeSpring }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
      <header class="modal-header"><div><span class="eyebrow">LOCAL CSV IMPORT</span><h2 id="import-modal-title">Review your import</h2></div><button class="icon-button" aria-label="Close" disabled={importBusy} onclick={() => importPreview = null}>×</button></header>
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
      <footer class="modal-footer"><button class="secondary-button" disabled={importBusy} onclick={() => importPreview = null}>Cancel</button>{#if importPreview.duplicateIds.length}<button class="secondary-button" disabled={importBusy} onclick={() => confirmImport('skip')}>{importBusy ? 'Importing…' : 'Skip duplicates'}</button>{#if importPreview.duplicateMatches.length}<button class="secondary-button" disabled={importBusy} onclick={() => confirmImport('update')}>{importBusy ? 'Importing…' : 'Update matches'}</button>{/if}{/if}<button class="primary-button" disabled={importBusy || importPreview.entries.length === 0} onclick={() => confirmImport('all')}>{importBusy ? 'Importing…' : importPreview.duplicateIds.length ? 'Import all' : 'Import items'} <span>↗</span></button></footer>
    </div>
  </div>
{/if}

{#if restoreOpen}
  <div class="modal-backdrop" role="presentation" onclick={(event) => { if (event.target === event.currentTarget && !restoreBusy) restoreOpen = false; }} in:fade={{ duration: reducedMotion ? 0 : motion.fast }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <div class="modal" role="dialog" aria-modal="true" aria-labelledby="restore-title" tabindex="-1" use:dialogAction={() => { if (!restoreBusy) restoreOpen = false; }} in:fly={{ y: reducedMotion ? 0 : 18, duration: reducedMotion ? 0 : motion.slow, easing: easeSpring }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
      <header class="modal-header"><div><span class="eyebrow">RESTORE BACKUP</span><h2 id="restore-title">Restore an encrypted backup</h2><p class="editor-subtitle">Restoring replaces this device’s current vault.</p></div><button class="icon-button" aria-label="Close" disabled={restoreBusy} onclick={() => restoreOpen = false}>×</button></header>
      <form onsubmit={confirmRestore} class="editor-form">
        <label>Backup master password<input type="password" bind:value={restorePassword} autocomplete="current-password" required /></label>
        <p class="field-hint">Use the master password that was active when this backup was created.</p>
        {#if error}<p class="error-message">{error}</p>{/if}
        <footer class="modal-footer"><button type="button" class="secondary-button" disabled={restoreBusy} onclick={() => restoreOpen = false}>Cancel</button><button class="primary-button" type="submit" disabled={restoreBusy || !restorePassword}>{restoreBusy ? 'Restoring…' : 'Choose backup and restore'} <span aria-hidden="true">↥</span></button></footer>
      </form>
    </div>
  </div>
{/if}

{#if commandOpen}
  <div class="command-backdrop" role="presentation" onclick={(event) => { if (event.target === event.currentTarget) closeCommandPalette(); }} in:fade={{ duration: reducedMotion ? 0 : motion.fast }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
    <div class="command-palette" role="dialog" aria-modal="true" aria-label="Command palette" tabindex="-1" use:dialogAction={closeCommandPalette} in:fly={{ y: reducedMotion ? 0 : -10, duration: reducedMotion ? 0 : motion.slow, easing: easeSpring }} out:fade={{ duration: reducedMotion ? 0 : motion.fast }}>
      <div class="command-search">
        <span class="command-search-icon" aria-hidden="true">⌕</span>
        <input bind:this={commandInput} bind:value={commandQuery} oninput={() => commandIndex = 0} onkeydown={handleCommandKeydown} placeholder="Search items or run a command…" aria-label="Command palette search" autocomplete="off" spellcheck="false" />
        <kbd>Esc</kbd>
      </div>
      <div class="command-list" role="listbox" aria-label="Commands" bind:this={commandList}>
        {#each commandResults as command, index (command.id)}
          <button type="button" class="command-item" class:active={index === commandIndex} role="option" aria-selected={index === commandIndex} onmouseenter={() => commandIndex = index} onclick={() => runPaletteCommand(command)}>
            <span class="command-icon" aria-hidden="true">{command.icon}</span>
            <span class="command-copy"><strong>{command.label}</strong><small>{command.hint}</small></span>
            {#if index === commandIndex}<span class="command-enter" aria-hidden="true">↵</span>{/if}
          </button>
        {:else}
          <div class="command-empty">No matches. Try another search.</div>
        {/each}
      </div>
      <footer class="command-footer"><span><kbd>↑</kbd><kbd>↓</kbd> navigate</span><span><kbd>↵</kbd> run</span><span><kbd>Ctrl</kbd><kbd>K</kbd> toggle</span></footer>
    </div>
  </div>
{/if}

</div>

<svelte:head><meta name="description" content="A local-first encrypted password manager" /></svelte:head>
