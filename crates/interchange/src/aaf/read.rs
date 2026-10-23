//! AAF objects → composition model.

use std::collections::{HashMap, HashSet};

use crate::ticks::{FrameRate, Tick};

use super::ids::{cls, ddef, def, pid};
use super::store::{self, Auid, Obj, Value, rational_of, utf16_of};
use crate::bytes::slice;
use crate::comp::{CClip, CItem, CKind, CMarker, CTrack, CTransition, Composition, Document, Gain, Source, units_to_ticks};
use crate::{Error, Report, Result};

type MobId = [u8; 32];

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataKind {
    Picture,
    Sound,
    Timecode,
    Other,
}

fn data_kind(o: &Obj) -> DataKind {
    match o.weak_key(pid::DATA_DEFINITION).and_then(|k| <Auid>::try_from(k).ok()) {
        Some(k) if k == ddef::PICTURE || k == ddef::LEGACY_PICTURE => DataKind::Picture,
        Some(k) if k == ddef::SOUND || k == ddef::LEGACY_SOUND => DataKind::Sound,
        Some(k) if k == ddef::TIMECODE || k == ddef::LEGACY_TIMECODE => DataKind::Timecode,
        Some(_) => DataKind::Other,
        None => DataKind::Other,
    }
}

fn len_of(o: &Obj) -> i64 {
    o.i64(pid::LENGTH).unwrap_or(0).max(0)
}

/// The value of an indirect property: (type id, bytes).
fn indirect(d: &[u8]) -> Option<(Auid, &[u8])> {
    // a byte-order mark, then the type id and the value; or (older writers) no mark
    if matches!(d.first(), Some(0x4C | 0x42))
        && let (Some(ty), Some(v)) = (slice(d, 1, 16), d.get(17..))
    {
        return Some((ty.try_into().ok()?, v));
    }
    Some((slice(d, 0, 16)?.try_into().ok()?, d.get(16..)?))
}

fn tagged_values(o: &Obj, p: u16) -> Vec<(String, String)> {
    o.objs(p)
        .iter()
        .filter_map(|t| {
            let name = t.string(pid::TAG_NAME)?;
            let (ty, v) = indirect(t.data(pid::TAG_VALUE)?)?;
            let value = if ty == def::TYPE_STRING || v.len() % 2 == 0 { utf16_of(v) } else { String::new() };
            Some((name, value))
        })
        .collect()
}

fn indirect_f64(d: &[u8]) -> Option<f64> {
    let (_, v) = indirect(d)?;
    match v.len() {
        8 => rational_of(v).filter(|r| r.1 != 0).map(|(n, d)| n as f64 / d as f64),
        4 => Some(i32::from_le_bytes(v.try_into().ok()?) as f64),
        _ => None,
    }
}

struct R<'a> {
    report: &'a mut Report,
    mobs: HashMap<MobId, &'a Obj>,
    essence: HashMap<MobId, &'a [u8]>,
    doc: Document,
    /// (file mob id, slot id, master key) → source index
    sources: HashMap<(MobId, u32, String), usize>,
    /// Composition mobs read as nested compositions: their index in `doc.nested`.
    nests: HashMap<MobId, usize>,
    /// The compositions being read, outermost first.
    open: Vec<MobId>,
    /// (mob, slot id) → slot: the first slot of a mob with that id.
    slots: HashMap<(MobId, u32), &'a Obj>,
    /// What a clip's (mob, slot, kind) resolved to, as (source index, offset added to the clip's
    /// start): a file mob is walked once however many clips use it.
    resolved: HashMap<(MobId, u32, CKind), Option<(usize, Tick)>>,
    /// The markers of each master mob, read once.
    master_markers: HashMap<MobId, Vec<CMarker>>,
    /// Markers still allowed into the document (each copy counts).
    marker_budget: usize,
    /// Embedded essence bytes still allowed into the document (each copy counts).
    embed_budget: usize,
}

/// Most markers read from one file.
const MAX_MARKERS: usize = 200_000;

/// Compositions nested deeper than this are read as gaps.
const MAX_NESTING: usize = 16;

pub(crate) fn read(bytes: &[u8], report: &mut Report) -> Result<Document> {
    let root = store::read(bytes).map_err(|e| Error::Parse { format: "AAF", message: e })?;
    let header = root.strong(pid::ROOT_HEADER).ok_or_else(|| Error::Parse { format: "AAF", message: "no header object".into() })?;
    let content = header.strong(pid::CONTENT).ok_or_else(|| Error::Parse { format: "AAF", message: "no content storage".into() })?;
    let mut mobs = HashMap::new();
    for m in content.objs(pid::MOBS) {
        if let Some(id) = m.mob_id(pid::MOB_ID) {
            mobs.insert(id, m);
        }
    }
    let mut essence = HashMap::new();
    for e in content.objs(pid::ESSENCE_DATA) {
        if let (Some(id), Some(d)) = (e.mob_id(pid::ESSENCE_MOB_ID), e.stream(pid::ESSENCE_STREAM)) {
            essence.insert(id, d);
        }
    }
    let mut slots = HashMap::new();
    for (id, m) in &mobs {
        for s in m.objs(pid::SLOTS) {
            if let Some(n) = s.u32(pid::SLOT_ID) {
                slots.entry((*id, n)).or_insert(s);
            }
        }
    }
    // a copy of essence per exposed channel is normal (a stereo file broken out to two mono
    // sources); the budget is what a hostile file could multiply that into
    let embed_budget = bytes.len().saturating_mul(8).saturating_add(16 << 20);
    let mut r = R {
        report,
        mobs,
        essence,
        doc: Document::default(),
        sources: HashMap::new(),
        nests: HashMap::new(),
        open: Vec::new(),
        slots,
        resolved: HashMap::new(),
        master_markers: HashMap::new(),
        marker_budget: MAX_MARKERS,
        embed_budget,
    };
    // Top-level compositions: those no other composition uses, among the ones tagged top-level
    // when any are. (Premiere Pro tags a nested sequence's composition top-level too; it is still
    // only a nested sequence.) Compositions that only use each other in a circle all count.
    let comps: Vec<&Obj> = content.objs(pid::MOBS).iter().filter(|m| m.class == cls::COMPOSITION_MOB).collect();
    let mut referenced = HashSet::new();
    for c in &comps {
        collect_refs(c, &mut referenced);
    }
    let tagged_top: Vec<&Obj> = comps.iter().copied().filter(|c| c.auid(pid::USAGE_CODE) == Some(def::USAGE_TOP_LEVEL)).collect();
    let pool = if tagged_top.is_empty() { comps } else { tagged_top };
    let unused: Vec<&Obj> = pool.iter().copied().filter(|c| c.mob_id(pid::MOB_ID).is_none_or(|id| !referenced.contains(&id))).collect();
    let top = if unused.is_empty() { pool } else { unused };
    for c in top {
        r.open.extend(c.mob_id(pid::MOB_ID));
        let comp = r.composition(c);
        r.open.clear();
        r.doc.compositions.push(comp);
    }
    if r.doc.compositions.is_empty() {
        return Err(Error::Empty);
    }
    Ok(r.doc)
}

fn collect_refs(o: &Obj, out: &mut HashSet<MobId>) {
    if o.class == cls::SOURCE_CLIP
        && let Some(id) = o.mob_id(pid::SOURCE_ID)
    {
        out.insert(id);
    }
    for (_, v) in &o.props {
        match v {
            Value::Strong(c) => collect_refs(c, out),
            Value::StrongVec(v) | Value::StrongSet(v, _) => v.iter().for_each(|c| collect_refs(c, out)),
            _ => {}
        }
    }
}

/// The most a rate's numerator or denominator may be (real rates are 24000/1001 or 48000/1).
const MAX_RATE_PART: i64 = 10_000_000;

/// A positive rate of sane size, else `None` (a damaged file's rate counts as absent).
fn rate_of(o: &Obj, p: u16) -> Option<(i64, i64)> {
    o.rational(p).filter(|r| (1..=MAX_RATE_PART).contains(&r.0) && (1..=MAX_RATE_PART).contains(&r.1))
}

impl R<'_> {
    fn composition(&mut self, c: &Obj) -> Composition {
        let name = c.string(pid::MOB_NAME).unwrap_or_else(|| "Sequence".into());
        let slots = c.objs(pid::SLOTS);
        let pic_rate = slots
            .iter()
            .filter(|s| s.class == cls::TIMELINE_MOB_SLOT)
            .find(|s| s.strong(pid::SEGMENT).is_some_and(|g| matches!(data_kind(g), DataKind::Picture | DataKind::Timecode)))
            .and_then(|s| rate_of(s, pid::EDIT_RATE))
            .or_else(|| slots.iter().find_map(|s| rate_of(s, pid::EDIT_RATE)))
            .unwrap_or((25, 1));
        let rate = FrameRate::new(pic_rate.0, pic_rate.1);
        let tags = tagged_values(c, pid::MOB_USER_COMMENTS);
        let tag = |n: &str| tags.iter().find(|(k, _)| crate::tags::aaf_is(k, n)).map(|(_, v)| v.clone());
        let (mut width, mut height) = tag(crate::tags::FRAME_SIZE)
            .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))))
            .unwrap_or((0, 0));
        let mut sample_rate = tag(crate::tags::AUDIO_SAMPLE_RATE).and_then(|s| s.parse().ok()).unwrap_or(0u32);
        let mut comp = Composition {
            name,
            rate,
            sample_rate: 48_000,
            width: 1920,
            height: 1080,
            start_tc: 0,
            drop: false,
            tracks: Vec::new(),
            markers: Vec::new(),
        };
        let (mut nv, mut na) = (0, 0);
        for s in slots {
            let Some(seg) = s.strong(pid::SEGMENT) else { continue };
            if s.class == cls::EVENT_MOB_SLOT {
                let er = rate_of(s, pid::EVENT_EDIT_RATE).unwrap_or(pic_rate);
                let found = markers_of(seg, er);
                comp.markers.extend(self.admit_markers(found));
                continue;
            }
            let er = rate_of(s, pid::EDIT_RATE).unwrap_or(pic_rate);
            let origin = s.i64(pid::ORIGIN).unwrap_or(0);
            match data_kind(seg) {
                DataKind::Timecode => {
                    if let Some(tc) = find_class(seg, cls::TIMECODE) {
                        comp.start_tc = tc.i64(pid::TC_START).unwrap_or(0);
                        comp.drop = tc.u8(pid::TC_DROP).unwrap_or(0) != 0;
                    }
                }
                kind @ (DataKind::Picture | DataKind::Sound) => {
                    let ckind = if kind == DataKind::Picture { CKind::Picture } else { CKind::Sound };
                    if ckind == CKind::Sound && sample_rate == 0 && er.1 == 1 && er.0 >= 8000 {
                        sample_rate = u32::try_from(er.0).unwrap_or(0);
                    }
                    // Premiere Pro writes every video track into one slot: a nested scope with a
                    // segment per track, the lowest track first. (A scope whose last segment
                    // refers to the others is an effect over layers: one track, as before.)
                    let layers: Vec<&Obj> = if ckind == CKind::Picture && seg.class == cls::NESTED_SCOPE && !refers_to_scope(seg, 0) {
                        seg.objs(pid::NESTED_SLOTS).iter().collect()
                    } else {
                        Vec::new()
                    };
                    let layered = !layers.is_empty();
                    let whole = units_to_ticks(len_of(seg), er.0, er.1);
                    for layer in if layered { layers } else { vec![seg] } {
                        let mut items = self.segment(layer, er, ckind, 0);
                        if layered {
                            fit(&mut items, whole);
                        }
                        if origin > 0 {
                            // material before the origin is not shown
                            trim_front(&mut items, units_to_ticks(origin, er.0, er.1));
                        }
                        let number = s.u32(pid::PHYSICAL_TRACK_NUMBER).unwrap_or(0);
                        let name = s.string(pid::SLOT_NAME).unwrap_or_default();
                        let channels = if ckind == CKind::Sound {
                            items
                                .iter()
                                .filter_map(|i| match i {
                                    CItem::Clip(c) => self.doc.sources.get(c.source).map(|s| s.channels),
                                    _ => None,
                                })
                                .max()
                                .unwrap_or(1)
                                .min(6)
                        } else {
                            0
                        };
                        let n = if ckind == CKind::Picture {
                            nv += 1;
                            nv
                        } else {
                            na += 1;
                            na
                        };
                        comp.tracks.push(CTrack { kind: ckind, name, number: if number > 0 && !layered { number } else { n }, channels, items });
                    }
                }
                DataKind::Other => self.report.info("a slot of an unsupported data kind was skipped"),
            }
        }
        if width == 0 {
            // no size in the file: that of the first picture this composition shows, else of any
            let own = comp.tracks.iter().filter(|t| t.kind == CKind::Picture).flat_map(|t| &t.items).find_map(|i| match i {
                CItem::Clip(c) => self.doc.sources.get(c.source).filter(|s| s.kind == CKind::Picture && s.width > 0 && s.nested.is_none()),
                _ => None,
            });
            let pic = own.or_else(|| self.doc.sources.iter().find(|s| s.kind == CKind::Picture && s.width > 0));
            (width, height) = pic.map_or((1920, 1080), |p| (p.width, p.height));
        }
        comp.width = width;
        comp.height = height;
        if sample_rate > 0 {
            comp.sample_rate = sample_rate;
        }
        comp
    }

    /// `found` cut to what the marker budget still allows.
    fn admit_markers(&mut self, mut found: Vec<CMarker>) -> Vec<CMarker> {
        if found.len() > self.marker_budget {
            found.truncate(self.marker_budget);
            self.report.warn("markers beyond the import limit were dropped");
        }
        self.marker_budget = self.marker_budget.saturating_sub(found.len());
        found
    }

    /// Items of a segment (a sequence or a single component).
    fn segment(&mut self, seg: &Obj, rate: (i64, i64), kind: CKind, depth: usize) -> Vec<CItem> {
        if depth > 32 {
            self.report.warn("segments nested too deeply were skipped");
            return vec![CItem::Filler(units_to_ticks(len_of(seg), rate.0, rate.1))];
        }
        if seg.class == cls::SEQUENCE {
            let mut out = Vec::new();
            for c in seg.objs(pid::COMPONENTS) {
                out.extend(self.component(c, rate, kind, depth + 1));
            }
            return out;
        }
        self.component(seg, rate, kind, depth + 1)
    }

    fn component(&mut self, c: &Obj, rate: (i64, i64), kind: CKind, depth: usize) -> Vec<CItem> {
        let len = units_to_ticks(len_of(c), rate.0, rate.1);
        match c.class {
            x if x == cls::FILLER => vec![CItem::Filler(len)],
            x if x == cls::SOURCE_CLIP => vec![self.clip(c, rate, kind)],
            x if x == cls::SEQUENCE => self.segment(c, rate, kind, depth),
            x if x == cls::TRANSITION => {
                let cut = units_to_ticks(c.i64(pid::CUT_POINT).unwrap_or(0), rate.0, rate.1);
                let og = c.strong(pid::OPERATION_GROUP);
                let effect = og
                    .and_then(|o| {
                        tagged_values(o, pid::COMPONENT_USER_COMMENTS)
                            .into_iter()
                            .find(|(k, _)| crate::tags::aaf_is(k, crate::tags::EFFECT))
                            .map(|(_, v)| v)
                    })
                    .unwrap_or_else(|| {
                        let op = og.and_then(|o| o.weak_key(pid::OPERATION)).and_then(|k| <Auid>::try_from(k).ok());
                        match (kind, op) {
                            (CKind::Sound, _) => "equal_power".into(),
                            (_, Some(o)) if o == def::VIDEO_FADE_TO_BLACK => "dip_to_black".into(),
                            (_, Some(o)) if o == def::SMPTE_VIDEO_WIPE => "wipe".into(),
                            _ => "cross_dissolve".into(),
                        }
                    });
                vec![CItem::Transition(CTransition { len, cut: cut.clamp(Tick::ZERO, len), effect })]
            }
            x if x == cls::OPERATION_GROUP => {
                let op = c.weak_key(pid::OPERATION).and_then(|k| <Auid>::try_from(k).ok());
                let inputs = c.objs(pid::INPUT_SEGMENTS);
                let Some(input) = inputs.first() else {
                    self.report.info("an effect without input was imported as a gap");
                    return vec![CItem::Filler(len)];
                };
                let gain = if op == Some(def::MONO_AUDIO_GAIN) {
                    self.gain(c, len, rate)
                } else {
                    self.report.warn("effects other than audio gain are not imported (their first input is used)");
                    None
                };
                let mut items = self.segment(input, rate, kind, depth);
                // the operation's length rules
                fit(&mut items, len);
                if let Some(g) = gain {
                    match items.as_mut_slice() {
                        [CItem::Clip(cl)] => cl.gain = Some(g),
                        _ => self.report.warn("audio gain around several clips was not imported"),
                    }
                }
                items
            }
            x if x == cls::SELECTOR => match c.strong(pid::SELECTED) {
                Some(s) => {
                    let mut items = self.segment(s, rate, kind, depth);
                    fit(&mut items, len);
                    items
                }
                None => vec![CItem::Filler(len)],
            },
            x if x == cls::NESTED_SCOPE => match c.objs(pid::NESTED_SLOTS).last() {
                Some(s) => {
                    let mut items = self.segment(s, rate, kind, depth);
                    fit(&mut items, len);
                    items
                }
                None => vec![CItem::Filler(len)],
            },
            _ => {
                self.report.info("an unsupported component was imported as a gap");
                vec![CItem::Filler(len)]
            }
        }
    }

    fn gain(&mut self, og: &Obj, len: Tick, _rate: (i64, i64)) -> Option<Gain> {
        let p = og
            .objs(pid::PARAMETERS)
            .iter()
            .find(|p| p.auid(pid::PARAMETER_DEFINITION) == Some(def::PARAM_AMPLITUDE))
            .or_else(|| og.objs(pid::PARAMETERS).first())?;
        if p.class == cls::CONSTANT_VALUE {
            return p.data(pid::CONSTANT_VALUE).and_then(indirect_f64).map(Gain::Constant);
        }
        if p.class == cls::VARYING_VALUE {
            let linear = p.weak_key(pid::INTERPOLATION).and_then(|k| <Auid>::try_from(k).ok()) != Some(def::INTERP_CONSTANT);
            let points: Vec<(Tick, f64)> = p
                .objs(pid::POINT_LIST)
                .iter()
                .filter_map(|cp| {
                    let (n, d) = cp.rational(pid::CP_TIME).filter(|r| r.1 != 0)?;
                    let v = cp.data(pid::CP_VALUE).and_then(indirect_f64)?;
                    Some((Tick((len.0 as i128 * n as i128 / d as i128).clamp(-(1i128 << 56), 1i128 << 56) as i64), v))
                })
                .collect();
            return (!points.is_empty()).then_some(Gain::Varying { linear, points });
        }
        None
    }

    fn clip(&mut self, c: &Obj, rate: (i64, i64), kind: CKind) -> CItem {
        let len = units_to_ticks(len_of(c), rate.0, rate.1);
        let start = units_to_ticks(c.i64(pid::START_TIME).unwrap_or(0), rate.0, rate.1);
        let name = tagged_values(c, pid::COMPONENT_USER_COMMENTS).into_iter().find(|(k, _)| k == "Clip Name").map(|(_, v)| v).unwrap_or_default();
        let (Some(id), Some(slot)) = (c.mob_id(pid::SOURCE_ID), c.u32(pid::SOURCE_MOB_SLOT_ID)) else { return CItem::Filler(len) };
        if id == [0; 32] {
            return CItem::Filler(len);
        }
        if let Some(mob) = self.mobs.get(&id).copied().filter(|m| m.class == cls::COMPOSITION_MOB) {
            return self.nest_clip(c, mob, id, slot, rate, kind, name);
        }
        match self.resolve(id, slot, start, kind) {
            Some((source, t)) => CItem::Clip(CClip { len, source, start: t, gain: None, name }),
            None => CItem::Filler(len),
        }
    }

    /// A source clip that points at a composition: a nested sequence (Premiere Pro writes its
    /// nested sequences this way). The composition is read once, however many clips use it.
    fn nest_clip(&mut self, c: &Obj, mob: &Obj, id: MobId, slot_id: u32, rate: (i64, i64), kind: CKind, name: String) -> CItem {
        let len = units_to_ticks(len_of(c), rate.0, rate.1);
        let index = match self.nests.get(&id) {
            Some(&i) => i,
            None => {
                if self.open.contains(&id) || self.open.len() >= MAX_NESTING {
                    self.report.warn("a composition nested in itself or nested too deeply was imported as a gap");
                    return CItem::Filler(len);
                }
                self.open.push(id);
                let comp = self.composition(mob);
                self.open.pop();
                let i = self.doc.nested.len();
                self.doc.nested.push(comp);
                self.nests.insert(id, i);
                i
            }
        };
        // the start counts edit units of the slot the clip points at
        let slot_rate = self.slots.get(&(id, slot_id)).and_then(|s| rate_of(s, pid::EDIT_RATE)).unwrap_or(rate);
        let start = units_to_ticks(c.i64(pid::START_TIME).unwrap_or(0), slot_rate.0, slot_rate.1);
        let skey = (id, if kind == CKind::Picture { 0 } else { 1 }, "composition".to_string());
        let source = match self.sources.get(&skey) {
            Some(&i) => i,
            None => {
                let Some(comp) = self.doc.nested.get(index) else { return CItem::Filler(len) };
                let length = comp
                    .tracks
                    .iter()
                    .map(|t| {
                        Tick(t.items.iter().fold(0i64, |a, i| {
                            if matches!(i, CItem::Transition(_)) { a.saturating_sub(i.len().0) } else { a.saturating_add(i.len().0) }
                        }))
                    })
                    .max()
                    .unwrap_or(Tick::ZERO);
                let channels = if kind == CKind::Sound { 2 } else { 0 };
                let s = Source {
                    key: format!("composition:{}", hex(&id)),
                    name: comp.name.clone(),
                    kind,
                    path: None,
                    channel: None,
                    channels,
                    file_channels: channels,
                    width: comp.width,
                    height: comp.height,
                    frame_rate: comp.rate,
                    sample_rate: comp.sample_rate,
                    bits: 16,
                    length,
                    offset: Tick::ZERO,
                    start_tc: None,
                    tc_rate: comp.rate,
                    embedded: None,
                    markers: Vec::new(),
                    nested: Some(index),
                };
                let i = self.doc.sources.len();
                self.doc.sources.push(s);
                self.sources.insert(skey, i);
                i
            }
        };
        CItem::Clip(CClip { len, source, start, gain: None, name })
    }

    /// Follow a source reference to a file source mob: (source index, file time). A (mob, slot)
    /// is walked once, however many clips use it.
    fn resolve(&mut self, id: MobId, slot_id: u32, start: Tick, kind: CKind) -> Option<(usize, Tick)> {
        let key = (id, slot_id, kind);
        let (source, delta) = match self.resolved.get(&key) {
            Some(r) => (*r)?,
            None => {
                // the chain of mobs only adds offsets to the start, so it is walked from zero
                let r = self.walk(id, slot_id, Tick::ZERO, kind, None, 0);
                self.resolved.insert(key, r);
                r?
            }
        };
        Some((source, start + delta))
    }

    /// The markers of a master mob (read once).
    fn markers_of_master(&mut self, id: MobId, mob: &Obj, rate: (i64, i64)) -> Vec<CMarker> {
        if let Some(m) = self.master_markers.get(&id) {
            return m.clone();
        }
        let found: Vec<CMarker> = mob
            .objs(pid::SLOTS)
            .iter()
            .filter(|s| s.class == cls::EVENT_MOB_SLOT)
            .flat_map(|s| s.strong(pid::SEGMENT).map(|g| markers_of(g, rate_of(s, pid::EVENT_EDIT_RATE).unwrap_or(rate))).unwrap_or_default())
            .collect();
        let found = self.admit_markers(found);
        self.master_markers.insert(id, found.clone());
        found
    }

    fn walk(&mut self, id: MobId, slot_id: u32, start: Tick, kind: CKind, master_key: Option<String>, depth: usize) -> Option<(usize, Tick)> {
        if depth > 16 {
            return None;
        }
        let Some(mob) = self.mobs.get(&id).copied() else {
            self.report.warn("a clip references a mob that is not in the file (imported as a gap)");
            return None;
        };
        let slot = self.slots.get(&(id, slot_id)).copied()?;
        let rate = rate_of(slot, pid::EDIT_RATE).unwrap_or((25, 1));
        let seg = slot.strong(pid::SEGMENT)?;
        if mob.class == cls::COMPOSITION_MOB {
            self.report.warn("nested compositions are not supported (imported as gaps)");
            return None;
        }
        if mob.class == cls::MASTER_MOB {
            let key = master_key.unwrap_or_else(|| hex(&id));
            let clip = first_clip(seg)?;
            let inner = units_to_ticks(clip.i64(pid::START_TIME).unwrap_or(0), rate.0, rate.1);
            let next = clip.mob_id(pid::SOURCE_ID)?;
            let next_slot = clip.u32(pid::SOURCE_MOB_SLOT_ID)?;
            if next == [0; 32] {
                return None;
            }
            let r = self.walk(next, next_slot, start + inner, kind, Some(key), depth + 1)?;
            if self.doc.sources.get(r.0).is_some_and(|s| s.markers.is_empty()) {
                let found = self.markers_of_master(id, mob, rate);
                let found = self.admit_markers(found);
                if let Some(src) = self.doc.sources.get_mut(r.0) {
                    src.markers = found;
                }
            }
            if let Some(src) = self.doc.sources.get_mut(r.0)
                && src.name.is_empty()
                && let Some(n) = mob.string(pid::MOB_NAME)
            {
                src.name = n;
            }
            return Some(r);
        }
        // a source mob: file (has a file descriptor) or physical (tape / import)
        let desc = mob.strong(pid::ESSENCE_DESCRIPTION);
        let is_file = desc.is_some_and(|d| d.get(pid::SAMPLE_RATE).is_some() || d.get(pid::LOCATOR).is_some() || self.essence.contains_key(&id));
        if !is_file {
            return None;
        }
        let key = master_key.unwrap_or_else(|| hex(&id));
        let skey = (id, slot_id, key.clone());
        if let Some(&i) = self.sources.get(&skey) {
            return Some((i, start));
        }
        let d = desc?;
        let seg_kind = match data_kind(seg) {
            DataKind::Picture => CKind::Picture,
            DataKind::Sound => CKind::Sound,
            _ => kind,
        };
        let path = d.objs(pid::LOCATOR).iter().find_map(|l| l.string(pid::URL_STRING)).map(|u| crate::common::file_url_to_path(&u));
        let file_rate = rate_of(d, pid::SAMPLE_RATE).unwrap_or(rate);
        let length = d
            .i64(pid::FILE_LENGTH)
            .map(|l| units_to_ticks(l, file_rate.0, file_rate.1))
            .unwrap_or_else(|| units_to_ticks(len_of(seg), rate.0, rate.1));
        let sound_slots = mob.objs(pid::SLOTS).iter().filter(|s| s.strong(pid::SEGMENT).is_some_and(|g| data_kind(g) == DataKind::Sound)).count();
        let file_channels = d.u32(pid::CHANNELS).unwrap_or(1).clamp(1, MAX_CHANNELS);
        let channel = (seg_kind == CKind::Sound && file_channels > 1 && sound_slots > 1)
            .then(|| slot.u32(pid::PHYSICAL_TRACK_NUMBER).unwrap_or(slot_id).saturating_sub(1).min(file_channels.saturating_sub(1)));
        let mut layout = d.u8(pid::FRAME_LAYOUT).unwrap_or(0);
        if layout > 4 {
            layout = 0;
        }
        let stored_h = d.u32(pid::STORED_HEIGHT).unwrap_or(0);
        let fr = FrameRate::new(file_rate.0, file_rate.1);
        // timecode from the physical source mob
        let tc = first_clip(seg).and_then(|clip| {
            let tid = clip.mob_id(pid::SOURCE_ID)?;
            let tape = self.mobs.get(&tid)?;
            let tslot = tape.objs(pid::SLOTS).iter().find_map(|s| {
                let g = s.strong(pid::SEGMENT)?;
                let t = find_class(g, cls::TIMECODE)?;
                Some((t.i64(pid::TC_START).unwrap_or(0), i64::from(t.u16(pid::TC_FPS).unwrap_or(25).max(1)), rate_of(s, pid::EDIT_RATE)))
            })?;
            let off = units_to_ticks(clip.i64(pid::START_TIME).unwrap_or(0), rate.0, rate.1);
            let tcr = tslot.2.map(|r| FrameRate::new(r.0, r.1)).unwrap_or_else(|| FrameRate::new(tslot.1, 1));
            Some((tslot.0.saturating_add(tcr.frame_at(off)), tcr))
        });
        let mut s = Source {
            key,
            name: mob.string(pid::MOB_NAME).unwrap_or_default(),
            kind: seg_kind,
            path,
            channel,
            channels: if channel.is_some() { 1 } else { file_channels },
            file_channels,
            width: d.u32(pid::STORED_WIDTH).unwrap_or(0).min(MAX_DIMENSION),
            height: if matches!(layout, 1 | 3) { stored_h.saturating_mul(2) } else { stored_h }.min(MAX_DIMENSION),
            frame_rate: if seg_kind == CKind::Picture { fr } else { FrameRate::FPS_25 },
            sample_rate: d
                .rational(pid::AUDIO_SAMPLING_RATE)
                .filter(|r| r.1 > 0 && r.0 > 0)
                .and_then(|r| u32::try_from(r.0 / r.1).ok())
                .unwrap_or_else(|| u32::try_from(file_rate.0).unwrap_or(0)),
            bits: u16::try_from(d.u32(pid::QUANTIZATION_BITS).unwrap_or(16).clamp(8, 32)).unwrap_or(16),
            length,
            offset: Tick::ZERO,
            start_tc: tc.map(|t| t.0),
            tc_rate: tc.map(|t| t.1).unwrap_or(fr),
            embedded: None,
            markers: Vec::new(),
            nested: None,
        };
        if s.kind == CKind::Sound {
            s.frame_rate = FrameRate::FPS_25;
            if let Some(data) = self.essence.get(&id).copied() {
                // embedded essence may be a whole WAVE / AIFF file
                match crate::wav::parse_wav(data) {
                    Some((pcm, ch, sr, bits)) => {
                        s.file_channels = u32::from(ch).clamp(1, MAX_CHANNELS);
                        s.channels = if s.channel.is_some() { 1 } else { s.file_channels };
                        s.sample_rate = sr;
                        s.bits = bits;
                        s.embedded = self.admit_embedded(pcm);
                    }
                    None => s.embedded = self.admit_embedded(data.to_vec()),
                }
            }
        }
        if s.kind == CKind::Picture && s.width == 0 {
            s.width = 1920;
            s.height = 1080;
        }
        let i = self.doc.sources.len();
        self.doc.sources.push(s);
        self.sources.insert(skey, i);
        Some((i, start))
    }

    /// `data` as a source's embedded essence, if the document may still take that many bytes.
    fn admit_embedded(&mut self, data: Vec<u8>) -> Option<Vec<u8>> {
        if data.len() > self.embed_budget {
            self.report.warn("embedded audio beyond the import limit was not read");
            return None;
        }
        self.embed_budget -= data.len();
        Some(data)
    }
}

/// Most channels a file or track may declare.
const MAX_CHANNELS: u32 = 64;
/// Largest picture width or height read.
const MAX_DIMENSION: u32 = 65_536;

/// Drop `t` from the front of a track's items (slot origin).
fn trim_front(items: &mut Vec<CItem>, mut t: Tick) {
    // whole items that lie before `t` (a transition there has nothing left to overlap)
    let mut cut = 0;
    while t > Tick::ZERO {
        match items.get(cut) {
            None => break,
            Some(CItem::Transition(_)) => cut += 1,
            Some(item) if item.len() <= t => {
                t -= item.len();
                cut += 1;
            }
            Some(_) => break,
        }
    }
    items.drain(..cut);
    if t > Tick::ZERO {
        match items.first_mut() {
            Some(CItem::Filler(x)) => *x -= t,
            Some(CItem::Clip(c)) => {
                c.len -= t;
                c.start += t;
            }
            _ => {}
        }
    }
}

/// Make the items exactly `len` long (an operation group / selector is as long as itself).
fn fit(items: &mut Vec<CItem>, len: Tick) {
    let total = Tick(
        items.iter().fold(0i64, |a, i| if matches!(i, CItem::Transition(_)) { a.saturating_sub(i.len().0) } else { a.saturating_add(i.len().0) }),
    );
    if total == len || items.is_empty() {
        if items.is_empty() {
            items.push(CItem::Filler(len));
        }
        return;
    }
    if total < len {
        items.push(CItem::Filler(len - total));
        return;
    }
    let mut excess = total - len;
    while excess > Tick::ZERO {
        match items.last_mut() {
            Some(CItem::Filler(x)) | Some(CItem::Clip(CClip { len: x, .. })) if *x > excess => {
                *x -= excess;
                excess = Tick::ZERO;
            }
            Some(CItem::Transition(_)) => {
                items.pop();
            }
            Some(other) => {
                excess -= other.len();
                items.pop();
            }
            None => break,
        }
    }
}

/// Whether anything inside `o` is a scope reference (a nested scope used as an effect's layers).
fn refers_to_scope(o: &Obj, depth: usize) -> bool {
    if o.class == cls::SCOPE_REFERENCE {
        return true;
    }
    if depth > 32 {
        return false;
    }
    o.props.iter().any(|(_, v)| match v {
        Value::Strong(c) => refers_to_scope(c, depth + 1),
        Value::StrongVec(v) | Value::StrongSet(v, _) => v.iter().any(|c| refers_to_scope(c, depth + 1)),
        _ => false,
    })
}

fn first_clip(seg: &Obj) -> Option<&Obj> {
    if seg.class == cls::SOURCE_CLIP {
        return Some(seg);
    }
    if seg.class == cls::SEQUENCE {
        return seg.objs(pid::COMPONENTS).iter().find_map(first_clip);
    }
    // essence group (choices 0x0501) / operation group inputs
    seg.objs(0x0501).iter().find_map(first_clip).or_else(|| seg.objs(pid::INPUT_SEGMENTS).iter().find_map(first_clip))
}

fn find_class(seg: &Obj, class: Auid) -> Option<&Obj> {
    if seg.class == class {
        return Some(seg);
    }
    seg.objs(pid::COMPONENTS).iter().find_map(|c| find_class(c, class))
}

fn markers_of(seg: &Obj, rate: (i64, i64)) -> Vec<CMarker> {
    let events: Vec<&Obj> = if seg.class == cls::SEQUENCE { seg.objs(pid::COMPONENTS).iter().collect() } else { vec![seg] };
    events
        .into_iter()
        .filter(|e| e.class == cls::COMMENT_MARKER || e.class == cls::DESCRIPTIVE_MARKER || e.get(pid::POSITION).is_some())
        .map(|e| {
            let tags = tagged_values(e, pid::COMPONENT_USER_COMMENTS);
            let tag = |n: &str| tags.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
            let comment = e.string(pid::COMMENT).unwrap_or_default();
            let start = units_to_ticks(e.i64(pid::POSITION).unwrap_or(0), rate.0, rate.1);
            CMarker {
                start,
                duration: units_to_ticks(e.i64(pid::LENGTH).unwrap_or(0).max(0), rate.0, rate.1),
                name: tag("Name").unwrap_or_else(|| comment.clone()),
                comment: tag("Comment").unwrap_or_else(|| if tags.is_empty() { String::new() } else { comment.clone() }),
                color: tag("Color"),
            }
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
