//! Playback engine. Runs on its own thread (the audio device handle is not
//! `Send`) and is driven by commands over a channel.
//!
//! Gapless playback: while a track plays, the next one is already appended to
//! the rodio `Player`, so the decoder hands over sample-exactly. The engine
//! notices the hand-over when the player's queue length drops.

mod meter;

use crossbeam_channel::{Receiver, Sender};
use meter::{Level, Meter};
use parking_lot::Mutex;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A decoded, metered track ready to append to the player.
type TrackSource = Meter<Decoder<BufReader<File>>>;

const TICK: Duration = Duration::from_millis(33);
const POSITION_EVERY: Duration = Duration::from_millis(250);
/// "Previous" restarts the current track instead when past this point.
const PREV_RESTART_AFTER: f64 = 3.0;
/// How long before the end of a track the next one is opened and queued.
const PRELOAD_AHEAD: f64 = 15.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Library id, when the track came from the library.
    pub id: Option<i64>,
    pub path: PathBuf,
    /// Known duration in seconds (from tags), used when the decoder can't tell.
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: PlayState,
    pub index: Option<usize>,
    pub track: Option<Track>,
    pub position: f64,
    pub duration: Option<f64>,
    pub volume: f32,
    pub repeat: Repeat,
    pub queue_len: usize,
    /// Changes whenever the queue's contents change, so the UI knows when to
    /// refetch it.
    pub queue_id: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    /// Full status; sent on any state change.
    Status(Status),
    /// Lightweight position update while playing.
    Position { position: f64, duration: Option<f64> },
    /// Bass level 0..1 for the glow pulse (only while the meter is enabled).
    Level { value: f32 },
    Error { path: Option<PathBuf>, message: String },
}

#[derive(Debug)]
pub enum Command {
    Load { queue: Vec<Track>, index: usize, autoplay: bool },
    /// Insert tracks at `at` (clamped to the queue length); optionally start
    /// playing the first inserted track.
    Insert { tracks: Vec<Track>, at: usize, play: bool },
    /// Remove these queue indices. Removing the current track moves on to the
    /// one after it (keeping play/pause), or stops at the end of the queue.
    Remove(Vec<usize>),
    /// Move the track at `from` so it ends up at index `to`.
    Move { from: usize, to: usize },
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    PlayIndex(usize),
    Seek(f64),
    SetVolume(f32),
    SetRepeat(Repeat),
    SetMeter(bool),
    Stop,
}

pub type Emitter = Arc<dyn Fn(Event) + Send + Sync>;

/// Cheap, cloneable handle used by the rest of the app.
#[derive(Clone)]
pub struct AudioHandle {
    tx: Sender<Command>,
    status: Arc<Mutex<Status>>,
    queue: Arc<Mutex<Vec<Track>>>,
}

impl AudioHandle {
    pub fn send(&self, cmd: Command) {
        // The engine thread lives as long as the app; a send can only fail
        // during shutdown, when there's nothing useful to do about it.
        let _ = self.tx.send(cmd);
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    pub fn queue(&self) -> Vec<Track> {
        self.queue.lock().clone()
    }
}

pub fn spawn(emit: Emitter) -> AudioHandle {
    let (tx, rx) = crossbeam_channel::unbounded();
    let status = Arc::new(Mutex::new(Status {
        state: PlayState::Stopped,
        index: None,
        track: None,
        position: 0.0,
        duration: None,
        volume: 0.8,
        repeat: Repeat::Off,
        queue_len: 0,
        queue_id: 0,
    }));
    let queue = Arc::new(Mutex::new(Vec::new()));
    let (shared, shared_queue) = (status.clone(), queue.clone());
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || Engine::new(emit, shared, shared_queue).run(rx))
        .expect("spawn audio thread");
    AudioHandle { tx, status, queue }
}

struct Engine {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    queue: Vec<Track>,
    index: Option<usize>,
    /// Queue index already appended behind the current track.
    preloaded: Option<usize>,
    /// Index whose preload failed, so we don't retry it every tick.
    preload_failed: Option<usize>,
    duration: Option<f64>,
    volume: f32,
    repeat: Repeat,
    level: Level,
    meter_on: bool,
    emit: Emitter,
    shared: Arc<Mutex<Status>>,
    shared_queue: Arc<Mutex<Vec<Track>>>,
    queue_id: u64,
    last_position: Instant,
}

impl Engine {
    fn new(emit: Emitter, shared: Arc<Mutex<Status>>, shared_queue: Arc<Mutex<Vec<Track>>>) -> Self {
        let volume = shared.lock().volume;
        Self {
            shared_queue,
            queue_id: 0,
            sink: None,
            player: None,
            queue: Vec::new(),
            index: None,
            preloaded: None,
            preload_failed: None,
            duration: None,
            volume,
            repeat: Repeat::Off,
            level: Level::default(),
            meter_on: false,
            emit,
            shared,
            last_position: Instant::now(),
        }
    }

    fn run(mut self, rx: Receiver<Command>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => {
                    self.handle(cmd);
                    self.publish();
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => self.tick(),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Load { queue, index, autoplay } => {
                self.queue = queue;
                self.queue_changed();
                if self.queue.is_empty() {
                    self.stop();
                    self.index = None;
                } else {
                    self.start(index.min(self.queue.len() - 1), autoplay);
                }
            }
            Command::Insert { tracks, at, play } => {
                if tracks.is_empty() {
                    return;
                }
                let at = at.min(self.queue.len());
                let n = tracks.len();
                self.queue.splice(at..at, tracks);
                let shift = |i: usize| if i >= at { i + n } else { i };
                self.index = self.index.map(shift);
                self.preloaded = self.preloaded.map(shift);
                self.preload_failed = self.preload_failed.map(shift);
                self.queue_changed();
                if play {
                    self.start(at, true);
                } else {
                    self.fix_preload();
                }
            }
            Command::Remove(indices) => self.remove(indices),
            Command::Move { from, to } => {
                let len = self.queue.len();
                if from >= len || to >= len || from == to {
                    return;
                }
                let t = self.queue.remove(from);
                self.queue.insert(to, t);
                let map = |i: usize| moved_index(i, from, to);
                self.index = self.index.map(map);
                self.preloaded = self.preloaded.map(map);
                self.preload_failed = self.preload_failed.map(map);
                self.queue_changed();
                self.fix_preload();
            }
            Command::Play => self.play(),
            Command::Pause => {
                if let Some(p) = &self.player {
                    p.pause();
                }
            }
            Command::Toggle => match self.state() {
                PlayState::Playing => {
                    if let Some(p) = &self.player {
                        p.pause();
                    }
                }
                _ => self.play(),
            },
            Command::Next => {
                if let Some(next) = self.next_index(true) {
                    if self.preloaded == Some(next) {
                        // Let the gapless hand-over machinery do the work.
                        if let Some(p) = &self.player {
                            p.skip_one();
                        }
                    } else {
                        self.start(next, self.state() != PlayState::Paused);
                    }
                }
            }
            Command::Prev => {
                let pos = self.position();
                match self.index {
                    Some(i) if pos < PREV_RESTART_AFTER && i > 0 => self.start(i - 1, self.state() != PlayState::Paused),
                    Some(_) => self.seek(0.0),
                    None => {}
                }
            }
            Command::PlayIndex(i) if i < self.queue.len() => self.start(i, true),
            Command::PlayIndex(_) => {}
            Command::Seek(secs) => self.seek(secs),
            Command::SetVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
                if let Some(p) = &self.player {
                    p.set_volume(perceptual(self.volume));
                }
            }
            Command::SetRepeat(r) => {
                self.repeat = r;
                self.fix_preload();
            }
            Command::SetMeter(on) => {
                self.meter_on = on;
                if !on {
                    self.level.reset();
                }
            }
            Command::Stop => self.stop(),
        }
    }

    fn tick(&mut self) {
        let Some(player) = &self.player else { return };
        let len = player.len();

        if self.preloaded.is_some() && len == 1 {
            // Gapless hand-over happened.
            self.index = self.preloaded.take();
            self.duration = self.index.and_then(|i| self.queue[i].duration);
            self.publish();
        } else if len == 0 && self.index.is_some() {
            // Reached the end of the queue.
            self.player = None;
            self.level.reset();
            self.publish();
            return;
        }

        self.maybe_preload();
        if self.meter_on && self.state() == PlayState::Playing {
            (self.emit)(Event::Level { value: self.level.get() });
        }
        if self.last_position.elapsed() >= POSITION_EVERY && self.state() == PlayState::Playing {
            self.last_position = Instant::now();
            let position = self.position();
            self.shared.lock().position = position;
            (self.emit)(Event::Position { position, duration: self.duration });
        }
    }

    fn sink(&mut self) -> Option<&MixerDeviceSink> {
        if self.sink.is_none() {
            match DeviceSinkBuilder::open_default_sink() {
                Ok(mut s) => {
                    s.log_on_drop(false);
                    self.sink = Some(s);
                }
                Err(e) => self.error(None, format!("No audio output device: {e}")),
            }
        }
        self.sink.as_ref()
    }

    /// Start playing queue index `i` on a fresh player, skipping over files
    /// that fail to open.
    fn start(&mut self, mut i: usize, autoplay: bool) {
        self.player = None;
        self.preloaded = None;
        self.preload_failed = None;
        self.level.reset();
        let Some(sink) = self.sink() else { return };
        let player = Player::connect_new(sink.mixer());
        player.set_volume(perceptual(self.volume));
        if !autoplay {
            player.pause();
        }

        for _ in 0..self.queue.len() {
            match self.open(i) {
                Ok((src, dur)) => {
                    player.append(src);
                    self.index = Some(i);
                    self.duration = dur;
                    self.player = Some(player);
                    self.maybe_preload();
                    return;
                }
                Err(msg) => {
                    let path = self.queue[i].path.clone();
                    self.error(Some(path), msg);
                    i = (i + 1) % self.queue.len();
                }
            }
        }
        self.index = None;
    }

    fn remove(&mut self, mut indices: Vec<usize>) {
        indices.sort_unstable();
        indices.dedup();
        indices.retain(|&i| i < self.queue.len());
        if indices.is_empty() {
            return;
        }
        let playing = self.state() == PlayState::Playing;
        let current_gone = self.index.is_some_and(|i| indices.binary_search(&i).is_ok());
        // The preloaded track is already inside the audio pipeline; if it goes,
        // the pipeline must be rebuilt or it would still play.
        let preload_gone = self.preloaded.is_some_and(|i| indices.binary_search(&i).is_ok());
        let old_index = self.index;

        let mut i = 0;
        self.queue.retain(|_| {
            let keep = indices.binary_search(&i).is_err();
            i += 1;
            keep
        });
        let map = |i: usize| index_after_remove(i, &indices);
        self.preloaded = self.preloaded.and_then(map);
        self.preload_failed = self.preload_failed.and_then(map);
        self.queue_changed();

        if self.queue.is_empty() {
            self.stop();
            self.index = None;
            self.duration = None;
            return;
        }
        if current_gone {
            // Whatever followed the removed track now sits at its old place.
            let cur = old_index.unwrap_or(0);
            let next = cur - indices.iter().filter(|&&r| r < cur).count();
            if next < self.queue.len() {
                self.start(next, playing);
            } else if self.repeat == Repeat::All {
                self.start(0, playing);
            } else {
                self.stop();
                self.index = Some(self.queue.len() - 1);
                self.duration = self.queue[self.queue.len() - 1].duration;
            }
            return;
        }
        self.index = self.index.and_then(map);
        if preload_gone {
            self.rebuild();
        } else {
            self.fix_preload();
        }
    }

    /// Rebuild the player at the current track and position (drops anything
    /// preloaded behind it).
    fn rebuild(&mut self) {
        let (pos, paused) = (self.position(), self.state() == PlayState::Paused);
        if let Some(i) = self.index {
            self.start(i, !paused);
            self.seek(pos);
        }
    }

    fn queue_changed(&mut self) {
        self.queue_id += 1;
        *self.shared_queue.lock() = self.queue.clone();
    }

    /// Preload the next track only near the end of the current one, so queue
    /// edits before then never need to touch the audio pipeline.
    fn maybe_preload(&mut self) {
        if self.preloaded.is_some() || self.player.is_none() {
            return;
        }
        if let Some(d) = self.duration {
            if d - self.position() > PRELOAD_AHEAD {
                return;
            }
        }
        self.preload();
    }

    /// If the track already queued behind the current one is no longer the
    /// right next track, rebuild the player at the same position.
    fn fix_preload(&mut self) {
        if self.preloaded.is_none() || self.preloaded == self.next_index(false) {
            return;
        }
        self.rebuild();
    }

    fn preload(&mut self) {
        if self.preloaded.is_some() {
            return;
        }
        let Some(next) = self.next_index(false) else { return };
        if self.preload_failed == Some(next) {
            return;
        }
        match self.open(next) {
            Ok((src, _)) => {
                if let Some(p) = &self.player {
                    p.append(src);
                    self.preloaded = Some(next);
                }
            }
            Err(msg) => {
                self.preload_failed = Some(next);
                let path = self.queue[next].path.clone();
                self.error(Some(path), msg);
            }
        }
    }

    fn open(&self, i: usize) -> Result<(TrackSource, Option<f64>), String> {
        let track = &self.queue[i];
        let file = open_shared(&track.path).map_err(|e| format!("Can't open file: {e}"))?;
        let len = file.metadata().map(|m| m.len()).ok();
        let mut builder = Decoder::builder().with_data(BufReader::new(file)).with_seekable(true).with_gapless(true);
        if let Some(len) = len {
            builder = builder.with_byte_len(len);
        }
        if let Some(ext) = track.path.extension().and_then(|e| e.to_str()) {
            builder = builder.with_hint(ext);
        }
        let decoder = builder.build().map_err(|e| format!("Can't decode: {e}"))?;
        let duration = rodio::Source::total_duration(&decoder).map(|d| d.as_secs_f64()).or(track.duration);
        Ok((Meter::new(decoder, self.level.clone()), duration))
    }

    /// Next index after the current one. `manual` = user pressed Next, which
    /// moves on even in repeat-one mode.
    fn next_index(&self, manual: bool) -> Option<usize> {
        next_index(self.index?, self.queue.len(), self.repeat, manual)
    }

    fn play(&mut self) {
        match &self.player {
            Some(p) => p.play(),
            // Stopped at the end of the queue: start over from the current/first track.
            None if !self.queue.is_empty() => self.start(self.index.unwrap_or(0), true),
            None => {}
        }
    }

    fn seek(&mut self, secs: f64) {
        let Some(p) = &self.player else { return };
        let secs = match self.duration {
            Some(d) => secs.clamp(0.0, (d - 0.25).max(0.0)),
            None => secs.max(0.0),
        };
        if let Err(e) = p.try_seek(Duration::from_secs_f64(secs)) {
            self.error(self.index.map(|i| self.queue[i].path.clone()), format!("Seek failed: {e}"));
        }
        self.shared.lock().position = secs;
        (self.emit)(Event::Position { position: secs, duration: self.duration });
    }

    fn stop(&mut self) {
        self.player = None;
        self.preloaded = None;
        self.level.reset();
    }

    fn state(&self) -> PlayState {
        match &self.player {
            Some(p) if p.len() > 0 && p.is_paused() => PlayState::Paused,
            Some(p) if p.len() > 0 => PlayState::Playing,
            _ => PlayState::Stopped,
        }
    }

    fn position(&self) -> f64 {
        self.player.as_ref().map(|p| p.get_pos().as_secs_f64()).unwrap_or(0.0)
    }

    fn error(&self, path: Option<PathBuf>, message: String) {
        log::warn!("audio: {message} ({path:?})");
        (self.emit)(Event::Error { path, message });
    }

    fn publish(&self) {
        let status = Status {
            state: self.state(),
            index: self.index,
            track: self.index.map(|i| self.queue[i].clone()),
            position: self.position(),
            duration: self.duration,
            volume: self.volume,
            repeat: self.repeat,
            queue_len: self.queue.len(),
            queue_id: self.queue_id,
        };
        *self.shared.lock() = status.clone();
        (self.emit)(Event::Status(status));
    }
}

/// Where queue index `i` ends up after the indices in `removed` (sorted,
/// unique) are taken out; None if `i` itself was removed.
fn index_after_remove(i: usize, removed: &[usize]) -> Option<usize> {
    match removed.binary_search(&i) {
        Ok(_) => None,
        Err(before) => Some(i - before),
    }
}

/// Where queue index `i` ends up after the track at `from` moves to `to`.
fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

fn next_index(i: usize, len: usize, repeat: Repeat, manual: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match repeat {
        Repeat::One if !manual => Some(i),
        Repeat::All | Repeat::One => Some((i + 1) % len),
        Repeat::Off => (i + 1 < len).then_some(i + 1),
    }
}

/// Open for reading while letting other code rename or replace the file, so
/// the tag editor can save the track that is currently playing. The decoder
/// keeps reading the original data until the next track.
pub fn open_shared(path: &std::path::Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    opts.open(path)
}

/// Map a linear 0..1 slider to gain so the slider feels even across its range.
fn perceptual(v: f32) -> f32 {
    v * v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_index_rules() {
        assert_eq!(next_index(0, 3, Repeat::Off, false), Some(1));
        assert_eq!(next_index(2, 3, Repeat::Off, false), None);
        assert_eq!(next_index(2, 3, Repeat::All, false), Some(0));
        assert_eq!(next_index(1, 3, Repeat::One, false), Some(1), "repeat-one replays automatically");
        assert_eq!(next_index(2, 3, Repeat::One, true), Some(0), "Next button moves on and wraps");
        assert_eq!(next_index(0, 1, Repeat::All, false), Some(0));
        assert_eq!(next_index(0, 0, Repeat::All, false), None);
    }

    #[test]
    fn remove_remaps_indices() {
        let removed = [1, 3];
        assert_eq!(index_after_remove(0, &removed), Some(0));
        assert_eq!(index_after_remove(1, &removed), None);
        assert_eq!(index_after_remove(2, &removed), Some(1));
        assert_eq!(index_after_remove(3, &removed), None);
        assert_eq!(index_after_remove(4, &removed), Some(2));
    }

    #[test]
    fn move_remaps_indices() {
        // [a b c d e], move b (1) to 3 -> [a c d b e]
        let order: Vec<usize> = (0..5).map(|i| moved_index(i, 1, 3)).collect();
        assert_eq!(order, vec![0, 3, 1, 2, 4]);
        // Move d (3) to 0 -> [d a b c e]
        let order: Vec<usize> = (0..5).map(|i| moved_index(i, 3, 0)).collect();
        assert_eq!(order, vec![1, 2, 3, 0, 4]);
        // Every move is a permutation.
        for from in 0..5 {
            for to in 0..5 {
                let mut seen: Vec<usize> = (0..5).map(|i| moved_index(i, from, to)).collect();
                seen.sort_unstable();
                assert_eq!(seen, vec![0, 1, 2, 3, 4], "from {from} to {to}");
            }
        }
    }

    #[test]
    fn volume_curve() {
        assert_eq!(perceptual(0.0), 0.0);
        assert_eq!(perceptual(1.0), 1.0);
        assert!(perceptual(0.5) < 0.5);
    }
}
