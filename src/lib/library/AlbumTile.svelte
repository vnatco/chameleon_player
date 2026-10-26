<script lang="ts">
  // Album tile (spec D2): crisp square; hover lifts 3 px and lights the
  // tile's own four-side glow.
  import { coverUrl, mono, type AlbumRow, type Palette } from "$lib/api";
  import { lib } from "$lib/library.svelte";
  import { rgbCss } from "$lib/palette";

  let { album, onopen }: { album: AlbumRow; onopen: () => void } = $props();
  // Right-click: the album menu (Play, Shuffle, Play Next, Add to Queue).

  let pal = $state<Palette | null>(null);
  let hot = $state(false);

  function enter() {
    hot = true;
    if (album.cover && !pal) lib.palette(album.cover.hash).then((p) => { pal = p; });
  }

  const shadow = $derived.by(() => {
    if (!hot || !pal) return "0 0 0 1px var(--ch-hairline)";
    const g = pal.glow;
    return `0 0 0 1px ${rgbCss(pal.roles.edge)}, 0 -10px 26px -10px ${rgbCss(g.t, 0.9)}, 10px 0 26px -10px ${rgbCss(g.r, 0.9)}, 0 14px 32px -10px ${rgbCss(g.b, 0.95)}, -10px 0 26px -10px ${rgbCss(g.l, 0.9)}`;
  });
  const flash = $derived(lib.flash === album.key);
</script>

<button class="tile" onclick={onopen} oncontextmenu={(e) => lib.openAlbumMenu(e, album)} onmouseenter={enter} onmouseleave={() => (hot = false)} onfocus={enter} onblur={() => (hot = false)} title="{album.title} - {album.artist}">
  <span class="art" class:hot class:flash style:box-shadow={flash ? undefined : shadow} style:--flash-edge={album.cover?.edge} style:--flash-glow={album.cover?.glow}>
    {#if album.cover}
      <img src={coverUrl(album.cover.thumb)} alt="" loading="lazy" decoding="async" />
    {:else}
      <span class="mono-big">{mono(album.title)}</span>
    {/if}
  </span>
  <span class="meta">
    <span class="t ellipsis">{album.title}</span>
    <span class="a ellipsis">{album.artist}</span>
  </span>
</button>

<style>
  .tile {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
    height: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    text-align: left;
    cursor: pointer;
    color: var(--ch-text);
  }
  .art {
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 1;
    background: var(--ch-surface-2);
    transition:
      box-shadow 0.3s var(--ease-out),
      transform 0.3s var(--ease-out);
  }
  .art.hot {
    transform: translateY(-3px);
  }
  .art img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .mono-big {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-family: var(--font-display);
    font-weight: 300;
    font-size: 56px;
    color: var(--ch-text-subtle);
    opacity: 0.5;
  }
  /* A cover was just assigned: bloom in its new palette (900 ms). */
  .art.flash {
    animation: bloom 0.9s var(--ease-out) both;
  }
  @keyframes bloom {
    0% {
      box-shadow:
        0 0 0 1px var(--flash-edge),
        0 0 0 0 var(--flash-glow);
    }
    40% {
      box-shadow:
        0 0 0 1px var(--flash-edge),
        0 0 40px 10px var(--flash-glow);
    }
    100% {
      box-shadow:
        0 0 0 1px var(--flash-edge),
        0 8px 24px -8px var(--flash-glow);
    }
  }
  /* Keyboard focus: the same lift and glow as hover, no outline. */
  .tile:focus-visible {
    outline: none;
  }
  .meta {
    min-width: 0;
  }
  .t {
    display: block;
    font-size: 13px;
    font-weight: 500;
  }
  .a {
    display: block;
    font-size: 12px;
    color: var(--ch-text-subtle);
  }
</style>
