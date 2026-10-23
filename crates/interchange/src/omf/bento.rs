//! The Bento container (Apple, *Bento Specification* revision 1.0d5), the storage layer of OMF
//! Interchange 2.0 files.
//!
//! A Bento container is a heap of value bytes, a table of contents (TOC) describing every value of
//! every object, and a fixed-size label at the very end of the file that locates the TOC:
//!
//! ```text
//! label (24 bytes, big-endian): magic A4 'C' 'M' A5 'H' 'd' 'r' D7, u16 flags, u16 buffer size
//!                               (KiB), u16 major version (1), u16 minor version (0),
//!                               u32 TOC offset, u32 TOC size
//! ```
//!
//! Every value belongs to an (object id, property id, type id) triple. Properties and types are
//! themselves objects carrying a global name (`"OMFI:CPNT:Length"`, `"omfi:Length32"`). The TOC
//! is a stream of one-byte operations, each followed by its operands (big-endian):
//!
//! | op | operands | meaning |
//! |---|---|---|
//! | 1 NewObject | u32 id | following entries belong to this object |
//! | 2 NewProperty | u32 id | … to this property |
//! | 3 NewType | u32 id | starts a new value of this type |
//! | 4 ExplicitGen | u32 | generation of the value |
//! | 5 Offset4Len4 | u32 offset, u32 length | a value segment in the file |
//! | 6 ContdOffset4Len4 | u32 offset, u32 length | a continuation segment |
//! | 7 Offset8Len4 / 8 Contd… | u64 offset, u32 length | segments beyond 4 GiB |
//! | 9–13 Immediate0–4 | 4 bytes | a value of 0–4 bytes stored in the TOC |
//! | 14 ContdImmediate4 | 4 bytes | continuation |
//! | 15 ReferenceListID | u32 id | the object holding this value's reference list |
//! | 24 EndOfBufr | — | skip to the end of the current TOC buffer |
//! | 0xFF Padding | — | ignored |
//!
//! Object references (OMF's `omfi:ObjRef`) are 4-byte keys resolved through the referencing
//! object's reference list (standard property 22: pairs of u32 key, u32 object id). This crate
//! uses the referenced object's id as the key.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::bytes::{array, be16, be32, slice};

pub(crate) const MAGIC: [u8; 8] = [0xA4, b'C', b'M', 0xA5, b'H', b'd', b'r', 0xD7];
pub(crate) const LABEL_SIZE: usize = 24;

// standard object ids
const TOC_OBJECT: u32 = 1;
const TOC_SEED: u32 = 2;
const TOC_MIN_SEED: u32 = 3;
const TOC_TYPE: u32 = 19;
const TYPE_7BIT_ASCII: u32 = 21;
pub(crate) const OBJ_REFERENCES: u32 = 22;
const GLOBAL_TYPE_NAME: u32 = 23;
const GLOBAL_PROP_NAME: u32 = 24;
const MIN_USER_ID: u32 = 100;

const OP_NEW_OBJECT: u8 = 1;
const OP_NEW_PROPERTY: u8 = 2;
const OP_NEW_TYPE: u8 = 3;
const OP_EXPLICIT_GEN: u8 = 4;
const OP_OFFSET4_LEN4: u8 = 5;
const OP_CONTD_OFFSET4_LEN4: u8 = 6;
const OP_OFFSET8_LEN4: u8 = 7;
const OP_CONTD_OFFSET8_LEN4: u8 = 8;
const OP_IMMEDIATE0: u8 = 9;
const OP_IMMEDIATE4: u8 = 13;
const OP_CONTD_IMMEDIATE4: u8 = 14;
const OP_REFERENCE_LIST_ID: u8 = 15;
const OP_END_OF_BUFFER: u8 = 24;
const OP_PADDING: u8 = 0xFF;

const TOC_BUFFER_KIB: u16 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Imm(Vec<u8>),
    At(u64, u32),
}

/// Builds a Bento container.
pub(crate) struct Writer {
    data: Vec<u8>,
    names: HashMap<(u32, String), u32>,
    next: u32,
    /// (object, property, type, segments)
    entries: Vec<(u32, u32, u32, Vec<Seg>)>,
    /// The first limit the content ran into (a value or the file past what the format can hold);
    /// reported by [`Writer::finish`].
    error: Option<String>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { data: Vec::new(), names: HashMap::new(), next: MIN_USER_ID, entries: Vec::new(), error: None }
    }

    fn named(&mut self, kind: u32, name: &str) -> u32 {
        if let Some(&id) = self.names.get(&(kind, name.to_string())) {
            return id;
        }
        let id = self.new_object();
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        let seg = self.store(&v);
        self.entries.push((id, kind, TYPE_7BIT_ASCII, vec![seg]));
        self.names.insert((kind, name.to_string()), id);
        id
    }

    pub fn property(&mut self, name: &str) -> u32 {
        self.named(GLOBAL_PROP_NAME, name)
    }

    pub fn type_id(&mut self, name: &str) -> u32 {
        self.named(GLOBAL_TYPE_NAME, name)
    }

    pub fn new_object(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    fn store(&mut self, v: &[u8]) -> Seg {
        if v.len() <= 4 {
            return Seg::Imm(v.to_vec());
        }
        let at = self.data.len() as u64;
        let Ok(len) = u32::try_from(v.len()) else {
            self.error.get_or_insert_with(|| "a value is larger than 4 GiB (the limit of OMF)".to_string());
            return Seg::Imm(Vec::new());
        };
        self.data.extend_from_slice(v);
        Seg::At(at, len)
    }

    /// Set a value of `obj` (values of more than 4 GiB are not supported by OMF).
    pub fn set(&mut self, obj: u32, property: &str, ty: &str, value: &[u8]) {
        let p = self.property(property);
        let t = self.type_id(ty);
        let seg = self.store(value);
        self.entries.push((obj, p, t, vec![seg]));
    }

    /// The reference list of `obj` (keys are object ids).
    pub fn set_references(&mut self, obj: u32, targets: &[u32]) {
        let mut v = Vec::with_capacity(targets.len() * 8);
        for t in targets {
            v.extend_from_slice(&t.to_be_bytes());
            v.extend_from_slice(&t.to_be_bytes());
        }
        let seg = self.store(&v);
        self.entries.push((obj, OBJ_REFERENCES, TYPE_7BIT_ASCII, vec![seg]));
    }

    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        let seed = self.next;
        self.entries.push((TOC_OBJECT, TOC_SEED, TOC_TYPE, vec![Seg::Imm(seed.to_be_bytes().to_vec())]));
        self.entries.push((TOC_OBJECT, TOC_MIN_SEED, TOC_TYPE, vec![Seg::Imm(MIN_USER_ID.to_be_bytes().to_vec())]));
        self.entries.sort_by_key(|e| (e.0, e.1, e.2));
        let mut toc = Vec::new();
        let (mut cur_obj, mut cur_prop) = (u32::MAX, u32::MAX);
        for (obj, prop, ty, segs) in &self.entries {
            if *obj != cur_obj {
                toc.push(OP_NEW_OBJECT);
                toc.extend_from_slice(&obj.to_be_bytes());
                cur_obj = *obj;
                cur_prop = u32::MAX;
            }
            if *prop != cur_prop {
                toc.push(OP_NEW_PROPERTY);
                toc.extend_from_slice(&prop.to_be_bytes());
                cur_prop = *prop;
            }
            toc.push(OP_NEW_TYPE);
            toc.extend_from_slice(&ty.to_be_bytes());
            for (i, s) in segs.iter().enumerate() {
                match s {
                    Seg::Imm(b) => {
                        // an immediate holds up to 4 bytes (`store` makes only such values)
                        let n = u8::try_from(b.len().min(4)).unwrap_or(4);
                        toc.push(if i > 0 { OP_CONTD_IMMEDIATE4 } else { OP_IMMEDIATE0 + n });
                        let mut w = [0u8; 4];
                        for (dst, src) in w.iter_mut().zip(b) {
                            *dst = *src;
                        }
                        toc.extend_from_slice(&w);
                    }
                    Seg::At(off, len) if u32::try_from(*off).is_ok() => {
                        toc.push(if i > 0 { OP_CONTD_OFFSET4_LEN4 } else { OP_OFFSET4_LEN4 });
                        toc.extend_from_slice(&u32::try_from(*off).unwrap_or(u32::MAX).to_be_bytes());
                        toc.extend_from_slice(&len.to_be_bytes());
                    }
                    Seg::At(off, len) => {
                        toc.push(if i > 0 { OP_CONTD_OFFSET8_LEN4 } else { OP_OFFSET8_LEN4 });
                        toc.extend_from_slice(&off.to_be_bytes());
                        toc.extend_from_slice(&len.to_be_bytes());
                    }
                }
            }
        }
        let mut out = self.data;
        let toc_at = u32::try_from(out.len()).map_err(|_| "the file is larger than 4 GiB (the limit of OMF)".to_string())?;
        let toc_len = u32::try_from(toc.len()).map_err(|_| "the table of contents is too large".to_string())?;
        out.extend_from_slice(&toc);
        let mut label = MAGIC.to_vec();
        label.extend_from_slice(&0u16.to_be_bytes()); // flags
        label.extend_from_slice(&TOC_BUFFER_KIB.to_be_bytes());
        label.extend_from_slice(&1u16.to_be_bytes());
        label.extend_from_slice(&0u16.to_be_bytes());
        label.extend_from_slice(&toc_at.to_be_bytes());
        label.extend_from_slice(&toc_len.to_be_bytes());
        out.extend_from_slice(&label);
        Ok(out)
    }
}

/// One value of a parsed object.
#[derive(Clone, Debug)]
pub(crate) struct Prop {
    pub prop: u32,
    #[allow(dead_code)]
    pub ty: u32,
    segs: Vec<Seg>,
}

impl Prop {
    /// The declared length of the value in bytes.
    fn len(&self) -> usize {
        self.segs
            .iter()
            .map(|s| match s {
                Seg::Imm(b) => b.len(),
                Seg::At(_, len) => usize::try_from(*len).unwrap_or(usize::MAX),
            })
            .fold(0usize, usize::saturating_add)
    }
}

/// A parsed Bento container.
pub(crate) struct Container<'a> {
    data: &'a [u8],
    pub objects: BTreeMap<u32, Vec<Prop>>,
    /// (object, property id) → position among the object's values: the first value with that id.
    index: HashMap<(u32, u32), usize>,
    names: HashMap<u32, String>,
    by_name: HashMap<String, u32>,
    /// Value bytes still allowed to be handed out (every [`Container::value`] charges its length,
    /// so segments that all point at the same big range cannot multiply it).
    budget: Cell<usize>,
    exhausted: Cell<bool>,
    /// Per object: reference key → object id, from its reference list (built on first use).
    ref_maps: RefCell<HashMap<u32, Rc<HashMap<u32, u32>>>>,
}

/// The longest global name (of a property or type) read; real ones are about 30 bytes.
const MAX_NAME_LEN: usize = 256;
/// Most segments one value may be made of.
const MAX_SEGMENTS: usize = 1 << 20;

/// Whether `b` ends with a Bento label.
pub(crate) fn sniff(b: &[u8]) -> bool {
    b.len().checked_sub(LABEL_SIZE).and_then(|at| slice(b, at, MAGIC.len())).is_some_and(|m| m == MAGIC)
}

/// `n` bytes of the TOC at `*i`, advancing `*i`.
fn take<'t>(toc: &'t [u8], i: &mut usize, n: usize) -> Result<&'t [u8], String> {
    let b = slice(toc, *i, n).ok_or_else(|| "truncated TOC".to_string())?;
    *i += n;
    Ok(b)
}

impl<'a> Container<'a> {
    pub fn open(data: &'a [u8]) -> Result<Container<'a>, String> {
        if !sniff(data) {
            return Err("no Bento label at the end of the file".into());
        }
        let label = data.get(data.len().saturating_sub(LABEL_SIZE)..).ok_or("no Bento label at the end of the file")?;
        let buf = usize::from(be16(label, 10).unwrap_or(0)).max(1) * 1024;
        let toc_at = usize::try_from(be32(label, 16).unwrap_or(0)).map_err(|_| "bad TOC offset")?;
        let toc_len = usize::try_from(be32(label, 20).unwrap_or(0)).map_err(|_| "bad TOC size")?;
        let toc = slice(data, toc_at, toc_len).ok_or("the TOC lies outside the file")?;
        let mut c = Container {
            data,
            objects: BTreeMap::new(),
            index: HashMap::new(),
            names: HashMap::new(),
            by_name: HashMap::new(),
            // a copy of the file per value read would be 1x; this allows the many small reads of
            // a normal import and stops a TOC built to read the same bytes over and over
            budget: Cell::new(data.len().saturating_mul(16).saturating_add(4 << 20)),
            exhausted: Cell::new(false),
            ref_maps: RefCell::new(HashMap::new()),
        };
        let (mut obj, mut prop) = (0u32, 0u32);
        let mut i = 0usize;
        while let Some(&op) = toc.get(i) {
            i += 1;
            match op {
                OP_NEW_OBJECT => obj = be32(take(toc, &mut i, 4)?, 0).unwrap_or(0),
                OP_NEW_PROPERTY => prop = be32(take(toc, &mut i, 4)?, 0).unwrap_or(0),
                OP_NEW_TYPE => {
                    let ty = be32(take(toc, &mut i, 4)?, 0).unwrap_or(0);
                    let props = c.objects.entry(obj).or_default();
                    c.index.entry((obj, prop)).or_insert(props.len());
                    props.push(Prop { prop, ty, segs: Vec::new() });
                }
                OP_EXPLICIT_GEN | OP_REFERENCE_LIST_ID => {
                    take(toc, &mut i, 4)?;
                }
                OP_OFFSET4_LEN4 | OP_CONTD_OFFSET4_LEN4 => {
                    let b = take(toc, &mut i, 8)?;
                    let seg = Seg::At(u64::from(be32(b, 0).unwrap_or(0)), be32(b, 4).unwrap_or(0));
                    c.push_seg(obj, seg)?;
                }
                OP_OFFSET8_LEN4 | OP_CONTD_OFFSET8_LEN4 => {
                    let b = take(toc, &mut i, 12)?;
                    let off = array::<8>(b, 0).map(u64::from_be_bytes).ok_or("truncated TOC")?;
                    let seg = Seg::At(off, be32(b, 8).unwrap_or(0));
                    c.push_seg(obj, seg)?;
                }
                x @ (OP_IMMEDIATE0..=OP_IMMEDIATE4 | OP_CONTD_IMMEDIATE4) => {
                    let b = take(toc, &mut i, 4)?;
                    let n = if x == OP_CONTD_IMMEDIATE4 { 4 } else { usize::from(x - OP_IMMEDIATE0) };
                    let seg = Seg::Imm(b.get(..n).unwrap_or(b).to_vec());
                    c.push_seg(obj, seg)?;
                }
                OP_END_OF_BUFFER => {
                    // the rest of this TOC buffer is unused
                    i = (i - 1) / buf * buf + buf;
                }
                OP_PADDING | 0 => {}
                other => return Err(format!("unknown TOC operation {other}")),
            }
        }
        // global names (of properties and types): short values of the two naming properties
        let mut found: Vec<(u32, Vec<u8>)> = Vec::new();
        for (&id, props) in &c.objects {
            let named = props.iter().find(|p| p.prop == GLOBAL_PROP_NAME || p.prop == GLOBAL_TYPE_NAME);
            if let Some(p) = named
                && p.len() <= MAX_NAME_LEN
                && let Ok(v) = c.value(p)
            {
                found.push((id, v));
            }
        }
        for (id, v) in found {
            let n = String::from_utf8_lossy(&v).trim_end_matches('\0').to_string();
            c.by_name.entry(n.clone()).or_insert(id);
            c.names.insert(id, n);
        }
        Ok(c)
    }

    fn push_seg(&mut self, obj: u32, seg: Seg) -> Result<(), String> {
        let p = self.objects.get_mut(&obj).and_then(|v| v.last_mut()).ok_or("a value without a type in the TOC")?;
        if p.segs.len() >= MAX_SEGMENTS {
            return Err("too many value segments".into());
        }
        p.segs.push(seg);
        Ok(())
    }

    /// Whether a read was refused because the budget for value bytes ran out. A reader checks this
    /// when it is done: an import that hit it was missing values and must not be trusted.
    pub fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    /// The bytes of a value.
    pub fn value(&self, p: &Prop) -> Result<Vec<u8>, String> {
        // validate, then charge, then copy: nothing is allocated for a value that is not whole
        let mut total = 0usize;
        for s in &p.segs {
            let n = match s {
                Seg::Imm(b) => b.len(),
                Seg::At(off, len) => {
                    let start = usize::try_from(*off).map_err(|_| "value offset too large")?;
                    let len = usize::try_from(*len).map_err(|_| "bad value length")?;
                    slice(self.data, start, len).ok_or("a value lies outside the file")?;
                    len
                }
            };
            total = total.checked_add(n).ok_or("bad value length")?;
        }
        if total > self.budget.get() {
            self.exhausted.set(true);
            return Err("the file asks for more data than it can hold".into());
        }
        self.budget.set(self.budget.get() - total);
        let mut out = Vec::with_capacity(total);
        for s in &p.segs {
            match s {
                Seg::Imm(b) => out.extend_from_slice(b),
                Seg::At(off, len) => {
                    let b = usize::try_from(*off).ok().zip(usize::try_from(*len).ok()).and_then(|(start, len)| slice(self.data, start, len));
                    out.extend_from_slice(b.ok_or("a value lies outside the file")?);
                }
            }
        }
        Ok(out)
    }

    pub fn id_of(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    #[allow(dead_code)]
    pub fn name_of(&self, id: u32) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// The first value of property `name` of `obj`.
    pub fn get(&self, obj: u32, name: &str) -> Option<Vec<u8>> {
        self.get_id(obj, self.id_of(name)?)
    }

    fn get_id(&self, obj: u32, pid: u32) -> Option<Vec<u8>> {
        let at = *self.index.get(&(obj, pid))?;
        let p = self.objects.get(&obj)?.get(at)?;
        self.value(p).ok()
    }

    /// Resolve an object reference key of `obj` through its reference list.
    pub fn resolve(&self, obj: u32, key: u32) -> u32 {
        let map = self.ref_maps.borrow().get(&obj).cloned();
        let map = match map {
            Some(m) => m,
            None => {
                let mut m = HashMap::new();
                if let Some(v) = self.get_id(obj, OBJ_REFERENCES) {
                    for pair in v.chunks_exact(8) {
                        if let (Some(k), Some(t)) = (be32(pair, 0), be32(pair, 4)) {
                            m.entry(k).or_insert(t);
                        }
                    }
                }
                let m = Rc::new(m);
                self.ref_maps.borrow_mut().insert(obj, Rc::clone(&m));
                m
            }
        };
        map.get(&key).copied().unwrap_or(key)
    }
}
