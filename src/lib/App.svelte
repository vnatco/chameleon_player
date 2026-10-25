<script lang="ts">
  // App shell: bootstrap, events, keyboard, drag & drop, and the window
  // morph between the square player and the library.
  //
  // The player window has a fixed size (the largest sleeve plus its glow
  // margin), so size changes are pure CSS. Player <-> library keeps the
  // window's top-left fixed, so the cover never jumps on screen when the OS
  // window resizes; the frame and cover then move with GPU transforms only.
  import { onMount, tick } from "svelte";
  import { availableMonitors } from "@tauri-apps/api/window";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { api, events, type Rect } from "$lib/api";
  import { lib } from "$lib/library.svelte";
  import { PaletteDriver, prepare } from "$lib/palette";
  import { kick } from "$lib/progress";
  import { app, LIB_DEFAULT, MARGINS, PLAYER_WIN, RAIL_COVER, SIZES, win } from "$lib/state.svelte";
  import Library from "$lib/library/Library.svelte";
  import Sleeve, { type DropState } from "$lib/sleeve/Sleeve.svelte";
  import Icon from "$lib/ui/Icon.svelte";
  import ContextMenu, { type MenuItem } from "$lib/ui/ContextMenu.svelte";
  import { renderFallback, updateShell } from "$lib/shell";
  import DefaultPrompt from "$lib/ui/DefaultPrompt.svelte";

  const MORPH_MS = 460;
  const LIB_ZONES = [
    ["Play Now", "Replaces the Queue"],
    ["Add to Queue", "Skips Songs Already Queued"],
    ["Add to Library", "Folders are watched for new files."],
  ] as const;
  const IMAGE_EXT = /\.(jpe?g|png|webp|bmp|gif)$/i;
  const AUDIO_EXT = /\.(mp3|m4a|m4b|mp4|aac|flac|ogg|oga|wav|aiff?|aifc)$/i;

  let pointerInside = $state(true);
  let drop = $state<DropState>(null);
  /** Drop zone under the pointer while files are dragged over the library. */
  let libDrop = $state<number | null>(null);
  let libFrame = $state({ w: LIB_DEFAULT.w, h: LIB_DEFAULT.h });
  let mica = $state(false);
  /** Morph phase: "start" = old geometry, no transition; "run" = animating to the new one. */
  let morph = $state<{ dir: "expand" | "collapse"; phase: "start" | "run"; from: Rect } | null>(null);
  let library: Library | undefined = $state();
  let menu = $state<{ x: number; y: number } | null>(null);
  // The default-player prompt waits a moment after launch, and for a
  // sleeve big enough to hold it.
  let promptReady = $state(false);
  const showPrompt = $derived(
    promptReady &&
      !app.defaultPromptDone &&
      app.ui.defaultPrompt !== "never" &&
      !!app.defaults?.registered &&
      !app.defaults.isDefault &&
      !morph &&
      (app.mode === "library" || (app.ui.size !== "mini" && !app.flipped)),
  );
  let menuRect = $state<Rect | null>(null);

  const S = $derived(SIZES[app.ui.size]);
  const off = $derived((PLAYER_WIN - S) / 2);
  const playerRect = $derived<Rect>({ x: off, y: off, w: S, h: S });
  // The library stays mounted while it collapses and fades out as the frame
  // closes around the cover, instead of vanishing in the first frame.
  const showLibrary = $derived(app.mode === "library");
  const closing = $derived(morph?.dir === "collapse");


  // ---- Sleeve geometry ------------------------------------------------------
  const sleeveStyle = $derived.by(() => {
    const lib = app.mode === "library" && morph?.dir !== "collapse";
    const r = lib ? { x: RAIL_COVER.x, y: RAIL_COVER.y, w: RAIL_COVER.s, h: RAIL_COVER.s } : playerRect;
    let transform = "none";
    let transition = app.anim && !morph ? `left ${MORPH_MS}ms var(--ease-out), top ${MORPH_MS}ms var(--ease-out), width ${MORPH_MS}ms var(--ease-out), height ${MORPH_MS}ms var(--ease-out)` : "none";
    if (morph) {
      // FLIP: start where the cover was, animate to where it is.
      const from = morph.dir === "expand" ? morph.from : { x: RAIL_COVER.x, y: RAIL_COVER.y, w: RAIL_COVER.s, h: RAIL_COVER.s };
      if (morph.phase === "start") transform = `translate(${from.x - r.x}px, ${from.y - r.y}px) scale(${from.w / r.w})`;
      transition = morph.phase === "run" && app.anim ? `transform ${MORPH_MS}ms var(--ease-out)` : "none";
    }
    return `left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;transform:${transform};transition:${transition}`;
  });

  // The morph scales the sleeve with a transform, which would scale its glow
  // too. In the first frame the glow is drawn so that, scaled, it looks
  // exactly as before; from there it eases to its new look with the morph.
  // The player's glow also fades out before the window edge (`edge`, in the
  // sleeve's own units, so it scales along with it).
  const sleeveGlow = $derived.by((): { as: "player" | "library" | null; scale: number; instant: boolean; edge: number | null } => {
    if (!morph) return { as: null, scale: 1, instant: false, edge: app.mode === "player" ? off : null };
    const k = RAIL_COVER.s / S;
    if (morph.phase === "start") {
      return morph.dir === "expand"
        ? { as: "player", scale: k, instant: true, edge: off * k }
        : { as: "library", scale: 1 / k, instant: true, edge: null };
    }
    return morph.dir === "expand"
      ? { as: "library", scale: 1, instant: false, edge: null }
      : { as: "player", scale: 1, instant: false, edge: off };
  });

  // The library frame grows from the cover (expand) or shrinks into it
  // (collapse) by animating a clip, so its content is never scaled or
  // distorted, only revealed, like a window opening.
  const frameStyle = $derived.by(() => {
    const full = !morph;
    const w = full ? "100vw" : `${libFrame.w}px`;
    const h = full ? "100vh" : `${libFrame.h}px`;
    let clip = "inset(0 round 8px)";
    let transition = "none";
    if (morph) {
      const atCover = (morph.dir === "expand") === (morph.phase === "start");
      const c = morph.dir === "expand" ? morph.from : playerRect;
      if (atCover) clip = `inset(${c.y}px ${libFrame.w - c.x - c.w}px ${libFrame.h - c.y - c.h}px ${c.x}px round 0px)`;
      if (morph.phase === "run" && app.anim) transition = `clip-path ${MORPH_MS}ms var(--ease-out)`;
    }
    return `width:${w};height:${h};clip-path:${clip};transition:${transition}`;
  });

  // ---- Click-through regions --------------------------------------------------
  $effect(() => {
    const rects: Rect[] = app.mode === "library" || morph ? [{ x: 0, y: 0, w: 10000, h: 10000 }] : [playerRect];
    if (menu && menuRect) rects.push(menuRect);
    api.windowHit(rects).catch(() => {});
  });

  // ---- Expand / collapse -----------------------------------------------------
  const frames = (n: number) =>
    new Promise<void>((done) => {
      const step = () => (--n <= 0 ? done() : requestAnimationFrame(step));
      requestAnimationFrame(step);
    });

  /** Resolve once the page has the window's new size (or after a short wait,
   * so a missed resize event can't hang the morph). */
  function viewport(w: number, h: number) {
    return new Promise<void>((done) => {
      const fits = () => Math.abs(window.innerWidth - w) <= 1 && Math.abs(window.innerHeight - h) <= 1;
      if (fits()) return done();
      const finish = () => {
        clearTimeout(timer);
        window.removeEventListener("resize", check);
        done();
      };
      const check = () => fits() && finish();
      const timer = setTimeout(finish, 300);
      window.addEventListener("resize", check);
    });
  }

  async function expand() {
    if (app.mode === "library" || morph) return;
    if (app.flipped) {
      app.flipped = false;
      app.applyPalette();
    }
    const f = await app.guard(api.windowFrame());
    if (!f) return;
    const saved = app.ui.remember ? app.ui.libFrame : undefined;
    const w = Math.min(saved?.w ?? LIB_DEFAULT.w, f.work.w);
    const h = Math.min(saved?.h ?? LIB_DEFAULT.h, f.work.h);
    // The library keeps the player's top-left, nudged onto the screen if needed.
    const x = Math.max(f.work.x, Math.min(f.x, f.work.x + f.work.w - w));
    const y = Math.max(f.work.y, Math.min(f.y, f.work.y + f.work.h - h));
    libFrame = { w, h };
    morph = { dir: "expand", phase: "start", from: playerRect };
    app.mode = "library";
    app.applyPalette();
    await tick();
    // Grow in place first. Moving the window's origin while the content
    // shifts back the other way can't happen in one frame: for a frame or
    // two the screen shows the moved window with the old content, and the
    // cover flicks sideways. A nudge onto the screen glides the whole window
    // instead, together with its content, while the morph runs.
    await app.guard(api.windowSetFrame({ x: f.x, y: f.y, w, h }));
    await viewport(w, h);
    // Start only once a frame at the new size is on screen.
    await frames(2);
    if (!morph) return;
    morph = { ...morph, phase: "run" };
    if (x !== f.x || y !== f.y) {
      if (app.anim) app.guard(api.windowGlide(x, y, MORPH_MS));
      else app.guard(api.windowSetFrame({ x, y, w, h }));
    }
    setTimeout(
      async () => {
        morph = null;
        try {
          await api.windowChrome(true, true);
          mica = true;
        } catch (e) {
          // Windows 10 or transparency off: solid background instead of Mica.
          mica = false;
          api.windowChrome(true, false).catch(() => {});
          console.warn("mica unavailable", e);
        }
      },
      app.anim ? MORPH_MS + 40 : 0,
    );
  }

  async function collapse() {
    if (app.mode !== "library" || morph) return;
    let f = await app.guard(api.windowFrame());
    if (!f) return;
    if (f.maximized) f = (await app.guard(api.windowToggleMaximize())) ?? f;
    else if (app.ui.remember) app.setUi("libFrame", { x: f.x, y: f.y, w: f.w, h: f.h });
    // Paint the opaque background before Mica goes, or for a frame or two the
    // see-through Mica tint has nothing behind it but the desktop.
    mica = false;
    await tick();
    await frames(2);
    await app.guard(api.windowChrome(false, false));
    libFrame = { w: f.w, h: f.h };
    morph = { dir: "collapse", phase: "start", from: playerRect };
    app.applyPalette();
    await tick();
    await frames(2);
    if (morph) morph = { ...morph, phase: "run" };
    setTimeout(
      async () => {
        await app.guard(api.windowSetFrame({ x: f!.x, y: f!.y, w: PLAYER_WIN, h: PLAYER_WIN }));
        app.mode = "player";
        morph = null;
        app.applyPalette();
      },
      app.anim ? MORPH_MS + 20 : 0,
    );
  }

  function toggleLibrary() {
    if (app.mode === "library") collapse();
    else expand();
  }

  function editFromLibrary() {
    collapse();
    setTimeout(() => app.flip("edit"), (app.anim ? MORPH_MS : 0) + 80);
  }

  // ---- Right-click menu ------------------------------------------------------
  let menuBounds = $state({ x0: 0, y0: 0, x1: 860, y1: 860 });
  async function onContextMenu(e: MouseEvent) {
    // A song row opened its own menu.
    if (e.defaultPrevented) return;
    const t = e.target as HTMLElement;
    // Text fields keep the system menu (cut, copy, paste).
    if (t.closest("input, textarea, [contenteditable]")) return;
    e.preventDefault();
    if (app.mode === "player") {
      const r = playerRect;
      if (e.clientX < r.x || e.clientY < r.y || e.clientX >= r.x + r.w || e.clientY >= r.y + r.h) return;
    }
    menuRect = null;
    const x = e.clientX;
    const y = e.clientY;
    // The window may hang off the screen edge (its glow margin does when the
    // sleeve sits near it); keep the menu on the visible part.
    const f = await api.windowFrame().catch(() => null);
    const w = window.innerWidth;
    const h = window.innerHeight;
    menuBounds = f
      ? {
          x0: Math.max(0, f.work.x - f.x),
          y0: Math.max(0, f.work.y - f.y),
          x1: Math.min(w, f.work.x + f.work.w - f.x),
          y1: Math.min(h, f.work.y + f.work.h - f.y),
        }
      : { x0: 0, y0: 0, x1: w, y1: h };
    menu = { x, y };
  }
  const menuItems = $derived.by((): MenuItem[] => {
    const player = app.mode === "player";
    const hasTrack = !!app.status?.track;
    const items: MenuItem[] = [
      { kind: "item", label: app.playing ? "Pause" : "Play", hint: "Space", disabled: !app.status?.queueLen, action: () => app.toggle() },
      { kind: "item", label: "Next", hint: "N", disabled: !hasTrack, action: () => app.next() },
      { kind: "item", label: "Previous", disabled: !hasTrack, action: () => app.prev() },
      { kind: "sep" },
    ];
    if (player) {
      items.push(
        { kind: "label", label: "Window Size" },
        { kind: "item", label: "Mini", hint: "1", checked: app.ui.size === "mini", action: () => app.setSize("mini") },
        { kind: "item", label: "Normal", hint: "2", checked: app.ui.size === "normal", action: () => app.setSize("normal") },
        { kind: "item", label: "Large", hint: "3", checked: app.ui.size === "large", action: () => app.setSize("large") },
        { kind: "sep" },
        { kind: "item", label: app.flipped ? "Flip to Cover" : "Flip to Back", hint: "F", action: () => app.flip() },
        { kind: "item", label: "Edit Tags", disabled: !hasTrack, action: () => (app.flipped ? (app.backTab = "edit") : app.flip("edit")) },
        { kind: "item", label: "Open Library", hint: "L", action: () => expand() },
      );
    } else {
      items.push({ kind: "item", label: "Back to Player", hint: "Esc", action: () => collapse() });
    }
    items.push(
      { kind: "sep" },
      { kind: "item", label: "Always on Top", checked: app.ui.onTop, action: () => app.setUi("onTop", !app.ui.onTop) },
    ...(app.defaults?.registered && !app.defaults.isDefault
      ? [{ kind: "item" as const, label: "Set as Default Player…", action: () => app.setAsDefault() }]
      : []),
      { kind: "item", label: "Minimize", action: () => win.minimize() },
      { kind: "item", label: "Close", action: () => win.close() },
    );
    return items;
  });

  // ---- Song menu (library rows) ---------------------------------------------
  const trackMenuItems = $derived.by((): MenuItem[] => {
    const tm = lib.trackMenu;
    if (!tm) return [];
    const tracks = tm.tracks;
    const one = tracks.length === 1 ? tracks[0] : null;
    const done = () => lib.clearSelection();
    const items: MenuItem[] = [
      { kind: "item", label: one ? "Play" : `Play ${tracks.length} Songs`, action: () => (one ? app.playOne(one) : app.playList(tracks, 0), done()) },
      { kind: "item", label: "Play Next", action: () => (app.enqueue(tracks, true), done()) },
      { kind: "item", label: "Add to Queue", action: () => (app.enqueue(tracks, false), done()) },
    ];
    if (one?.albumKey && !(app.libView === "album" && app.albumKey === one.albumKey)) {
      const key = one.albumKey;
      items.push(
        { kind: "sep" },
        {
          kind: "item",
          label: "Go to Album",
          action: () => {
            done();
            app.libView = "album";
            app.albumKey = key;
            lib.openAlbum(key);
          },
        },
      );
    }
    return items;
  });

  // ---- Keyboard (spec H) ----------------------------------------------------
  function onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    const typing = t.tagName === "INPUT" && (t as HTMLInputElement).type !== "range";
    if (e.key === "Escape") {
      if (library?.escape()) return;
      if (typing) {
        t.blur();
        return;
      }
      if (app.flipped) return app.flip();
      if (app.mode === "library") return collapse();
      return;
    }
    if (typing || e.ctrlKey || e.altKey || e.metaKey) return;
    switch (e.key) {
      case " ":
        if (t.tagName === "BUTTON") return;
        e.preventDefault();
        app.toggle();
        break;
      case "ArrowRight":
        if (t.getAttribute("role") === "slider" || t.tagName === "INPUT") return;
        app.seek(app.positionNow() + 5);
        break;
      case "ArrowLeft":
        if (t.getAttribute("role") === "slider" || t.tagName === "INPUT") return;
        app.seek(app.positionNow() - 5);
        break;
      case "ArrowUp":
        e.preventDefault();
        app.setVolume((app.status?.volume ?? 0) + 0.05);
        break;
      case "ArrowDown":
        e.preventDefault();
        app.setVolume((app.status?.volume ?? 0) - 0.05);
        break;
      case "n":
      case "N":
        app.next();
        break;
      case "f":
      case "F":
        app.flip();
        break;
      case "l":
      case "L":
        toggleLibrary();
        break;
      case "1":
        if (app.mode === "player") app.setSize("mini");
        break;
      case "2":
        if (app.mode === "player") app.setSize("normal");
        break;
      case "3":
        if (app.mode === "player") app.setSize("large");
        break;
    }
  }

  // ---- Drag & drop ------------------------------------------------------------
  function dropTarget(paths: string[], x: number, y: number) {
    const single = paths.length === 1 && IMAGE_EXT.test(paths[0]);
    if (app.mode === "library") {
      lib.dropTile = null;
      if (single && app.libView === "missing") {
        const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-album-key]");
        lib.dropTile = el?.dataset.albumKey ?? null;
      }
      // An image goes on a missing-cover tile, never into the queue.
      libDrop = single ? null : Math.min(2, Math.floor((x / window.innerWidth) * 3));
      return;
    }
    const r = playerRect;
    const inside = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
    if (!inside || app.flipped) {
      drop = null;
      return;
    }
    // Mini has one zone (Play); the others three: Play Now, Add to Queue, Add to Library.
    drop = { kind: single ? "image" : "files", zone: app.ui.size === "mini" ? 0 : Math.min(2, Math.floor(((y - r.y) / r.h) * 3)) };
  }

  /** The library folder that already holds `path`, if any. */
  function libraryFolderOf(path: string) {
    const norm = (p: string) => p.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
    const p = norm(path);
    return app.folders.find((f) => {
      const root = norm(f.path);
      return p === root || p.startsWith(root + "\\");
    });
  }

  /** Watch dropped folders (or the folders of dropped songs). Ones the library
   * already covers are fine, not an error: their songs are already in it. */
  async function addToLibrary(folders: string[], fromSongs = false) {
    const fresh = folders.filter((f) => !libraryFolderOf(f));
    if (!fresh.length) {
      app.notify(fromSongs ? "These Songs Are Already in Your Library" : "Already in Your Library");
      return;
    }
    for (const f of fresh) {
      const added = await app.guard(api.addFolder(f));
      if (added) {
        app.scanning = true;
        app.notify(`Added ${added.path} to Your Library`);
      }
    }
  }

  async function setCoverFromImage(image: string) {
    const now = app.now;
    if (!now) {
      app.notify("Play a song first, then drop its cover on the sleeve.", true);
      return;
    }
    if (!app.ui.writeCovers && now.inLibrary) {
      const tags = await app.guard(api.readTags(now.path));
      if (!tags) return;
      await app.guard(api.updateTrackOnly(now.path, { ...tags, cover: { action: "replace", path: image } }));
      app.notify(`Cover Set for ${now.album || now.title} in Your Library`);
      return;
    }
    const tags = await app.guard(api.readTags(now.path));
    if (!tags) return;
    if (tags.readOnly) {
      if (now.inLibrary) {
        await app.guard(api.updateTrackOnly(now.path, { ...tags, cover: { action: "replace", path: image } }));
        app.notify("The file is read-only, so the cover was saved to your library only.");
      } else {
        app.notify("This file is read-only, so its cover can't be changed.", true);
      }
      return;
    }
    const ok = await app.guard(api.writeTags(now.path, { ...tags, cover: { action: "replace", path: image } }));
    if (ok !== undefined) app.notify(`Cover Set for ${now.album || now.title}`);
  }

  async function onDrop(paths: string[], x: number, y: number) {
    const single = paths.length === 1 && IMAGE_EXT.test(paths[0]);
    const z = drop;
    drop = null;
    const folders = paths.filter((p) => !/\.[a-z0-9]{2,5}$/i.test(p.split(/[\\/]/).pop() ?? ""));
    const audio = paths.filter((p) => AUDIO_EXT.test(p));

    if (app.mode === "library") {
      const key = lib.dropTile;
      const zone = libDrop;
      lib.dropTile = null;
      libDrop = null;
      if (single && key) {
        const a = lib.missingAlbums.find((m) => m.key === key);
        if (a) lib.assign(a, paths[0]);
        return;
      }
      if (zone != null) dropFiles(zone, folders, audio);
      return;
    }
    if (app.flipped) {
      if (single && app.backTab === "edit") window.dispatchEvent(new CustomEvent("editor-image", { detail: paths[0] }));
      return;
    }
    if (!z) return;
    if (z.kind === "image") return setCoverFromImage(paths[0]);
    dropFiles(z.zone, folders, audio);
  }

  /** Dropped songs and folders, by zone: 0 Play Now (replaces the queue),
   * 1 Add to Queue (skips songs already queued), 2 Add to Library. */
  async function dropFiles(zone: number, folders: string[], audio: string[]) {
    const media = [...folders, ...audio];
    if (!media.length) {
      app.notify("These Files Can't Be Played", true);
      return;
    }
    if (zone === 0) {
      const n = await app.guard(api.openPaths(media));
      if (n) app.notify(`Playing ${n} ${n === 1 ? "Song" : "Songs"}`);
    } else if (zone === 1) {
      await app.enqueuePaths(media, false);
    } else {
      if (folders.length) await addToLibrary(folders);
      if (audio.length) await addToLibrary([...new Set(audio.map((p) => p.replace(/[\\/][^\\/]+$/, "")))], true);
    }
  }

  // ---- Boot -----------------------------------------------------------------
  onMount(() => {
    const root = document.documentElement;
    app.driver = new PaletteDriver(root);
    const mq = matchMedia("(prefers-reduced-motion: reduce)");
    app.reduceMotion = mq.matches;
    mq.onchange = () => (app.reduceMotion = mq.matches);

    let lastPaths: string[] = [];
    const unsubs: Promise<() => void>[] = [
      events.player((e) => {
        if (e.type === "status") {
          const { type: _t, ...s } = e;
          app.onStatus(s);
          kick();
        } else if (e.type === "position") {
          app.onPosition(e.position, e.duration);
          kick();
        } else if (e.type === "level") {
          app.level = e.value;
        } else if (e.type === "error") {
          app.notify(e.path ? `${e.path.split(/[\\/]/).pop()}: ${e.message}` : e.message, true);
        }
      }),
      events.scanProgress((p) => {
        app.scanning = true;
        app.progress = p;
      }),
      events.scanDone((d) => {
        app.scanning = false;
        app.progress = null;
        if (d.error) app.notify(`Scan failed: ${d.error}`, true);
        if (d.summary) {
          app.summary = d.summary;
          const s = d.summary;
          if (s.added || s.removed) app.notify(`Library Updated: ${s.added} New, ${s.removed} Removed`);
        }
      }),
      events.libraryChanged((paths) => {
        app.libraryVersion++;
        app.guard(api.stats()).then((s) => { if (s) app.stats = s; });
        const cur = app.now?.path;
        if (!paths.length || (cur && paths.includes(cur))) app.refreshNow();
        if (lib.album) lib.refreshAlbum();
      }),
      events.appError((m) => app.notify(m, true)),
      events.pointerInside((inside) => (pointerInside = inside)),
      getCurrentWebview().onDragDropEvent((e) => {
        const p = e.payload;
        const dpr = window.devicePixelRatio || 1;
        if (p.type === "enter") {
          lastPaths = p.paths;
          dropTarget(p.paths, p.position.x / dpr, p.position.y / dpr);
        } else if (p.type === "over") dropTarget(lastPaths, p.position.x / dpr, p.position.y / dpr);
        else if (p.type === "drop") onDrop(p.paths, p.position.x / dpr, p.position.y / dpr);
        else {
          drop = null;
          libDrop = null;
          lib.dropTile = null;
        }
      }),
    ];

    (async () => {
      const b = await app.guard(api.bootstrap());
      if (!b) {
        await api.windowShow().catch(() => {});
        return;
      }
      app.fallback = b.fallback;
      app.loadUi(b.settings.ui);
      app.coverSource = b.settings.coverSource;
      app.stats = b.stats;
      app.scanning = b.scanning;
      root.dataset.motion = app.anim ? "on" : "off";
      app.driver!.set(prepare(b.fallback), 0);

      // Always start as the square player, whatever state the window was
      // left in (e.g. a reload while the library was open).
      await api.windowChrome(false, false).catch(() => {});
      const start = await app.guard(api.windowFrame());
      if (start && (Math.round(start.w) !== PLAYER_WIN || Math.round(start.h) !== PLAYER_WIN)) {
        await app.guard(api.windowSetFrame({ x: start.x, y: start.y, w: PLAYER_WIN, h: PLAYER_WIN }));
      }

      // Restore the window where it was, if that spot is still on a screen.
      if (app.ui.remember && app.ui.playerPos) {
        const { x, y } = app.ui.playerPos;
        const mons = await availableMonitors().catch(() => []);
        const cx = x + PLAYER_WIN / 2;
        const cy = y + PLAYER_WIN / 2;
        const onScreen = mons.some((m) => {
          const s = m.scaleFactor;
          const wx = m.workArea.position.x / s;
          const wy = m.workArea.position.y / s;
          return cx >= wx && cy >= wy && cx < wx + m.workArea.size.width / s && cy < wy + m.workArea.size.height / s;
        });
        if (onScreen) await app.guard(api.windowSetFrame({ x, y, w: PLAYER_WIN, h: PLAYER_WIN }));
      }
      await app.guard(api.windowOnTop(app.ui.onTop));
      if (app.ui.pulse) app.cmd({ type: "setMeter", enabled: true });
      app.onStatus(b.status);
      app.guard(api.folders()).then((f) => { if (f) app.folders = f; });
      app.refreshDefaults();
      setTimeout(() => (promptReady = true), 2500);
      app.ready = true;
      // Show the window only once the first frame is painted (no flash).
      requestAnimationFrame(() => requestAnimationFrame(() => api.windowShow().catch(() => {})));
    })();

    // Remember where the player sits.
    let moveTimer: ReturnType<typeof setTimeout> | undefined;
    unsubs.push(
      win.onMoved(() => {
        clearTimeout(moveTimer);
        moveTimer = setTimeout(async () => {
          if (!app.ui.remember || morph) return;
          const f = await api.windowFrame().catch(() => null);
          if (!f) return;
          if (app.mode === "player") app.setUi("playerPos", { x: f.x, y: f.y });
          else if (!f.maximized) app.setUi("libFrame", { x: f.x, y: f.y, w: f.w, h: f.h });
        }, 400);
      }),
      // Back from Windows Settings: see whether we became the default.
      win.onFocusChanged(({ payload: focused }) => {
        if (focused) app.refreshDefaults();
      }),
      win.onResized(() => {
        if (app.mode === "library" && !morph) libFrame = { w: window.innerWidth, h: window.innerHeight };
      }),
    );

    return () => unsubs.forEach((u) => u.then((f) => f()));
  });

  $effect(() => {
    document.documentElement.dataset.motion = app.anim ? "on" : "off";
  });

  // Tray, taskbar thumbnail and media overlay follow the track.
  $effect(() => {
    if (!app.ready) return;
    const now = app.now;
    const pal = app.palette ?? app.fallback;
    if (!pal) return;
    const inLib = app.mode === "library";
    const coverRect = inLib ? { x: RAIL_COVER.x, y: RAIL_COVER.y, w: RAIL_COVER.s, h: RAIL_COVER.s } : playerRect;
    updateShell({
      path: now?.path ?? null,
      title: now?.title ?? "",
      artist: now?.artist ?? "",
      accent: pal.roles.accent,
      playing: app.playing,
      cover: now?.cover?.full ?? null,
      fallbackPng: now && !now.cover ? renderFallback(now.title, now.artist) : null,
      coverRect,
    });
  });
</script>

<svelte:window onkeydown={onKey} oncontextmenu={onContextMenu} />

<div class="stage" class:ready={app.ready}>
  {#if app.mode === "library" || morph}
    <div class="frame" class:mica class:closing class:round={app.mode === "library" && !morph} style={frameStyle}>
      {#if showLibrary}
        <Library bind:this={library} oncollapse={collapse} onedit={editFromLibrary} />
      {/if}
    </div>
  {/if}

  <div class="sleeve-pos" style={sleeveStyle}>
    <Sleeve inLibrary={app.mode === "library" && morph?.dir !== "collapse"} {drop}
      {pointerInside}
      onlibrary={expand}
      glowAs={sleeveGlow.as}
      glowScale={sleeveGlow.scale}
      glowInstant={sleeveGlow.instant}
      glowEdge={sleeveGlow.edge}
    />
  </div>


  {#if showPrompt}
    <div
      class="prompt"
      style={app.mode === "library"
        ? "right:20px;bottom:20px;width:340px"
        : `left:${playerRect.x + 12}px;width:${playerRect.w - 24}px;bottom:${PLAYER_WIN - (playerRect.y + playerRect.h) + 12}px`}
    >
      <DefaultPrompt />
    </div>
  {/if}

  {#if libDrop != null && app.mode === "library" && !morph}
    <div class="libdrop">
      {#each LIB_ZONES as [label, sub], i (label)}
        <div class="lzone" class:hot={libDrop === i}>
          <span class="lzi"><Icon name="drop" /></span>
          <span class="lzl">{label}</span>
          <span class="lzs">{sub}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if lib.trackMenu}
    <ContextMenu
      x={lib.trackMenu.x}
      y={lib.trackMenu.y}
      items={trackMenuItems}
      bounds={{ x0: 0, y0: 0, x1: window.innerWidth, y1: window.innerHeight }}
      onclose={() => (lib.trackMenu = null)}
      onrect={() => {}}
    />
  {/if}

  {#if menu}
    <ContextMenu
      x={menu.x}
      y={menu.y}
      items={menuItems}
      bounds={menuBounds}
      onclose={() => {
        menu = null;
        menuRect = null;
      }}
      onrect={(r) => (menuRect = r)}
    />
  {/if}

  {#if app.toast}
    {#key app.toast.id}
      <div
        class="toast"
        class:err={app.toast.error}
        role="status"
        style:bottom={app.mode === "library" ? "24px" : `${PLAYER_WIN - (off + S) + 14}px`}
        style:max-width={app.mode === "library" ? null : `${Math.max(S - 24, 136)}px`}
      >
        {#if app.toast.error}<span class="dot"></span>{:else}<span class="ok"><Icon name="check" size={14} stroke={2.4} /></span>{/if}
        <span>{app.toast.text}</span>
      </div>
    {/key}
  {/if}
</div>

<style>
  .stage {
    position: fixed;
    inset: 0;
    overflow: hidden;
    opacity: 0;
  }
  .stage.ready {
    opacity: 1;
  }
  .frame {
    position: absolute;
    left: 0;
    top: 0;
    background: var(--ch-bg);
    box-shadow: inset 0 0 0 1px var(--ch-hairline);
    overflow: hidden;
    will-change: clip-path;
  }
  .frame.round {
    border-radius: 8px;
  }
  .frame.mica {
    background: var(--ch-mica);
    transition: background-color 0.2s;
  }
  .frame.closing {
    pointer-events: none;
  }
  .frame.closing > :global(.lib) {
    animation: libOut 0.22s var(--ease-out) both;
  }
  @keyframes libOut {
    to {
      opacity: 0;
    }
  }
  .sleeve-pos {
    position: absolute;
    transform-origin: 0 0;
    z-index: 2;
  }
  .prompt {
    position: absolute;
    z-index: 8;
  }
  .libdrop {
    position: absolute;
    inset: 0;
    z-index: 9;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
    padding: 56px 20px 20px;
    border-radius: 8px;
    background: var(--ch-scrim);
    animation: chFade 0.14s ease-out both;
    pointer-events: none;
  }
  .lzone {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 1.5px dashed var(--ch-text-subtle);
    border-radius: 4px;
    text-align: center;
    padding: 12px;
    color: var(--ch-text);
    transition:
      background-color 0.12s,
      border-color 0.12s;
  }
  .lzone.hot {
    border-color: var(--ch-accent);
    background: var(--ch-accent-soft);
  }
  .lzi {
    width: 28px;
    height: 28px;
  }
  .lzl {
    font-size: 18px;
    font-weight: 600;
  }
  .lzs {
    font-size: 13px;
    color: var(--ch-text-subtle);
  }
  .toast {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: min(560px, 90vw);
    padding: 8px 14px;
    border-radius: 4px;
    background: var(--ch-surface-2);
    color: var(--ch-text);
    font-size: 12.5px;
    box-shadow:
      0 0 0 1px var(--ch-hairline),
      0 10px 30px rgb(0 0 0 / 0.35);
    animation: toastIn 0.2s var(--ease-out) both;
    pointer-events: none;
    z-index: 10;
  }
  .toast.err {
    box-shadow:
      0 0 0 1px var(--ch-danger),
      0 10px 30px rgb(0 0 0 / 0.35);
  }
  .toast .ok {
    color: var(--ch-accent);
    display: grid;
  }
  .toast .dot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--ch-danger);
    flex: none;
  }
  @keyframes toastIn {
    from {
      opacity: 0;
      transform: translate(-50%, 8px);
    }
    to {
      opacity: 1;
      transform: translate(-50%, 0);
    }
  }
</style>
