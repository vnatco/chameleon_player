<script lang="ts">
  // The play queue: every queued song, the current one highlighted. Click
  // plays, the cross removes, dragging a row reorders.
  import { coverUrl, formatTime, mono } from "$lib/api";
  import { app } from "$lib/state.svelte";
  import Icon from "$lib/ui/Icon.svelte";
  import VirtualGrid from "$lib/ui/VirtualGrid.svelte";

  let { scroller }: { scroller: HTMLElement } = $props();

  const ROW = 48;
  const items = $derived(app.queue.map((q, i) => ({ q, i })));
  const current = $derived(app.status?.track ? app.status.index : null);
  const total = $derived(app.queue.reduce((s, q) => s + (q.duration || 0), 0));
  const upcoming = $derived(current == null ? app.queue.length : app.queue.length - current - 1);

  // ---- Drag to reorder ------------------------------------------------------
  let list: HTMLDivElement | undefined = $state();
  let drag = $state<{ from: number; gap: number } | null>(null);
  let press: { i: number; y: number; id: number } | null = null;
  let dragged = false;
  let raf = 0;
  let lastY = 0;

  function gapAt(y: number) {
    if (!list) return 0;
    const top = list.getBoundingClientRect().top;
    return Math.max(0, Math.min(app.queue.length, Math.round((y - top) / ROW)));
  }

  function down(e: PointerEvent, i: number) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".rm")) return;
    press = { i, y: e.clientY, id: e.pointerId };
    dragged = false;
  }

  function move(e: PointerEvent) {
    if (!press) return;
    lastY = e.clientY;
    if (!drag) {
      if (Math.abs(e.clientY - press.y) < 5) return;
      drag = { from: press.i, gap: gapAt(e.clientY) };
      dragged = true;
      (e.currentTarget as HTMLElement).setPointerCapture(press.id);
      raf = requestAnimationFrame(autoScroll);
    }
    drag = { ...drag, gap: gapAt(e.clientY) };
  }

  // Near the top or bottom edge of the panel, scroll so far rows are reachable.
  function autoScroll() {
    if (!drag) return;
    const r = scroller.getBoundingClientRect();
    const edge = 48;
    const v = lastY < r.top + edge ? -(r.top + edge - lastY) / 3 : lastY > r.bottom - edge ? (lastY - (r.bottom - edge)) / 3 : 0;
    if (v) {
      scroller.scrollTop += v;
      drag = { ...drag, gap: gapAt(lastY) };
    }
    raf = requestAnimationFrame(autoScroll);
  }

  function up() {
    cancelAnimationFrame(raf);
    if (drag) {
      const { from, gap } = drag;
      const to = gap > from ? gap - 1 : gap;
      app.moveInQueue(from, to);
    }
    drag = null;
    press = null;
  }

  function play(i: number) {
    // A drag ends with a click on the row; that one doesn't play.
    if (dragged) {
      dragged = false;
      return;
    }
    app.cmd({ type: "playIndex", index: i });
  }
</script>

<div class="head">
  <div>
    <div class="h">Queue</div>
    <div class="subtle s">
      {app.queue.length}
      {app.queue.length === 1 ? "Song" : "Songs"} · {formatTime(total)}{current != null ? ` · ${upcoming} Up Next` : ""}
    </div>
  </div>
  {#if app.queue.length > (current == null ? 0 : 1)}
    <button class="btn secondary sm" onclick={() => app.clearQueue()} title="Removes every song except the one playing.">Clear Queue</button>
  {/if}
</div>

{#if !app.queue.length}
  <p class="subtle pad">The queue is empty. Play an album or a song, or drop music here and choose Add to Queue.</p>
{:else}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="list" class:dragging={!!drag} bind:this={list} onpointermove={move} onpointerup={up} onpointercancel={up}>
    <VirtualGrid {items} {scroller} rowHeight={ROW} key={(x) => x.i}>
      {#snippet item({ q, i })}
        {@const on = i === current}
        <div class="rowwrap" class:lifted={drag?.from === i}>
          <button class="row" class:on class:played={current != null && i < current} onpointerdown={(e) => down(e, i)} onclick={() => play(i)}>
            <span class="grip" aria-hidden="true"><Icon name="grip" size={14} /></span>
            <span class="n">{#if on}<Icon name={app.playing ? "play" : "pause"} size={12} />{:else}{i + 1}{/if}</span>
            <span class="thumb" style:box-shadow={q.cover ? `0 0 0 1px ${q.cover.edge}e6, 0 2px 10px -2px ${q.cover.glow}b3` : "0 0 0 1px var(--ch-hairline)"}>
              {#if q.cover}<img src={coverUrl(q.cover.thumb)} alt="" loading="lazy" decoding="async" />{:else}{mono(q.title)}{/if}
            </span>
            <span class="who">
              <span class="t ellipsis">{q.title}</span>
              <span class="a ellipsis">{[q.artist, q.album].filter(Boolean).join(" · ")}</span>
            </span>
            <span class="d">{formatTime(q.duration)}</span>
          </button>
          <button class="rm" onclick={() => app.removeFromQueue([i])} aria-label="Remove From Queue" title="Remove From Queue">
            <Icon name="close" size={12} stroke={2} />
          </button>
        </div>
      {/snippet}
    </VirtualGrid>
    {#if drag}
      <div class="gap" style:transform="translateY({drag.gap * ROW - 1}px)"></div>
    {/if}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 14px;
  }
  .h {
    font-family: var(--font-display);
    font-size: 24px;
    font-weight: 600;
  }
  .s {
    font-size: 12.5px;
    margin-top: 2px;
  }
  .pad {
    padding: 8px 0;
    font-size: 13px;
  }
  .list {
    position: relative;
    touch-action: none;
  }
  .list.dragging {
    cursor: grabbing;
  }
  .rowwrap {
    position: relative;
    height: 100%;
  }
  .rowwrap.lifted {
    opacity: 0.45;
  }
  .row {
    display: grid;
    grid-template-columns: 16px 26px 34px minmax(0, 1fr) 48px;
    gap: 0 12px;
    align-items: center;
    width: 100%;
    height: 100%;
    padding: 0 40px 0 6px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--ch-text);
    text-align: left;
    cursor: pointer;
  }
  .row:hover {
    background: var(--ch-hover);
  }
  /* Keyboard focus on a row: a quiet highlight, not the global outline. */
  .row:focus-visible {
    outline: none;
    background: var(--ch-hover);
  }
  .row.on {
    background: var(--ch-accent-soft);
  }
  .row.on .t,
  .row.on .n {
    color: var(--ch-accent);
  }
  .row.played .who,
  .row.played .thumb {
    opacity: 0.6;
  }
  .grip {
    color: var(--ch-text-subtle);
    opacity: 0;
    cursor: grab;
  }
  .row:hover .grip {
    opacity: 1;
  }
  .n {
    font-size: 12px;
    color: var(--ch-text-subtle);
    font-variant-numeric: tabular-nums;
    text-align: right;
    display: flex;
    justify-content: flex-end;
  }
  .thumb {
    width: 34px;
    height: 34px;
    background: var(--ch-surface-2);
    overflow: hidden;
    display: grid;
    place-items: center;
    font-size: 12px;
    color: var(--ch-text-subtle);
  }
  .thumb img {
    width: 34px;
    height: 34px;
    object-fit: cover;
    display: block;
  }
  .who {
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .t {
    font-size: 13px;
  }
  .a {
    font-size: 11.5px;
    color: var(--ch-text-subtle);
  }
  .d {
    font-size: 12px;
    color: var(--ch-text-subtle);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .rm {
    position: absolute;
    right: 8px;
    top: 50%;
    width: 26px;
    height: 26px;
    margin-top: -13px;
    padding: 7px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--ch-text-subtle);
    cursor: pointer;
    opacity: 0;
  }
  .rowwrap:hover .rm,
  .rm:focus-visible {
    opacity: 1;
  }
  .rm:hover {
    background: var(--ch-hover);
    color: var(--ch-text);
  }
  .dragging .rm {
    opacity: 0;
  }
  .dragging .row:not(.on):hover {
    background: transparent;
  }
  .gap {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 2px;
    border-radius: 1px;
    background: var(--ch-accent);
    box-shadow: 0 0 8px var(--ch-accent);
    pointer-events: none;
  }
</style>
