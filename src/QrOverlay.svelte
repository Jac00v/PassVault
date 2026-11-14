<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';

  type ScanRegionResult = { ok: boolean; label?: string | null; message?: string | null };
  type Status = 'loading' | 'selecting' | 'scanning' | 'error' | 'closing';

  const minimumSelection = 8;
  const defaultNoQrMessage = 'No QR code found in that area — try a tighter box';

  let stage: HTMLElement | undefined;
  let canvas: HTMLCanvasElement | undefined;
  let frameWidth = 0;
  let frameHeight = 0;
  let selection = { x: 0, y: 0, width: 0, height: 0 };
  let previousSelection = { x: 0, y: 0, width: 0, height: 0 };
  let dragStart: { x: number; y: number } | undefined;
  let dragResetTimer: ReturnType<typeof setTimeout> | undefined;
  let status: Status = 'loading';
  let message = '';
  let foundLabel = '';
  let lastActivityPing = 0;

  $: hasSelection = selection.width >= 1 && selection.height >= 1;
  $: selectionStyle = frameWidth > 0 && frameHeight > 0
    ? `left:${(selection.x / frameWidth) * 100}%;top:${(selection.y / frameHeight) * 100}%;width:${(selection.width / frameWidth) * 100}%;height:${(selection.height / frameHeight) * 100}%;`
    : '';

  function clamp(value: number, minimum: number, maximum: number): number {
    return Math.min(Math.max(value, minimum), maximum);
  }

  function framePoint(event: PointerEvent): { x: number; y: number } {
    if (!canvas) return { x: 0, y: 0 };
    const bounds = canvas.getBoundingClientRect();
    if (!bounds.width || !bounds.height) return { x: 0, y: 0 };
    return {
      x: clamp((event.clientX - bounds.left) * frameWidth / bounds.width, 0, frameWidth),
      y: clamp((event.clientY - bounds.top) * frameHeight / bounds.height, 0, frameHeight),
    };
  }

  function normalizeRect(start: { x: number; y: number }, end: { x: number; y: number }) {
    return {
      x: Math.min(start.x, end.x),
      y: Math.min(start.y, end.y),
      width: Math.abs(end.x - start.x),
      height: Math.abs(end.y - start.y),
    };
  }

  function pingActivity() {
    const now = Date.now();
    if (now - lastActivityPing < 2000) return;
    lastActivityPing = now;
    void invoke('qr_overlay_activity').catch(() => undefined);
  }

  function clearDragReset() {
    if (dragResetTimer !== undefined) {
      clearTimeout(dragResetTimer);
      dragResetTimer = undefined;
    }
  }

  function armDragReset() {
    clearDragReset();
    dragResetTimer = setTimeout(() => {
      if (dragStart) stopDragWithoutScan();
    }, 20_000);
  }

  function beginSelection(event: PointerEvent) {
    if (event.button !== 0 || status === 'loading' || status === 'scanning' || status === 'closing') return;
    pingActivity();
    previousSelection = { ...selection };
    dragStart = framePoint(event);
    status = 'selecting';
    message = '';
    foundLabel = '';
    selection = { x: dragStart.x, y: dragStart.y, width: 0, height: 0 };
    armDragReset();
    event.preventDefault();
  }

  function stopDragWithoutScan() {
    dragStart = undefined;
    clearDragReset();
    status = 'selecting';
  }

  function updateSelection(event: PointerEvent) {
    pingActivity();
    if (!dragStart) return;
    if (event.buttons === 0) {
      stopDragWithoutScan();
      return;
    }
    selection = normalizeRect(dragStart, framePoint(event));
  }

  function endSelection(event: PointerEvent) {
    if (!dragStart) return;
    pingActivity();
    const next = normalizeRect(dragStart, framePoint(event));
    dragStart = undefined;
    clearDragReset();
    if (next.width < minimumSelection || next.height < minimumSelection) {
      selection = { ...previousSelection };
      status = 'selecting';
      return;
    }
    selection = next;
    void scanSelection();
  }

  function abortSelection() {
    dragStart = undefined;
    clearDragReset();
    selection = { ...previousSelection };
    status = 'selecting';
  }

  function onPointerLeave() {
    if (dragStart) stopDragWithoutScan();
  }

  function selectionRequest() {
    const x = clamp(Math.round(selection.x), 0, Math.max(0, frameWidth - 1));
    const y = clamp(Math.round(selection.y), 0, Math.max(0, frameHeight - 1));
    const right = clamp(Math.round(selection.x + selection.width), x + 1, frameWidth);
    const bottom = clamp(Math.round(selection.y + selection.height), y + 1, frameHeight);
    return { x, y, width: right - x, height: bottom - y };
  }

  async function scanSelection() {
    if (status === 'loading' || status === 'scanning' || status === 'closing') return;
    if (selection.width < minimumSelection || selection.height < minimumSelection) return;

    status = 'scanning';
    message = '';
    foundLabel = '';
    try {
      const result = await invoke<ScanRegionResult>('qr_scan_region', selectionRequest());
      if (isClosing()) return;
      if (result.ok) {
        foundLabel = result.label ?? '';
        await submitSelection();
      } else {
        message = result.message || defaultNoQrMessage;
        status = 'error';
      }
    } catch (cause) {
      if (isClosing()) return;
      message = typeof cause === 'string' && cause ? cause : 'The QR code could not be read.';
      status = 'error';
    }
  }

  async function submitSelection() {
    if (status === 'closing' || frameWidth === 0) return;
    status = 'closing';
    try {
      await invoke('qr_scan_submit');
    } catch (cause) {
      message = typeof cause === 'string' && cause ? cause : 'The captured key could not be added.';
      status = 'error';
    }
  }

  function cancelScan() {
    if (status === 'closing') return;
    status = 'closing';
    void invoke('qr_scan_cancel').catch(() => undefined);
  }

  function isClosing(): boolean {
    return status === 'closing';
  }

  function moveSelection(event: KeyboardEvent) {
    if (!event.key.startsWith('Arrow') || !hasSelection || frameWidth === 0 || frameHeight === 0) return false;
    const step = Math.max(8, Math.round(Math.min(frameWidth, frameHeight) / 100));

    if (event.shiftKey) {
      event.preventDefault();
      if (event.key === 'ArrowLeft') {
        selection.width = Math.max(minimumSelection, selection.width - step);
      } else if (event.key === 'ArrowRight') {
        selection.width = Math.min(frameWidth - selection.x, selection.width + step);
      } else if (event.key === 'ArrowUp') {
        selection.height = Math.max(minimumSelection, selection.height - step);
      } else if (event.key === 'ArrowDown') {
        selection.height = Math.min(frameHeight - selection.y, selection.height + step);
      }
    } else {
      event.preventDefault();
      if (event.key === 'ArrowLeft') selection.x -= step;
      else if (event.key === 'ArrowRight') selection.x += step;
      else if (event.key === 'ArrowUp') selection.y -= step;
      else if (event.key === 'ArrowDown') selection.y += step;
      selection.x = clamp(selection.x, 0, Math.max(0, frameWidth - selection.width));
      selection.y = clamp(selection.y, 0, Math.max(0, frameHeight - selection.height));
    }

    selection = { ...selection };
    status = 'selecting';
    message = '';
    foundLabel = '';
    pingActivity();
    return true;
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      cancelScan();
      return;
    }

    if (event.key === 'Enter') {
      if ((event.target as HTMLElement | null)?.tagName === 'BUTTON') return;
      event.preventDefault();
      if (hasSelection && status !== 'scanning' && status !== 'closing' && status !== 'loading') {
        void scanSelection();
      }
      return;
    }

    if (status === 'scanning' || status === 'closing' || status === 'loading') return;
    moveSelection(event);
  }

  async function loadFrame() {
    try {
      const buffer = await invoke<ArrayBuffer>('qr_overlay_frame');
      if (!(buffer instanceof ArrayBuffer) || buffer.byteLength < 8) throw new Error('Frame unavailable');
      const view = new DataView(buffer);
      const width = view.getUint32(0, true);
      const height = view.getUint32(4, true);
      if (!width || !height || buffer.byteLength !== 8 + width * height * 4) {
        throw new Error('Frame unavailable');
      }
      if (!canvas) throw new Error('Frame unavailable');

      frameWidth = width;
      frameHeight = height;
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext('2d');
      if (!context) throw new Error('Frame unavailable');
      context.putImageData(
        new ImageData(new Uint8ClampedArray(buffer, 8, width * height * 4), width, height),
        0,
        0,
      );
      await tick();
      await new Promise((resolve) => setTimeout(resolve, 30));
      await invoke('qr_overlay_ready');
      status = 'selecting';
      stage?.focus();
      window.focus();
    } catch (cause) {
      message = typeof cause === 'string' && cause
        ? cause
        : 'The screen scan could not be prepared.';
      status = 'error';
      void invoke('qr_scan_cancel').catch(() => undefined);
    }
  }

  onMount(() => {
    const onWindowPointerUp = () => {
      if (dragStart) abortSelection();
    };
    const onWindowBlur = () => {
      if (dragStart) abortSelection();
    };
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('pointerup', onWindowPointerUp);
    window.addEventListener('blur', onWindowBlur);
    void loadFrame();
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('pointerup', onWindowPointerUp);
      window.removeEventListener('blur', onWindowBlur);
      clearDragReset();
    };
  });
</script>

<div
  bind:this={stage}
  class="stage"
  role="application"
  aria-label="Select the 2FA QR code region"
  tabindex="-1"
  onpointerdown={beginSelection}
  onpointermove={updateSelection}
  onpointerup={endSelection}
  onpointercancel={abortSelection}
  onpointerleave={onPointerLeave}
>
  <canvas bind:this={canvas} aria-hidden="true"></canvas>

  <div class="shade-layer" aria-hidden="true">
    {#if hasSelection}
      <div class="shade" style={selectionStyle}></div>
    {:else}
      <div class="veil"></div>
    {/if}
  </div>

  <div class="hud" role="group" aria-label="Screen scan controls" onpointerdown={(event) => event.stopPropagation()}>
    <div class="status" role="status" aria-live="polite">
      {#if status === 'loading'}
        <strong>Preparing the screen…</strong>
      {:else if status === 'scanning'}
        <strong>Reading the QR code…</strong>
      {:else if status === 'error'}
        <strong>{message || defaultNoQrMessage}</strong>
        <span>Drag a new box, or press Esc to cancel.</span>
      {:else if status === 'closing'}
        <strong>Adding {foundLabel || 'the 2FA key'}…</strong>
      {:else}
        <strong>Drag a box around the QR code</strong>
        {#if hasSelection}
          <span>Arrows move · Shift + arrows resize · Enter rescans · Esc cancels</span>
        {:else}
          <span>Draw tightly around the code · Esc cancels</span>
        {/if}
      {/if}
    </div>
    <button type="button" class="cancel" disabled={status === 'closing'} onclick={cancelScan}>Cancel</button>
  </div>
</div>

<style>
  :global(html, body) {
    height: 100%;
    margin: 0;
    overflow: hidden;
    background: #000;
  }

  :global(body) {
    color: #fff;
    font-family: "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
    user-select: none;
    -webkit-user-select: none;
  }

  .stage,
  .stage canvas,
  .shade-layer,
  .shade,
  .veil {
    cursor: crosshair;
  }

  .stage {
    position: fixed;
    inset: 0;
    overflow: hidden;
    background: #000;
    cursor: crosshair;
    touch-action: none;
  }

  .stage:focus {
    outline: 0;
  }

  canvas {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  .shade-layer {
    position: absolute;
    inset: 0;
    overflow: hidden;
    pointer-events: none;
  }

  .shade {
    position: absolute;
    border: 2px solid rgba(122, 226, 196, .95);
    border-radius: 4px;
    box-shadow: 0 0 0 100vmax rgba(2, 8, 12, .58), 0 0 0 1px rgba(0, 0, 0, .55);
    transition: border-color 160ms ease, box-shadow 160ms ease;
  }

  .veil {
    position: absolute;
    inset: 0;
    background: rgba(2, 8, 12, .38);
  }

  .hud {
    position: absolute;
    top: 18px;
    left: 50%;
    display: flex;
    max-width: min(760px, calc(100vw - 36px));
    align-items: center;
    gap: 14px;
    padding: 10px 12px 10px 16px;
    border: 1px solid rgba(255, 255, 255, .16);
    border-radius: 14px;
    color: #f2f7f5;
    background: rgba(13, 21, 24, .82);
    box-shadow: 0 16px 42px rgba(0, 0, 0, .36);
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
    transform: translateX(-50%);
    cursor: default;
  }

  .status {
    display: grid;
    min-width: 0;
    gap: 3px;
    font-size: 12px;
    line-height: 1.45;
  }

  .status strong {
    font-weight: 600;
  }

  .status span {
    color: rgba(226, 236, 232, .72);
    font-size: 11px;
  }

  .cancel {
    flex: 0 0 auto;
    padding: 7px 12px;
    border: 1px solid rgba(255, 255, 255, .18);
    border-radius: 9px;
    color: #e9f4ef;
    background: rgba(255, 255, 255, .08);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }

  .cancel:hover {
    background: rgba(255, 255, 255, .15);
  }

  .cancel:focus-visible {
    outline: 2px solid rgba(122, 226, 196, .9);
    outline-offset: 2px;
  }

  .cancel:disabled {
    opacity: .55;
    cursor: default;
  }

  @media (prefers-reduced-motion: reduce) {
    .shade {
      transition: none;
    }
  }
</style>
