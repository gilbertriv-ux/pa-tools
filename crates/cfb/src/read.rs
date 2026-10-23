//! Reading compound files ([MS-CFB] §2.2–2.6).
//!
//! The reader treats the file as hostile. Every sector number, chain length, stream size and
//! directory index comes from the file, so none is trusted:
//!
//! - bytes are read with total accessors (`le16`/`le32`/`le64`) and `get()`; nothing indexes a
//!   slice with a number from the file (the crate denies `clippy::indexing_slicing`);
//! - the structural chains (FAT sectors, DIFAT chain, directory, mini FAT, mini stream) are
//!   claimed in one bitmap, so a chain that loops, or two chains that share a sector, is an
//!   error at [`CompoundFile::open`];
//! - stream chains are walked with Brent's cycle detection, so a looping FAT fails after a few
//!   steps instead of after reading as many sectors as the file has;
//! - allocations follow [`Limits`]: entries, chain length and the total bytes of stream data
//!   returned all scale with the file size, and each has an absolute ceiling.

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;

use crate::{ENDOFCHAIN, Error, FREESECT, MAXREGSECT, MINI_SECTOR_SIZE, NOSTREAM, Result, SIGNATURE, fold_name};

/// Absolute ceiling on directory entries (a 4 Mi-entry directory is 512 MiB of file).
pub const MAX_ENTRIES: usize = 4 << 20;
/// Absolute ceiling on the stream bytes one [`CompoundFile`] hands out (4 GiB).
pub const MAX_READ_BYTES: u64 = 4 << 30;

/// Resource limits for opening and reading a compound file.
///
/// The defaults ([`Limits::for_len`]) follow the size of the file: a directory cannot hold more
/// entries than the file has room for, and the streams of an intact file add up to at most the
/// file's length, so reading four times that (to allow a few re-reads) is generous. A file whose
/// streams overlap, or claim sizes it does not have, runs into the budget instead of making the
/// reader copy the same sectors over and over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most directory entries.
    pub max_entries: usize,
    /// Most stream bytes [`CompoundFile::read`] may return over the life of the value.
    pub max_read_bytes: u64,
}

impl Limits {
    /// Limits for a file of `len` bytes: entries up to `len / 128 + 1`, reads up to `4 * len + 1 MiB`,
    /// both capped by the absolute ceilings [`MAX_ENTRIES`] and [`MAX_READ_BYTES`].
    pub fn for_len(len: usize) -> Limits {
        Limits {
            max_entries: (len / 128).saturating_add(1).min(MAX_ENTRIES),
            max_read_bytes: (len as u64).saturating_mul(4).saturating_add(1 << 20).min(MAX_READ_BYTES),
        }
    }
}

/// Directory entry object type (§2.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Root,
    Storage,
    Stream,
    /// Unallocated (type 0) or an unknown type.
    Unknown,
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Class id of a storage (AAF: the class of the object stored in it).
    pub clsid: [u8; 16],
    pub state_bits: u32,
    /// FILETIME stamps (0 when not set).
    pub created: u64,
    pub modified: u64,
    /// First sector (regular or mini) of a stream; the mini stream's for the root.
    pub start: u32,
    pub size: u64,
    /// Child entries of a storage, in sibling-tree order (sorted by [`compare_names`]).
    pub children: Vec<usize>,
    left: u32,
    right: u32,
    child: u32,
}

/// What [`CompoundFile::root`] returns for a value that has no entries (never the case after a
/// successful [`CompoundFile::open`]; this keeps the accessor total).
static NO_ROOT: Entry = Entry {
    name: String::new(),
    kind: EntryKind::Root,
    clsid: [0; 16],
    state_bits: 0,
    created: 0,
    modified: 0,
    start: ENDOFCHAIN,
    size: 0,
    children: Vec::new(),
    left: NOSTREAM,
    right: NOSTREAM,
    child: NOSTREAM,
};

/// A parsed compound file over borrowed bytes.
#[derive(Clone, Debug)]
pub struct CompoundFile<'a> {
    data: &'a [u8],
    /// Major version: 3 (512-byte sectors) or 4 (4096-byte sectors).
    pub version: u16,
    /// Header CLSID (normally zero).
    pub header_clsid: [u8; 16],
    sector_size: usize,
    mini_cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini_stream: Vec<u8>,
    pub entries: Vec<Entry>,
    limits: Limits,
    /// Stream bytes handed out so far (charged against `limits.max_read_bytes`).
    read_total: Cell<u64>,
    /// (storage, folded name) → entry, built on the first lookup: a storage with many children
    /// would otherwise cost a scan of all of them per lookup.
    names: OnceCell<HashMap<(usize, Vec<u16>), usize>>,
}

/// Little-endian fields. They return 0 when the field lies outside `b`: callers pass the fixed
/// 512-byte header or a 128-byte directory entry, so a short buffer cannot occur, and a zero is
/// never worse than the garbage it replaces.
fn le16(b: &[u8], at: usize) -> u16 {
    at.checked_add(2).and_then(|end| b.get(at..end)).and_then(|s| s.try_into().ok()).map_or(0, u16::from_le_bytes)
}
fn le32(b: &[u8], at: usize) -> u32 {
    at.checked_add(4).and_then(|end| b.get(at..end)).and_then(|s| s.try_into().ok()).map_or(0, u32::from_le_bytes)
}
fn le64(b: &[u8], at: usize) -> u64 {
    at.checked_add(8).and_then(|end| b.get(at..end)).and_then(|s| s.try_into().ok()).map_or(0, u64::from_le_bytes)
}
fn guid(b: &[u8], at: usize) -> [u8; 16] {
    at.checked_add(16).and_then(|end| b.get(at..end)).and_then(|s| s.try_into().ok()).unwrap_or([0; 16])
}

fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}

/// A set of sectors, to see that no two chains use the same one.
struct Claims {
    bits: Vec<u64>,
    sectors: usize,
}

impl Claims {
    fn new(sectors: usize) -> Claims {
        Claims { bits: vec![0; sectors.div_ceil(64)], sectors }
    }

    /// Mark sector `s` used; false when it already was (or does not exist).
    fn claim(&mut self, s: u32) -> bool {
        let s = s as usize;
        if s >= self.sectors {
            return false;
        }
        match self.bits.get_mut(s / 64) {
            Some(word) => {
                let bit = 1u64 << (s % 64);
                let fresh = *word & bit == 0;
                *word |= bit;
                fresh
            }
            None => false,
        }
    }
}

/// A walk along a sector chain of `table` (the FAT or the mini FAT). Ends at [`ENDOFCHAIN`];
/// fails on a number outside the table, a free entry, a chain longer than `max_len`, and, by
/// Brent's cycle detection, on a chain that loops, however long the loop is.
struct Chain<'t> {
    table: &'t [u32],
    cur: u32,
    steps: usize,
    max_len: usize,
    tortoise: u32,
    power: usize,
    lam: usize,
}

impl<'t> Chain<'t> {
    fn new(table: &'t [u32], start: u32, max_len: usize) -> Chain<'t> {
        Chain { table, cur: start, steps: 0, max_len, tortoise: FREESECT, power: 1, lam: 0 }
    }
}

impl Iterator for Chain<'_> {
    type Item = Result<u32>;

    fn next(&mut self) -> Option<Result<u32>> {
        let s = self.cur;
        if s == ENDOFCHAIN {
            return None;
        }
        self.cur = ENDOFCHAIN; // after an error the walk is over
        if s > MAXREGSECT {
            return Some(Err(invalid(format!("bad sector {s:#x} in a chain"))));
        }
        if s == self.tortoise {
            return Some(Err(invalid(format!("sector chain loops at sector {s}"))));
        }
        if self.steps >= self.max_len {
            return Some(Err(invalid(format!("sector chain is longer than {} sectors", self.max_len))));
        }
        self.steps += 1;
        self.lam += 1;
        if self.lam == self.power {
            self.tortoise = s;
            self.power = self.power.saturating_mul(2);
            self.lam = 0;
        }
        match self.table.get(s as usize) {
            None => Some(Err(invalid(format!("sector {s} has no FAT entry")))),
            Some(&FREESECT) => Some(Err(invalid("chain runs into a free sector"))),
            Some(&next) => {
                self.cur = next;
                Some(Ok(s))
            }
        }
    }
}

impl<'a> CompoundFile<'a> {
    /// Parse the header, FAT, mini FAT, directory and mini stream of `data` with the default
    /// [`Limits`] for its size.
    pub fn open(data: &'a [u8]) -> Result<CompoundFile<'a>> {
        Self::open_with(data, Limits::for_len(data.len()))
    }

    /// [`CompoundFile::open`] with explicit limits.
    pub fn open_with(data: &'a [u8], limits: Limits) -> Result<CompoundFile<'a>> {
        let Some(header) = data.get(..512) else { return Err(Error::NotCfb) };
        if header.get(..8) != Some(&SIGNATURE[..]) || le16(header, 28) != 0xFFFE {
            return Err(Error::NotCfb);
        }
        let version = le16(header, 26);
        let shift = le16(header, 30);
        if !(7..=16).contains(&shift) {
            return Err(invalid(format!("sector shift {shift}")));
        }
        let mini_shift = le16(header, 32);
        if 1usize.checked_shl(u32::from(mini_shift)) != Some(MINI_SECTOR_SIZE) {
            return Err(invalid(format!("mini sector shift {mini_shift}")));
        }
        let sector_size = 1usize << shift;
        // sectors that lie wholly in the file (sector 0 starts after the first `sector_size` bytes)
        let n_sectors = (data.len() / sector_size).saturating_sub(1);
        let n_fat = le32(header, 44) as usize;
        let first_dir = le32(header, 48);
        let mini_cutoff = u64::from(le32(header, 56));
        let first_minifat = le32(header, 60);
        let first_difat = le32(header, 68);
        if n_fat > n_sectors {
            return Err(invalid(format!("{n_fat} FAT sectors in a {}-byte file", data.len())));
        }
        let mut cf = CompoundFile {
            data,
            version,
            header_clsid: guid(header, 8),
            sector_size,
            mini_cutoff,
            fat: Vec::new(),
            minifat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
            limits,
            read_total: Cell::new(0),
            names: OnceCell::new(),
        };
        let mut claims = Claims::new(n_sectors);

        // FAT sector numbers: 109 in the header, then the DIFAT chain. Each DIFAT sector is
        // claimed, so a DIFAT chain that loops fails at its second visit.
        let mut fat_sectors: Vec<u32> = (0..109).map(|i| le32(header, 76 + i * 4)).filter(|&s| s <= MAXREGSECT).collect();
        let per = sector_size / 4 - 1;
        let mut d = first_difat;
        while d <= MAXREGSECT && fat_sectors.len() < n_fat {
            if !claims.claim(d) {
                return Err(invalid(format!("DIFAT chain loops or leaves the file at sector {d} in a {}-byte file", data.len())));
            }
            let s = cf.sector(d)?;
            fat_sectors.extend((0..per).map(|i| le32(s, i * 4)).filter(|&v| v <= MAXREGSECT));
            d = le32(s, per * 4);
        }
        fat_sectors.truncate(n_fat);
        let mut fat = Vec::with_capacity(fat_sectors.len().saturating_mul(sector_size / 4));
        for s in fat_sectors {
            if !claims.claim(s) {
                return Err(invalid(format!("FAT sector {s} is listed twice or lies outside the file")));
            }
            let b = cf.sector(s)?;
            fat.extend((0..sector_size / 4).map(|i| le32(b, i * 4)));
        }
        cf.fat = fat;

        // Directory: every sector of its chain is claimed, and it holds at most `max_entries`.
        let max_dir_sectors = limits.max_entries.saturating_mul(128).div_ceil(sector_size);
        let dir = cf.read_structural(first_dir, max_dir_sectors, &mut claims, "directory")?;
        let n_entries = dir.len() / 128;
        if n_entries == 0 {
            return Err(invalid("empty directory"));
        }
        if n_entries > limits.max_entries {
            return Err(invalid(format!("{n_entries} directory entries in a {}-byte file (the limit is {})", data.len(), limits.max_entries)));
        }
        cf.entries = dir.chunks_exact(128).map(|e| parse_entry(e, version)).collect();
        if cf.entries.first().map(|e| e.kind) != Some(EntryKind::Root) {
            return Err(invalid("first directory entry is not the root"));
        }

        // Mini FAT and mini stream.
        if first_minifat <= MAXREGSECT {
            let mf = cf.read_structural(first_minifat, n_sectors, &mut claims, "mini FAT")?;
            cf.minifat = mf.chunks_exact(4).map(|c| le32(c, 0)).collect();
        }
        let (root_start, root_size) = cf.entries.first().map_or((ENDOFCHAIN, 0), |r| (r.start, r.size));
        if root_start <= MAXREGSECT && root_size > 0 {
            let size = usize::try_from(root_size).unwrap_or(usize::MAX).min(data.len());
            let sectors = cf.read_structural(root_start, size.div_ceil(sector_size), &mut claims, "mini stream")?;
            cf.mini_stream = sectors;
            cf.mini_stream.truncate(size);
            if cf.mini_stream.len() < size {
                return Err(invalid("mini stream is truncated"));
            }
        }
        cf.build_tree()?;
        Ok(cf)
    }

    /// Sector `n` (a whole sector; a truncated last sector is an error).
    fn sector(&self, n: u32) -> Result<&'a [u8]> {
        if n > MAXREGSECT {
            return Err(invalid(format!("sector {n} is outside the file")));
        }
        let start = (n as usize).checked_add(1).and_then(|k| k.checked_mul(self.sector_size)).ok_or_else(|| invalid("sector number overflow"))?;
        let end = start.checked_add(self.sector_size).ok_or_else(|| invalid("sector number overflow"))?;
        self.data.get(start..end).ok_or_else(|| invalid(format!("sector {n} is outside the file")))
    }

    /// The whole chain of a structure that must not share sectors with another (directory, mini
    /// FAT, mini stream): `max_sectors` long at most, every sector claimed.
    fn read_structural(&self, start: u32, max_sectors: usize, claims: &mut Claims, what: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for s in Chain::new(&self.fat, start, max_sectors) {
            let s = s.map_err(|e| match e {
                Error::Invalid(m) => invalid(format!("{what}: {m}")),
                other => other,
            })?;
            if !claims.claim(s) {
                return Err(invalid(format!("sector {s} of the {what} is used twice or lies outside the file")));
            }
            out.extend_from_slice(self.sector(s)?);
        }
        Ok(out)
    }

    /// The first `size` bytes of the regular sector chain at `start`. `size` is charged against
    /// the read budget before anything is allocated.
    fn read_chain(&self, start: u32, size: usize) -> Result<Vec<u8>> {
        let max_len = (self.data.len() / self.sector_size).max(1);
        let mut out = Vec::with_capacity(size.min(self.data.len()));
        for s in Chain::new(&self.fat, start, max_len) {
            out.extend_from_slice(self.sector(s?)?);
            if out.len() >= size {
                out.truncate(size);
                return Ok(out);
            }
        }
        Err(invalid("stream is truncated"))
    }

    fn read_mini_chain(&self, start: u32, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size.min(self.mini_stream.len()));
        let max_len = (self.mini_stream.len() / MINI_SECTOR_SIZE).max(1);
        for s in Chain::new(&self.minifat, start, max_len) {
            let s = s? as usize;
            let b = s
                .checked_mul(MINI_SECTOR_SIZE)
                .and_then(|at| at.checked_add(MINI_SECTOR_SIZE).and_then(|end| self.mini_stream.get(at..end)))
                .ok_or_else(|| invalid("mini sector outside the mini stream"))?;
            out.extend_from_slice(b);
            if out.len() >= size {
                out.truncate(size);
                return Ok(out);
            }
        }
        Err(invalid("mini stream chain ends early"))
    }

    fn build_tree(&mut self) -> Result<()> {
        let n = self.entries.len();
        let mut owner = vec![false; n];
        if let Some(o) = owner.first_mut() {
            *o = true;
        }
        for parent in 0..n {
            let Some(p) = self.entries.get(parent) else { continue };
            if !matches!(p.kind, EntryKind::Root | EntryKind::Storage) {
                continue;
            }
            let mut kids = Vec::new();
            // in-order walk of the sibling tree, iterative, cycle-safe
            let mut stack: Vec<u32> = Vec::new();
            let mut cur = p.child;
            let mut steps = 0usize;
            while cur != NOSTREAM || !stack.is_empty() {
                while cur != NOSTREAM {
                    let seen = owner.get_mut(cur as usize);
                    let Some(seen) = seen.filter(|o| !**o) else {
                        return Err(invalid(format!("directory entry {cur} is referenced twice or does not exist")));
                    };
                    *seen = true;
                    stack.push(cur);
                    cur = self.entries.get(cur as usize).map_or(NOSTREAM, |e| e.left);
                    steps += 1;
                    if steps > n {
                        return Err(invalid("directory tree loops"));
                    }
                }
                if let Some(top) = stack.pop() {
                    kids.push(top as usize);
                    cur = self.entries.get(top as usize).map_or(NOSTREAM, |e| e.right);
                }
            }
            if let Some(p) = self.entries.get_mut(parent) {
                p.children = kids;
            }
        }
        Ok(())
    }

    pub fn root(&self) -> &Entry {
        self.entries.first().unwrap_or(&NO_ROOT)
    }

    pub fn entry(&self, id: usize) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Child of storage `parent` named `name` (case-insensitive as in §2.6.4).
    pub fn child(&self, parent: usize, name: &str) -> Option<usize> {
        let index = self.names.get_or_init(|| {
            let mut m: HashMap<(usize, Vec<u16>), usize> = HashMap::new();
            for (p, e) in self.entries.iter().enumerate() {
                for &c in &e.children {
                    if let Some(ce) = self.entries.get(c) {
                        // First in sibling order wins, as a linear scan would.
                        m.entry((p, fold_name(&ce.name))).or_insert(c);
                    }
                }
            }
            m
        });
        index.get(&(parent, fold_name(name))).copied()
    }

    /// Entry at a `/`-separated path below the root (`""` is the root).
    pub fn find(&self, path: &str) -> Result<usize> {
        let mut cur = 0;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            cur = self.child(cur, part).ok_or_else(|| Error::NotFound(path.to_string()))?;
        }
        Ok(cur)
    }

    /// Contents of stream `id`.
    ///
    /// The declared size is checked against the file and charged against the read budget
    /// ([`Limits::max_read_bytes`]) before any memory is allocated, so many streams that claim
    /// the same sectors cannot multiply the work.
    pub fn read(&self, id: usize) -> Result<Vec<u8>> {
        let e = self.entries.get(id).ok_or_else(|| Error::NotFound(format!("entry {id}")))?;
        if e.kind != EntryKind::Stream {
            return Err(invalid(format!("{:?} is not a stream", e.name)));
        }
        let size = usize::try_from(e.size).map_err(|_| invalid("stream too large"))?;
        if size == 0 {
            return Ok(Vec::new());
        }
        if size as u64 > self.data.len() as u64 {
            return Err(invalid(format!("stream {:?} is larger than the file", e.name)));
        }
        let total = self.read_total.get().saturating_add(size as u64);
        if total > self.limits.max_read_bytes {
            return Err(invalid(format!("reading stream {:?} would exceed the read budget of {} bytes", e.name, self.limits.max_read_bytes)));
        }
        self.read_total.set(total);
        if e.size < self.mini_cutoff { self.read_mini_chain(e.start, size) } else { self.read_chain(e.start, size) }
    }

    /// Contents of the stream at `path`.
    pub fn read_path(&self, path: &str) -> Result<Vec<u8>> {
        self.read(self.find(path)?)
    }

    /// Sector size in bytes (512 or 4096).
    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    /// The limits this value was opened with.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Stream bytes that may still be read before the budget is spent.
    pub fn read_budget_left(&self) -> u64 {
        self.limits.max_read_bytes.saturating_sub(self.read_total.get())
    }
}

fn parse_entry(b: &[u8], version: u16) -> Entry {
    let name_len = usize::from(le16(b, 64)).min(64);
    let units: Vec<u16> = b.get(..name_len).unwrap_or(&[]).chunks_exact(2).map(|c| le16(c, 0)).take_while(|&u| u != 0).collect();
    let kind = match b.get(66) {
        Some(5) => EntryKind::Root,
        Some(1) => EntryKind::Storage,
        Some(2) => EntryKind::Stream,
        _ => EntryKind::Unknown,
    };
    let size = le64(b, 120);
    Entry {
        name: String::from_utf16_lossy(&units),
        kind,
        clsid: guid(b, 80),
        state_bits: le32(b, 96),
        created: le64(b, 100),
        modified: le64(b, 108),
        start: le32(b, 116),
        // version 3 files may leave garbage in the high 32 bits (§2.6.3)
        size: if version == 3 { size & 0xFFFF_FFFF } else { size },
        children: Vec::new(),
        left: le32(b, 68),
        right: le32(b, 72),
        child: le32(b, 76),
    }
}
