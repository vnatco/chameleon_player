// Library data for the library views: loaded lazily per view, cached, and
// reloaded when the backend reports changes.

import { api, type AlbumRow, type Candidate, type NamedCount, type Palette, type TrackRow } from "./api";
import { app } from "./state.svelte";

export type MissState =
  | { st: "idle" }
  | { st: "searching" }
  | { st: "found"; cand: Candidate }
  | { st: "none"; reason?: string }
  | { st: "assigning" }
  | { st: "assigned" };

class LibraryData {
  albums = $state<AlbumRow[] | null>(null);
  artists = $state<NamedCount[] | null>(null);
  genres = $state<NamedCount[] | null>(null);
  songs = $state<TrackRow[] | null>(null);
  album = $state<{ row: AlbumRow; tracks: TrackRow[]; palette: Palette | null } | null>(null);
  missing = $state<Record<string, MissState>>({});
  /** Missing-cover tile currently under a dragged image. */
  dropTile = $state<string | null>(null);
  /** Album key whose tile just received a cover (bloom animation). */
  flash = $state<string | null>(null);
  finding = $state(false);

  // ---- Song selection (Songs and album track lists) -------------------------
  /** The list the selection belongs to, so actions keep its order. */
  private selScope: TrackRow[] = [];
  private anchor: number | null = null;
  selIds = $state<Set<number>>(new Set());
  /** Right-click menu for songs, in window coordinates. */
  trackMenu = $state<{ x: number; y: number; tracks: TrackRow[] } | null>(null);

  get selected(): TrackRow[] {
    return this.selScope.filter((t) => this.selIds.has(t.id));
  }

  clearSelection() {
    if (this.selIds.size) this.selIds = new Set();
    this.anchor = null;
  }

  /** Ctrl+click toggles a song, Shift+click selects a range. Returns false
   * for a plain click, which plays the song as before. */
  select(e: MouseEvent, list: TrackRow[], i: number): boolean {
    const ctrl = e.ctrlKey || e.metaKey;
    if (!ctrl && !e.shiftKey) {
      this.clearSelection();
      return false;
    }
    if (list !== this.selScope) {
      this.selScope = list;
      this.selIds = new Set();
      this.anchor = null;
    }
    const ids = new Set(e.shiftKey ? (ctrl ? this.selIds : []) : this.selIds);
    if (e.shiftKey && this.anchor != null) {
      const [a, b] = this.anchor < i ? [this.anchor, i] : [i, this.anchor];
      for (let k = a; k <= b; k++) ids.add(list[k].id);
    } else {
      const id = list[i].id;
      if (ids.has(id)) ids.delete(id);
      else ids.add(id);
      this.anchor = i;
    }
    this.selIds = ids;
    return true;
  }

  /** Open the song menu: for the whole selection when the song is part of it. */
  openTrackMenu(e: MouseEvent, list: TrackRow[], i: number) {
    e.preventDefault();
    e.stopPropagation();
    const t = list[i];
    if (list === this.selScope && this.selIds.has(t.id) && this.selIds.size > 1) {
      this.trackMenu = { x: e.clientX, y: e.clientY, tracks: this.selected };
    } else {
      this.trackMenu = { x: e.clientX, y: e.clientY, tracks: [t] };
    }
  }

  private version = -1;
  private palettes = new Map<string, Promise<Palette | null>>();

  /** Palette for a cover hash, cached (tile hover, album pages, genres). */
  palette(hash: string): Promise<Palette | null> {
    let p = this.palettes.get(hash);
    if (!p) {
      p = api.coverPalette(hash).catch(() => null);
      this.palettes.set(hash, p);
    }
    return p;
  }

  /** Drop cached lists after the library changed. */
  invalidate() {
    this.albums = null;
    this.artists = null;
    this.genres = null;
    this.songs = null;
    this.version = app.libraryVersion;
  }

  async ensure(view: "albums" | "artists" | "genres" | "songs") {
    if (this.version !== app.libraryVersion) this.invalidate();
    if (view === "albums" && !this.albums) this.albums = (await app.guard(api.albums())) ?? [];
    if (view === "artists" && !this.artists) this.artists = (await app.guard(api.artists())) ?? [];
    if (view === "genres" && !this.genres) this.genres = (await app.guard(api.genres())) ?? [];
    if (view === "songs" && !this.songs) this.songs = (await app.guard(api.tracks())) ?? [];
  }

  async openAlbum(key: string) {
    await this.ensure("albums");
    const row = this.albums?.find((a) => a.key === key);
    if (!row) return;
    const [tracks, palette] = await Promise.all([
      app.guard(api.tracks({ albumKey: key })),
      row.cover ? this.palette(row.cover.hash) : Promise.resolve(null),
    ]);
    this.album = { row, tracks: tracks ?? [], palette };
  }

  async refreshAlbum() {
    if (this.album) await this.openAlbum(this.album.row.key);
  }

  get missingAlbums(): AlbumRow[] {
    return (this.albums ?? []).filter((a) => !a.cover || a.cover.placeholder);
  }

  // ---- Missing covers -------------------------------------------------------

  /** Search one album; a failure is recorded on its tile, not thrown, so a
   * batch keeps going. */
  private async search(a: AlbumRow) {
    this.missing[a.key] = { st: "searching" };
    try {
      const c = await api.findCovers(a.artist, a.title, "");
      this.missing[a.key] = c.length ? { st: "found", cand: c[0] } : { st: "none" };
    } catch (e) {
      this.missing[a.key] = { st: "none", reason: String(e) };
    }
  }

  /** Look every missing album up online, one by one (spec: 380 ms stagger;
   * MusicBrainz's rate limit makes the real pace about 1 s). */
  async findAll() {
    if (this.finding) return;
    this.finding = true;
    try {
      for (const a of this.missingAlbums) {
        const m = this.missing[a.key];
        if (m && m.st !== "idle" && m.st !== "none") continue;
        await this.search(a);
        await new Promise((r) => setTimeout(r, 380));
      }
    } finally {
      this.finding = false;
    }
  }

  async accept(a: AlbumRow) {
    const m = this.missing[a.key];
    if (m?.st !== "found") return;
    this.missing[a.key] = { st: "assigning" };
    try {
      const local = await api.downloadCover(m.cand.image);
      await this.assign(a, local);
    } catch (e) {
      this.missing[a.key] = m;
      app.notify(String(e), true);
    }
  }

  async acceptAll() {
    for (const a of this.missingAlbums) if (this.missing[a.key]?.st === "found") await this.accept(a);
  }

  skip(a: AlbumRow) {
    this.missing[a.key] = { st: "idle" };
  }

  /** Give an album a cover from a local image file. */
  async assign(a: AlbumRow, image: string) {
    this.missing[a.key] = { st: "assigning" };
    try {
      const r = await api.albumSetCover(a.key, image, app.ui.writeCovers);
      this.missing[a.key] = { st: "assigned" };
      this.flash = a.key;
      setTimeout(() => this.flash === a.key && (this.flash = null), 1000);
      if (r.failures.length) app.notify(`Cover set, with problems: ${r.failures[0]}`, true);
      else if (r.written) app.notify(`Cover Set for ${a.title}`);
      else app.notify(`Cover Set for ${a.title} in Your Library`);
    } catch (e) {
      this.missing[a.key] = { st: "idle" };
      app.notify(String(e), true);
    }
  }
}

export const lib = new LibraryData();
