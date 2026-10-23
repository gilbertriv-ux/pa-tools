//! [`FrameCache`]: the decoded-frame cache, one per app and shared by every open [`crate::Movie`]
//! (Gil, V4 decision 6, 2026-10-23).
//!
//! Before this each movie kept its own byte-bounded LRU, so the Video track's eight open movies could
//! hold eight times the budget. Now one cache holds the frames of all of them, counted in bytes, and
//! when it is over its budget the least recently used frame goes, whichever movie it belongs to.
//!
//! ## Behaviour
//!
//! - **Budget.** [`DEFAULT_CACHE_BYTES`] (1 GiB) of RGBA picture data for the whole cache, counted as
//!   `rgba.len()` of every frame it holds. The count never exceeds the budget: a frame is inserted,
//!   then least recently used frames are dropped until the total is back inside it. A frame that is
//!   larger than the whole budget is not cached at all (the caller still gets it; only the cache
//!   skips it), so the bound is exact, not "plus the newest frame".
//! - **Eviction is plain global LRU.** A hit makes a frame the most recently used one. So a movie that
//!   is being played, decoding forward, pushes out the frames another movie is not touching, oldest
//!   first, and a movie that is idle loses its frames only when the cache is full and they are the
//!   oldest. There is no per-movie reserve. Two movies that take turns (a cut between two clips, or a
//!   play head crossing a video track with two clips on it) both stay cached as long as their
//!   combined working set fits the budget; when it does not, plain LRU thrashes, as any LRU does on
//!   a cyclic access pattern bigger than the cache. Neither movie is starved for good: each request
//!   that has to decode a frame inserts it as the most recent one, so a movie that is asked for again
//!   always gets its newest frames back.
//! - **Per-movie count ceiling.** Each movie also holds at most [`DEFAULT_CACHE_FRAMES`] frames
//!   ([`crate::Movie::set_cache_frames`] lowers it), and the whole cache at most
//!   [`DEFAULT_SHARED_CACHE_FRAMES`]. These are not the bound for any real picture (the byte budget
//!   is, from 480p up); they cap the bookkeeping for tiny ones.
//! - **Not covered.** The decoder's own working memory (each movie keeps its own budget, decisions 5,
//!   5a and 5b), the pictures a running decoder holds for requests that have not come yet, and
//!   timeline thumbnails (a small per-movie cache) are separate from this cache. Frames a caller still
//!   holds stay alive after the cache drops them.
//!
//! ## Locking
//!
//! One `Mutex` guards the whole state. It is held only for the bookkeeping of a lookup or an insert
//! (a few map operations), never while a frame is decoded, converted or dropped: evicted frames are
//! dropped after the lock is released. A poisoned lock is not an error here: the cache is only a cache,
//! so a thread that panicked while holding it leaves nothing that must be kept, and the next caller
//! empties it and carries on ([`FrameCache`] methods never panic and never return an error for this).
//!
//! ## Closing a movie
//!
//! A [`crate::Movie`] holds a lease on the cache; dropping the movie drops the lease, which removes
//! the movie's frames and gives their bytes back at once.

use crate::movie::{DEFAULT_CACHE_BYTES, DEFAULT_CACHE_FRAMES, Frame};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// Most frames the shared cache holds in all, for every movie together. It is a ceiling for pictures
/// so small that the byte budget would let millions of them in (the bound there is the memory the
/// bookkeeping takes, a few hundred bytes a frame), not the bound for any real picture.
pub const DEFAULT_SHARED_CACHE_FRAMES: usize = 4096;

/// A cached frame and where it stands in the global recency order.
struct Entry {
    /// Position in the global LRU order (`State::by_use`); larger is more recent.
    tick: u64,
    frame: Arc<Frame>,
}

/// What the cache knows about one movie.
struct MovieSlot {
    /// Most frames this movie may hold.
    frame_cap: usize,
    /// Bytes of this movie's frames.
    bytes: usize,
    /// Presentation index -> cached frame.
    frames: HashMap<usize, Entry>,
}

struct State {
    byte_cap: usize,
    frame_cap: usize,
    /// Bytes of every frame held.
    bytes: usize,
    /// Last recency number handed out.
    tick: u64,
    /// Last movie number handed out.
    next_id: u64,
    /// Recency order of every frame: tick -> (movie, index). The first entry is the least recently used.
    by_use: BTreeMap<u64, (u64, usize)>,
    movies: HashMap<u64, MovieSlot>,
}

impl State {
    fn new(byte_cap: usize, frame_cap: usize) -> State {
        State {
            byte_cap: byte_cap.max(1),
            frame_cap: frame_cap.max(1),
            bytes: 0,
            tick: 0,
            next_id: 0,
            by_use: BTreeMap::new(),
            movies: HashMap::new(),
        }
    }

    /// The next recency number.
    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    /// Drop one frame (it must exist), keeping every count right. The frame goes into `out` so that
    /// the caller drops it after the lock is released.
    fn remove(&mut self, movie: u64, index: usize, out: &mut Vec<Arc<Frame>>) {
        let Some(slot) = self.movies.get_mut(&movie) else { return };
        let Some(entry) = slot.frames.remove(&index) else { return };
        let len = entry.frame.rgba.len();
        slot.bytes = slot.bytes.saturating_sub(len);
        self.bytes = self.bytes.saturating_sub(len);
        self.by_use.remove(&entry.tick);
        out.push(entry.frame);
    }

    /// Drop the least recently used frame of any movie. False when the cache is empty.
    fn evict_oldest(&mut self, out: &mut Vec<Arc<Frame>>) -> bool {
        let Some((&tick, &(movie, index))) = self.by_use.iter().next() else { return false };
        let before = self.by_use.len();
        self.remove(movie, index, out);
        if self.by_use.len() == before {
            // The order and the movie tables disagree (they cannot, short of a bug): forget the stale
            // order entry so that the loop above always makes progress.
            self.by_use.remove(&tick);
        }
        true
    }

    /// Drop the least recently used frame of one movie. False when it holds none.
    fn evict_oldest_of(&mut self, movie: u64, out: &mut Vec<Arc<Frame>>) -> bool {
        let Some(slot) = self.movies.get(&movie) else { return false };
        let Some((&index, _)) = slot.frames.iter().min_by_key(|(_, e)| e.tick) else { return false };
        self.remove(movie, index, out);
        true
    }

    /// Bring the cache back inside the byte budget and the frame ceilings, oldest first.
    fn trim(&mut self, movie: Option<u64>, out: &mut Vec<Arc<Frame>>) {
        if let Some(m) = movie {
            while self.movies.get(&m).is_some_and(|s| s.frames.len() > s.frame_cap) {
                if !self.evict_oldest_of(m, out) {
                    break;
                }
            }
        }
        while self.bytes > self.byte_cap || self.by_use.len() > self.frame_cap {
            if !self.evict_oldest(out) {
                break;
            }
        }
    }

    /// Forget everything held (the frames go into `out`), keeping the movies and their settings.
    fn clear_frames(&mut self, out: &mut Vec<Arc<Frame>>) {
        for slot in self.movies.values_mut() {
            out.extend(slot.frames.drain().map(|(_, e)| e.frame));
            slot.bytes = 0;
        }
        self.by_use.clear();
        self.bytes = 0;
    }
}

/// A handle on a frame cache. Cloning it gives another handle on the same cache, so one cache can be
/// given to every movie (see the module docs). `Send + Sync`.
#[derive(Clone)]
pub struct FrameCache {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for FrameCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("FrameCache").field("bytes", &s.bytes).field("byte_cap", &s.byte_cap).field("frames", &s.by_use.len()).finish_non_exhaustive()
    }
}

/// The cache an app shares among its movies: [`DEFAULT_CACHE_BYTES`] and [`DEFAULT_SHARED_CACHE_FRAMES`].
impl Default for FrameCache {
    fn default() -> Self {
        FrameCache::new(DEFAULT_CACHE_BYTES, DEFAULT_SHARED_CACHE_FRAMES)
    }
}

impl FrameCache {
    /// A cache holding at most `byte_budget` bytes of picture data and `max_frames` frames. Both
    /// are at least 1.
    pub fn new(byte_budget: usize, max_frames: usize) -> FrameCache {
        FrameCache { state: Arc::new(Mutex::new(State::new(byte_budget, max_frames))) }
    }

    /// The private cache a movie gets when it is opened without one: the old per-movie size,
    /// [`DEFAULT_CACHE_BYTES`] and [`DEFAULT_CACHE_FRAMES`].
    pub(crate) fn private() -> FrameCache {
        FrameCache::new(DEFAULT_CACHE_BYTES, DEFAULT_CACHE_FRAMES)
    }

    /// The state, even if a thread panicked while it held the lock: the cache is emptied (its accounting
    /// may be half-updated, and what it held can be decoded again) and the poison is cleared.
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                let mut dropped = Vec::new();
                // Rebuild from the movie tables, which are the owners of the frames, so the byte counts
                // cannot be wrong even if the panic came halfway through an update.
                guard.by_use.clear();
                guard.bytes = 0;
                for slot in guard.movies.values_mut() {
                    dropped.extend(slot.frames.drain().map(|(_, e)| e.frame));
                    slot.bytes = 0;
                }
                self.state.clear_poison();
                drop(dropped);
                guard
            }
        }
    }

    /// Bytes of picture data the whole cache holds now (all movies).
    pub fn cached_bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Frames the whole cache holds now (all movies).
    pub fn cached_frames(&self) -> usize {
        self.lock().by_use.len()
    }

    /// The most bytes of picture data the cache keeps.
    pub fn budget_bytes(&self) -> usize {
        self.lock().byte_cap
    }

    /// Change the byte budget, for every movie sharing the cache; frames are dropped, oldest first,
    /// until the cache is inside it. At least 1.
    pub fn set_budget_bytes(&self, bytes: usize) {
        let mut dropped = Vec::new();
        {
            let mut s = self.lock();
            s.byte_cap = bytes.max(1);
            s.trim(None, &mut dropped);
        }
        drop(dropped);
    }

    /// Change the ceiling on the number of frames the whole cache holds. At least 1.
    pub fn set_max_frames(&self, frames: usize) {
        let mut dropped = Vec::new();
        {
            let mut s = self.lock();
            s.frame_cap = frames.max(1);
            s.trim(None, &mut dropped);
        }
        drop(dropped);
    }

    /// Drop every frame (all movies), keeping the movies' settings.
    pub fn clear(&self) {
        let mut dropped = Vec::new();
        self.lock().clear_frames(&mut dropped);
        drop(dropped);
    }

    /// Register a movie. Dropping the lease removes the movie's frames.
    pub(crate) fn lease(&self) -> Lease {
        let mut s = self.lock();
        s.next_id = s.next_id.wrapping_add(1);
        let id = s.next_id;
        s.movies.insert(id, MovieSlot { frame_cap: DEFAULT_CACHE_FRAMES, bytes: 0, frames: HashMap::new() });
        Lease { cache: self.clone(), id }
    }

    /// Make the cache's mutex poisoned, as a panic in a thread holding it would (tests only).
    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let state = Arc::clone(&self.state);
        let _ = std::thread::spawn(move || {
            let _held = state.lock();
            std::panic::resume_unwind(Box::new("poisoning the frame cache on purpose"));
        })
        .join();
    }
}

/// One movie's place in a [`FrameCache`]. The frames it adds are keyed by presentation index.
pub(crate) struct Lease {
    cache: FrameCache,
    id: u64,
}

impl Lease {
    /// The cache frames are stored in.
    pub fn cache(&self) -> &FrameCache {
        &self.cache
    }

    /// The frame at `index`, if cached; it becomes the most recently used one.
    pub fn get(&self, index: usize) -> Option<Arc<Frame>> {
        let mut s = self.cache.lock();
        let tick = s.next_tick();
        let slot = s.movies.get_mut(&self.id)?;
        let entry = slot.frames.get_mut(&index)?;
        let old = std::mem::replace(&mut entry.tick, tick);
        let frame = Arc::clone(&entry.frame);
        s.by_use.remove(&old);
        s.by_use.insert(tick, (self.id, index));
        Some(frame)
    }

    /// True if the frame at `index` is cached (it does not count as a use).
    pub fn contains(&self, index: usize) -> bool {
        self.cache.lock().movies.get(&self.id).is_some_and(|s| s.frames.contains_key(&index))
    }

    /// Cache `frame` as the most recently used one, then drop the oldest frames of any movie until the
    /// cache is inside its budget. A frame larger than the whole byte budget is not cached.
    pub fn put(&self, index: usize, frame: Arc<Frame>) {
        let mut dropped = Vec::new();
        {
            let mut s = self.cache.lock();
            // A frame this key held before is replaced, whatever happens to the new one.
            s.remove(self.id, index, &mut dropped);
            let len = frame.rgba.len();
            if len <= s.byte_cap && s.movies.contains_key(&self.id) {
                let tick = s.next_tick();
                s.by_use.insert(tick, (self.id, index));
                s.bytes = s.bytes.saturating_add(len);
                if let Some(slot) = s.movies.get_mut(&self.id) {
                    slot.bytes = slot.bytes.saturating_add(len);
                    slot.frames.insert(index, Entry { tick, frame });
                }
                s.trim(Some(self.id), &mut dropped);
            }
        }
        // Frames are freed here, outside the lock.
        drop(dropped);
    }

    /// Bytes of this movie's frames in the cache.
    pub fn bytes(&self) -> usize {
        self.cache.lock().movies.get(&self.id).map_or(0, |s| s.bytes)
    }

    /// Change how many frames this movie may hold (the oldest of its own go first).
    pub fn set_frame_cap(&self, frames: usize) {
        let mut dropped = Vec::new();
        {
            let mut s = self.cache.lock();
            if let Some(slot) = s.movies.get_mut(&self.id) {
                slot.frame_cap = frames.max(1);
            }
            s.trim(Some(self.id), &mut dropped);
        }
        drop(dropped);
    }
}

impl Drop for Lease {
    /// Closing a movie gives its frames' bytes back.
    fn drop(&mut self) {
        let mut dropped = Vec::new();
        {
            let mut s = self.cache.lock();
            if let Some(slot) = s.movies.remove(&self.id) {
                for (_, entry) in slot.frames {
                    s.bytes = s.bytes.saturating_sub(entry.frame.rgba.len());
                    s.by_use.remove(&entry.tick);
                    dropped.push(entry.frame);
                }
            }
        }
        drop(dropped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: usize, index: usize) -> Arc<Frame> {
        Arc::new(Frame { width: 1, height: 1, rgba: vec![0; bytes], index, time: 0.0 })
    }

    #[test]
    fn a_hit_makes_a_frame_the_newest_and_eviction_is_oldest_first_across_movies() {
        let cache = FrameCache::new(1000, 100);
        let (a, b) = (cache.lease(), cache.lease());
        // interleaved: a0 b0 a1 b1 (each 250 bytes) fill the budget
        a.put(0, frame(250, 0));
        b.put(0, frame(250, 0));
        a.put(1, frame(250, 1));
        b.put(1, frame(250, 1));
        assert_eq!(cache.cached_bytes(), 1000);
        // a hit on a0 makes it the newest; the oldest is now b0
        assert!(a.get(0).is_some());
        a.put(2, frame(250, 2));
        assert!(!b.contains(0), "b's oldest frame went, although a made the request");
        assert!(a.contains(0) && a.contains(1) && a.contains(2) && b.contains(1));
        assert_eq!(cache.cached_bytes(), 1000);
        // the next one takes the oldest again: a1 (a0 was used after it)
        b.put(2, frame(250, 2));
        assert!(!a.contains(1) && a.contains(0) && b.contains(1) && b.contains(2));
        assert_eq!((a.bytes(), b.bytes(), cache.cached_bytes()), (500, 500, 1000));
    }

    #[test]
    fn replacing_a_key_swaps_its_bytes_and_a_huge_frame_is_not_cached() {
        let cache = FrameCache::new(1000, 100);
        let a = cache.lease();
        a.put(0, frame(400, 0));
        a.put(0, frame(100, 0));
        assert_eq!(cache.cached_bytes(), 100);
        // bigger than the whole budget: not cached, and the old frame at that key is gone
        a.put(0, frame(1001, 0));
        assert_eq!((cache.cached_bytes(), cache.cached_frames()), (0, 0));
        assert!(!a.contains(0) && a.get(0).is_none());
        // exactly the budget is cached
        a.put(1, frame(1000, 1));
        assert_eq!(cache.cached_bytes(), 1000);
        // and the huge one does not evict what is there
        a.put(2, frame(5000, 2));
        assert_eq!(cache.cached_bytes(), 1000);
        assert!(a.contains(1));
    }

    #[test]
    fn the_frame_ceilings_hold_for_the_movie_and_for_the_whole_cache() {
        let cache = FrameCache::new(1 << 30, 5);
        let (a, b) = (cache.lease(), cache.lease());
        for i in 0..4 {
            a.put(i, frame(10, i));
        }
        for i in 0..4 {
            b.put(i, frame(10, i));
        }
        assert_eq!(cache.cached_frames(), 5, "the whole cache holds at most 5");
        assert!(!a.contains(0) && !a.contains(1) && !a.contains(2) && a.contains(3), "a's oldest went first");
        assert_eq!(cache.cached_bytes(), 50);
        // a movie's own ceiling: only its own oldest frames go
        cache.set_max_frames(100);
        b.set_frame_cap(2);
        assert!(!b.contains(0) && !b.contains(1) && b.contains(2) && b.contains(3));
        assert!(a.contains(3), "the other movie keeps its frames");
        a.put(7, frame(10, 7));
        b.put(7, frame(10, 7));
        assert_eq!(b.bytes(), 20, "b still holds two frames");
        assert!(!b.contains(2) && b.contains(3) && b.contains(7));
    }

    #[test]
    fn dropping_a_lease_gives_its_bytes_back() {
        let cache = FrameCache::new(1 << 20, 100);
        let a = cache.lease();
        let b = cache.lease();
        a.put(0, frame(300, 0));
        b.put(0, frame(200, 0));
        b.put(1, frame(100, 1));
        assert_eq!(cache.cached_bytes(), 600);
        drop(b);
        assert_eq!((cache.cached_bytes(), cache.cached_frames()), (300, 1));
        assert!(a.contains(0));
        drop(a);
        assert_eq!((cache.cached_bytes(), cache.cached_frames()), (0, 0));
        assert!(cache.lock().movies.is_empty());
    }

    #[test]
    fn a_frame_a_caller_holds_outlives_its_eviction() {
        let cache = FrameCache::new(100, 100);
        let a = cache.lease();
        a.put(0, frame(60, 0));
        let held = a.get(0).unwrap();
        a.put(1, frame(60, 1));
        assert!(!a.contains(0));
        assert_eq!((held.rgba.len(), cache.cached_bytes()), (60, 60));
    }

    #[test]
    fn the_budget_can_be_lowered_and_the_cache_cleared() {
        let cache = FrameCache::new(1000, 100);
        let (a, b) = (cache.lease(), cache.lease());
        for i in 0..2 {
            a.put(i, frame(200, i));
            b.put(i, frame(200, i));
        }
        assert_eq!(cache.cached_bytes(), 800);
        cache.set_budget_bytes(450);
        assert_eq!(cache.budget_bytes(), 450);
        assert_eq!(cache.cached_bytes(), 400);
        assert!(a.contains(1) && b.contains(1) && !a.contains(0) && !b.contains(0));
        cache.set_budget_bytes(0);
        assert_eq!((cache.budget_bytes(), cache.cached_bytes()), (1, 0), "a budget of 0 is 1 byte: nothing real fits");
        cache.set_budget_bytes(1000);
        a.put(0, frame(10, 0));
        b.put(0, frame(10, 0));
        cache.clear();
        assert_eq!((cache.cached_bytes(), cache.cached_frames()), (0, 0));
        assert_eq!(a.bytes(), 0);
    }

    #[test]
    fn a_poisoned_lock_empties_the_cache_and_carries_on() {
        let cache = FrameCache::new(1000, 100);
        let a = cache.lease();
        a.put(0, frame(100, 0));
        assert_eq!(cache.cached_bytes(), 100);
        cache.poison_for_test();
        assert!(cache.state.is_poisoned());
        // every entry point recovers, and none panics
        assert_eq!(cache.cached_bytes(), 0, "what the cache held is gone, and the count agrees");
        assert!(!cache.state.is_poisoned(), "the poison is cleared");
        assert!(a.get(0).is_none() && !a.contains(0));
        a.put(0, frame(100, 0));
        assert!(a.contains(0));
        assert_eq!((a.bytes(), cache.cached_bytes()), (100, 100));
        cache.poison_for_test();
        let b = cache.lease();
        b.put(3, frame(10, 3));
        assert!(b.contains(3) && !a.contains(0));
        cache.poison_for_test();
        drop(a);
        cache.poison_for_test();
        drop(b);
        cache.poison_for_test();
        assert!(format!("{cache:?}").contains("FrameCache"));
        assert_eq!((cache.cached_bytes(), cache.cached_frames()), (0, 0));
    }

    #[test]
    fn the_handle_and_the_lease_can_move_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrameCache>();
        assert_send_sync::<Lease>();
    }

    #[test]
    fn the_defaults_are_a_gibibyte_and_the_old_ceilings() {
        let cache = FrameCache::default();
        assert_eq!((cache.budget_bytes(), DEFAULT_CACHE_BYTES), (1 << 30, 1 << 30));
        assert_eq!(cache.lock().frame_cap, DEFAULT_SHARED_CACHE_FRAMES);
        assert_eq!(FrameCache::private().lock().frame_cap, DEFAULT_CACHE_FRAMES);
        const { assert!(DEFAULT_SHARED_CACHE_FRAMES >= 8 * DEFAULT_CACHE_FRAMES) };
    }
}
