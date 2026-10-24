//! Compound File Binary reader (MS-CFB, versions 3 and 4).

use crate::InterchangeError as E;
use std::path::Path;

/// Largest input accepted (bytes).
pub const MAX_FILE_BYTES: u64 = 1 << 31;
/// Most directory entries listed.
pub const MAX_ENTRIES: usize = 16_384;
const MAX_DEPTH: usize = 32;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const MAXREG: u32 = 0xFFFF_FFFA;
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const NOSTREAM: u32 = 0xFFFF_FFFF;
const MINI_CUTOFF: u32 = 4096;
const MINI_SS: usize = 64;

/// A 16-byte class id, stored as on disk (first three fields little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clsid(pub [u8; 16]);

impl std::fmt::Display for Clsid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Root,
    Storage,
    Stream,
}

/// One storage or stream, with its full `/`-separated path (the root is `""`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    /// Declared size in bytes (streams); 0 for storages.
    pub size: u64,
    pub clsid: Clsid,
    id: u32,
    start: u32,
}

/// An opened container. Holds the bytes; streams are read on demand.
#[derive(Debug)]
pub struct Cfb {
    data: Vec<u8>,
    major: u16,
    minor: u16,
    ss: usize,
    nsectors: u32,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    ministream: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16, E> {
    let b = d.get(o..o.checked_add(2).ok_or(E::Truncated("offset"))?).ok_or(E::Truncated("field"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32, E> {
    let b = d.get(o..o.checked_add(4).ok_or(E::Truncated("offset"))?).ok_or(E::Truncated("field"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn u64_at(d: &[u8], o: usize) -> Result<u64, E> {
    let lo = u32_at(d, o)? as u64;
    let hi = u32_at(d, o + 4)? as u64;
    Ok(lo | (hi << 32))
}

impl Cfb {
    /// Read a file (size-checked before reading).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, E> {
        let p = path.as_ref();
        let len = std::fs::metadata(p).map_err(|e| E::Io(e.to_string()))?.len();
        if len > MAX_FILE_BYTES {
            return Err(E::TooLarge(len));
        }
        Self::from_bytes(std::fs::read(p).map_err(|e| E::Io(e.to_string()))?)
    }

    /// Parse a container from memory.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, E> {
        if data.len() as u64 > MAX_FILE_BYTES {
            return Err(E::TooLarge(data.len() as u64));
        }
        if data.get(..8) != Some(&SIGNATURE[..]) {
            return Err(E::NotCfb);
        }
        if data.len() < 512 {
            return Err(E::Truncated("header"));
        }
        let minor = u16_at(&data, 24)?;
        let major = u16_at(&data, 26)?;
        if u16_at(&data, 28)? != 0xFFFE {
            return Err(E::Unsupported("big-endian compound file"));
        }
        let (sshift, ss) = match (major, u16_at(&data, 30)?) {
            (3, 9) => (9, 512usize),
            (4, 12) => (12, 4096usize),
            (3 | 4, _) => return Err(E::Malformed("sector shift does not match version")),
            _ => return Err(E::Unsupported("compound file major version")),
        };
        let _ = sshift;
        if u16_at(&data, 32)? != 6 {
            return Err(E::Malformed("mini sector shift is not 6"));
        }
        if u32_at(&data, 56)? != MINI_CUTOFF {
            return Err(E::Malformed("mini stream cutoff is not 4096"));
        }
        // Sectors that physically exist after the header sector.
        let body = data.len() - ss.min(data.len());
        let nsectors = u32::try_from(body.div_ceil(ss)).map_err(|_| E::TooLarge(data.len() as u64))?;
        if data.len() < ss {
            return Err(E::Truncated("header sector"));
        }
        let mut c = Cfb { data, major, minor, ss, nsectors, fat: Vec::new(), minifat: Vec::new(), ministream: Vec::new(), entries: Vec::new() };
        c.load_fat()?;
        c.load_directory()?;
        c.load_ministream_and_minifat()?;
        c.validate_sizes()?;
        Ok(c)
    }

    pub fn major_version(&self) -> u16 {
        self.major
    }
    pub fn minor_version(&self) -> u16 {
        self.minor
    }
    pub fn sector_size(&self) -> usize {
        self.ss
    }
    /// All entries, root first, then depth-first in tree order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn root_clsid(&self) -> Clsid {
        self.entries.first().map(|e| e.clsid).unwrap_or_default()
    }
    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    fn sector(&self, id: u32) -> Result<&[u8], E> {
        if id >= self.nsectors {
            return Err(E::Malformed("sector index beyond end of file"));
        }
        let off = (id as usize).checked_add(1).and_then(|n| n.checked_mul(self.ss)).ok_or(E::Malformed("offset overflow"))?;
        // A final partial sector is tolerated by padding semantics only if fully present.
        self.data.get(off..off + self.ss).ok_or(E::Truncated("sector"))
    }

    fn load_fat(&mut self) -> Result<(), E> {
        let nfat = u32_at(&self.data, 44)?;
        if nfat > self.nsectors {
            return Err(E::Malformed("FAT sector count exceeds file"));
        }
        let mut ids: Vec<u32> = Vec::with_capacity(nfat as usize);
        for i in 0..109usize {
            if ids.len() as u32 >= nfat {
                break;
            }
            let v = u32_at(&self.data, 76 + 4 * i)?;
            if v > MAXREG {
                return Err(E::Malformed("bad FAT sector id in header"));
            }
            ids.push(v);
        }
        let mut difat = u32_at(&self.data, 68)?;
        let ndifat = u32_at(&self.data, 72)?;
        if ndifat > self.nsectors {
            return Err(E::Malformed("DIFAT sector count exceeds file"));
        }
        let per = self.ss / 4 - 1;
        let mut seen = 0u32;
        while (ids.len() as u32) < nfat {
            if difat > MAXREG || seen >= ndifat {
                return Err(E::Malformed("DIFAT chain too short for declared FAT size"));
            }
            seen += 1;
            let s = self.sector(difat)?;
            for k in 0..per {
                if ids.len() as u32 >= nfat {
                    break;
                }
                let v = u32_at(s, 4 * k)?;
                if v > MAXREG {
                    return Err(E::Malformed("bad FAT sector id in DIFAT"));
                }
                ids.push(v);
            }
            difat = u32_at(s, self.ss - 4)?;
        }
        let mut fat = Vec::with_capacity((nfat as usize).saturating_mul(self.ss / 4));
        for id in ids {
            let s = self.sector(id)?;
            for k in 0..self.ss / 4 {
                fat.push(u32_at(s, 4 * k)?);
            }
        }
        self.fat = fat;
        Ok(())
    }

    /// Follow a regular-sector chain, returning the sector ids. Cycle and bound checked.
    fn chain(&self, start: u32) -> Result<Vec<u32>, E> {
        let mut out = Vec::new();
        let mut cur = start;
        while cur != ENDOFCHAIN {
            if cur > MAXREG || cur >= self.nsectors {
                return Err(E::Malformed("chain leaves the file"));
            }
            if out.len() as u32 >= self.nsectors {
                return Err(E::Malformed("cyclic sector chain"));
            }
            out.push(cur);
            cur = *self.fat.get(cur as usize).ok_or(E::Malformed("sector not covered by FAT"))?;
        }
        Ok(out)
    }

    fn read_chain(&self, start: u32, size: Option<u64>) -> Result<Vec<u8>, E> {
        let ch = self.chain(start)?;
        let cap = (ch.len() as u64).saturating_mul(self.ss as u64);
        let want = match size {
            Some(s) => {
                if s > cap {
                    return Err(E::Malformed("declared stream size exceeds its sector chain"));
                }
                s as usize
            }
            None => cap as usize,
        };
        let mut out = Vec::with_capacity(want);
        for id in ch {
            let s = self.sector(id)?;
            let take = (want - out.len()).min(s.len());
            out.extend_from_slice(&s[..take]);
            if out.len() == want {
                break;
            }
        }
        Ok(out)
    }

    fn load_directory(&mut self) -> Result<(), E> {
        let first = u32_at(&self.data, 48)?;
        let raw = self.read_chain(first, None)?;
        let n = raw.len() / 128;
        if n == 0 {
            return Err(E::Malformed("empty directory"));
        }
        let n = n.min(MAX_ENTRIES);
        struct Raw {
            name: String,
            ty: u8,
            left: u32,
            right: u32,
            child: u32,
            clsid: Clsid,
            start: u32,
            size: u64,
        }
        let mut rs = Vec::with_capacity(n);
        for i in 0..n {
            let e = &raw[i * 128..i * 128 + 128];
            let nl = (u16_at(e, 64)? as usize).min(64) & !1;
            let units: Vec<u16> = e[..nl].chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).take_while(|&u| u != 0).collect();
            let mut clsid = [0u8; 16];
            clsid.copy_from_slice(&e[80..96]);
            let mut size = u64_at(e, 120)?;
            if self.major == 3 {
                size &= 0xFFFF_FFFF;
            }
            rs.push(Raw {
                name: String::from_utf16_lossy(&units),
                ty: e[66],
                left: u32_at(e, 68)?,
                right: u32_at(e, 72)?,
                child: u32_at(e, 76)?,
                clsid: Clsid(clsid),
                start: u32_at(e, 116)?,
                size,
            });
        }
        if rs[0].ty != 5 {
            return Err(E::Malformed("first directory entry is not the root"));
        }
        let mut visited = vec![false; n];
        visited[0] = true;
        let mut out = vec![Entry {
            path: String::new(),
            name: rs[0].name.clone(),
            kind: EntryKind::Root,
            size: rs[0].size,
            clsid: rs[0].clsid,
            id: 0,
            start: rs[0].start,
        }];
        // Stack of (entry id, parent path, depth); visits siblings via left/right links.
        let mut stack: Vec<(u32, String, usize)> = Vec::new();
        if rs[0].child != NOSTREAM {
            stack.push((rs[0].child, String::new(), 1));
        }
        while let Some((id, parent, depth)) = stack.pop() {
            let i = id as usize;
            let r = rs.get(i).ok_or(E::Malformed("directory link out of range"))?;
            if std::mem::replace(&mut visited[i], true) {
                return Err(E::Malformed("directory tree has a cycle or shared node"));
            }
            if depth > MAX_DEPTH {
                return Err(E::Malformed("directory nesting too deep"));
            }
            let path = if parent.is_empty() { r.name.clone() } else { format!("{parent}/{}", r.name) };
            let kind = match r.ty {
                1 => EntryKind::Storage,
                2 => EntryKind::Stream,
                0 => continue, // unallocated: skip, but do not follow its links
                _ => return Err(E::Malformed("unexpected object type in directory")),
            };
            // Same-level siblings keep the same parent path and depth.
            if r.left != NOSTREAM {
                stack.push((r.left, parent.clone(), depth));
            }
            if r.right != NOSTREAM {
                stack.push((r.right, parent.clone(), depth));
            }
            if kind == EntryKind::Storage && r.child != NOSTREAM {
                stack.push((r.child, path.clone(), depth + 1));
            }
            out.push(Entry {
                path,
                name: r.name.clone(),
                kind,
                size: if kind == EntryKind::Stream { r.size } else { 0 },
                clsid: r.clsid,
                id,
                start: r.start,
            });
        }
        self.entries = out;
        Ok(())
    }

    fn load_ministream_and_minifat(&mut self) -> Result<(), E> {
        let nmini = u32_at(&self.data, 64)?;
        let first = u32_at(&self.data, 60)?;
        if nmini > self.nsectors {
            return Err(E::Malformed("mini FAT sector count exceeds file"));
        }
        if nmini > 0 {
            let raw = self.read_chain(first, None)?;
            self.minifat = raw.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        }
        let root = self.entries.first().ok_or(E::Malformed("no root"))?;
        if root.size > 0 {
            let (start, size) = (root.start, root.size);
            self.ministream = self.read_chain(start, Some(size))?;
        }
        Ok(())
    }

    /// Cheap checks that need no chain walking: a declared size can never exceed what exists.
    fn validate_sizes(&self) -> Result<(), E> {
        for e in self.entries.iter().filter(|e| e.kind == EntryKind::Stream) {
            let limit = if e.size < MINI_CUTOFF as u64 { self.ministream.len() as u64 } else { self.data.len() as u64 };
            if e.size > limit {
                return Err(E::Malformed("declared stream size exceeds the container"));
            }
        }
        Ok(())
    }

    /// Read a stream by path (e.g. `"Header-2/properties"`).
    pub fn read_stream(&self, path: &str) -> Result<Vec<u8>, E> {
        let e = self.find(path).filter(|e| e.kind == EntryKind::Stream).ok_or_else(|| E::NoSuchStream(path.to_string()))?;
        let _ = e.id;
        if e.size >= MINI_CUTOFF as u64 {
            return self.read_chain(e.start, Some(e.size));
        }
        let want = e.size as usize;
        let mut out = Vec::with_capacity(want);
        let mut cur = e.start;
        let mut steps = 0usize;
        while out.len() < want {
            if cur > MAXREG {
                return Err(E::Malformed("mini chain ended early"));
            }
            steps += 1;
            if steps > self.minifat.len() + 1 {
                return Err(E::Malformed("cyclic mini chain"));
            }
            let off = (cur as usize).checked_mul(MINI_SS).ok_or(E::Malformed("offset overflow"))?;
            let s = self.ministream.get(off..off + MINI_SS).or_else(|| self.ministream.get(off..)).ok_or(E::Truncated("mini sector"))?;
            let take = (want - out.len()).min(s.len());
            if take == 0 {
                return Err(E::Truncated("mini sector"));
            }
            out.extend_from_slice(&s[..take]);
            cur = *self.minifat.get(cur as usize).ok_or(E::Malformed("mini sector not covered by mini FAT"))?;
        }
        Ok(out)
    }
}
