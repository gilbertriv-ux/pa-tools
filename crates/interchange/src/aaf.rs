//! AAF identification and the header facts reachable from the container alone.
//!
//! PROVENANCE: the root class id and the stream/storage names below are recalled from public AAF
//! documentation and open-source readers' descriptions, not verified against a real AAF file in
//! this repository (none is available in the build container). Identification is therefore a
//! reported [`Confidence`], never a certainty.

use crate::{Cfb, EntryKind, InterchangeError as E};

/// Root-storage class id used by AAF files (`{B3B398A5-1C90-11D4-8053-080036210804}`), stored in
/// on-disk byte order. Recalled, unverified here.
pub const AAF_ROOT_CLSID: crate::Clsid =
    crate::Clsid([0xA5, 0x98, 0xB3, 0xB3, 0x90, 0x1C, 0xD4, 0x11, 0x80, 0x53, 0x08, 0x00, 0x36, 0x21, 0x08, 0x04]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Neither the class id nor the AAF structure was found.
    None,
    /// Only structure matched (header storage present, no class id match).
    Structural,
    /// The root class id matched; the header storage may or may not be there.
    ClassId,
    /// Both matched.
    Strong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AafIdentity {
    pub root_clsid_matches: bool,
    /// A storage named `Header-<n>` exists at the root.
    pub has_header_storage: bool,
    /// A storage named `MetaDictionary-<n>` exists at the root.
    pub has_meta_dictionary: bool,
    pub confidence: Confidence,
}

impl AafIdentity {
    /// True when at least the structure or the class id matched.
    pub fn is_aaf(&self) -> bool {
        self.confidence != Confidence::None
    }
}

/// One entry of a `properties` stream index: local property key, stored form, byte length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyIndexEntry {
    pub key: u16,
    pub stored_form: u16,
    pub length: u16,
}

/// What can be read from `Header-<n>/properties` without an object model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AafHeader {
    /// Path of the stream that was read.
    pub stream_path: String,
    /// `true` for little-endian property data (marker byte `0x4C`), `false` for big (`0x42`).
    pub little_endian: bool,
    /// Second byte of the stream (format version marker); reported raw, not interpreted.
    pub version_marker: u8,
    /// Index entries declared by the stream (key/form/length triples).
    pub index: Vec<PropertyIndexEntry>,
}

impl Cfb {
    /// Decide whether this container looks like AAF. Checks: root class id equals
    /// [`AAF_ROOT_CLSID`]; root has a storage named `Header-<digits>`; root has a storage named
    /// `MetaDictionary-<digits>`.
    pub fn identify(&self) -> AafIdentity {
        let root_clsid_matches = self.root_clsid() == AAF_ROOT_CLSID;
        let named = |prefix: &str| {
            self.entries().iter().any(|e| {
                e.kind == EntryKind::Storage
                    && !e.path.contains('/')
                    && e.name.strip_prefix(prefix).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            })
        };
        let has_header_storage = named("Header-");
        let has_meta_dictionary = named("MetaDictionary-");
        let structural = has_header_storage && has_meta_dictionary;
        let confidence = match (root_clsid_matches, structural) {
            (true, true) => Confidence::Strong,
            (true, false) => Confidence::ClassId,
            (false, true) => Confidence::Structural,
            (false, false) => Confidence::None,
        };
        AafIdentity { root_clsid_matches, has_header_storage, has_meta_dictionary, confidence }
    }

    /// Parse the header's `properties` stream: byte-order marker, version marker, and the
    /// key/form/length index. Property values are NOT decoded. Errors if the file is not AAF-like
    /// or the stream is malformed.
    pub fn aaf_header(&self) -> Result<AafHeader, E> {
        if !self.identify().is_aaf() {
            return Err(E::Unsupported("container does not look like AAF"));
        }
        let hdr = self
            .entries()
            .iter()
            .find(|e| {
                e.kind == EntryKind::Storage
                    && !e.path.contains('/')
                    && e.name.strip_prefix("Header-").is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            })
            .ok_or(E::NoSuchStream("Header-<n>".into()))?;
        let stream_path = format!("{}/properties", hdr.path);
        let d = self.read_stream(&stream_path)?;
        if d.len() < 4 {
            return Err(E::Truncated("properties stream"));
        }
        let little_endian = match d.first() {
            Some(0x4C) => true,
            Some(0x42) => false,
            _ => return Err(E::Malformed("unknown byte-order marker in properties stream")),
        };
        let u16v = |o: usize| -> Result<u16, E> {
            let b = d.get(o..o + 2).ok_or(E::Truncated("properties index"))?;
            Ok(if little_endian { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
        };
        let count = u16v(2)? as usize;
        if count.checked_mul(6).and_then(|n| n.checked_add(4)).is_none_or(|need| need > d.len()) {
            return Err(E::Malformed("properties index larger than its stream"));
        }
        let mut index = Vec::with_capacity(count);
        for i in 0..count {
            let o = 4 + i * 6;
            index.push(PropertyIndexEntry { key: u16v(o)?, stored_form: u16v(o + 2)?, length: u16v(o + 4)? });
        }
        Ok(AafHeader { stream_path, little_endian, version_marker: d.get(1).copied().unwrap_or(0), index })
    }
}
