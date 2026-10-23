//! Structural checks of written AAF files against the required properties and rules of the AAF
//! Object Specification v1.1 and the Edit Protocol, read back through our compound file reader.
//!
//! (Ported from the donor's tests; the fixtures are Pa Tools sessions.)

use std::collections::HashSet;

use soundcraft_model::{Clip, ClipId, Session, SourceId, TrackKind, VideoSource};

use super::ids::{self, cls, ddef, def, path, pid};
use super::store::{self, Obj, Value};
use crate::support::{SR, demo_like, video_session};
use crate::{ExportOptions, Format, ImportOptions, export, import};

fn aaf(s: &Session, small_sectors: bool) -> Vec<u8> {
    export(s, Format::Aaf, &ExportOptions { small_sectors, ..Default::default() }).expect("export").0
}

fn walk<'a>(o: &'a Obj, out: &mut Vec<&'a Obj>) {
    out.push(o);
    for (_, v) in &o.props {
        match v {
            Value::Strong(c) => walk(c, out),
            Value::StrongVec(v) | Value::StrongSet(v, _) => v.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
}

fn keys(dict: &Obj, p: u16) -> HashSet<Vec<u8>> {
    dict.objs(p).iter().filter_map(|d| d.data(pid::IDENTIFICATION).map(<[u8]>::to_vec)).collect()
}

/// #323: readers that load the meta-dictionary (pyaaf2) need its ClassDefinitions and
/// TypeDefinitions sets. They are empty: everything written uses the baseline classes and types
/// those readers already know.
#[test]
fn the_meta_dictionary_has_its_class_and_type_definition_sets() {
    let s = demo_like();
    for small in [false, true] {
        let bytes = aaf(&s, small);
        let cf = soundcraft_cfb::CompoundFile::open(&bytes).expect("open");
        assert_eq!(cf.entries[cf.find("MetaDictionary-1").expect("meta")].clsid, ids::META_DICTIONARY);
        for name in ["ClassDefinitions-3", "TypeDefinitions-4"] {
            // u32 count, u32 first free key, u32 last free key, u16 key pid, u8 key size
            let index = cf.read_path(&format!("MetaDictionary-1/{name} index")).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(index.len(), 15, "{name}: an empty set");
            assert_eq!(index[0..4], 0u32.to_le_bytes(), "{name}: no entries");
            assert_eq!(u16::from_le_bytes([index[12], index[13]]), pid::META_IDENTIFICATION, "{name}: keyed by the definition's AUID");
            assert_eq!(index[14], 16, "{name}: 16-byte keys");
        }
        let root = store::read(&bytes).expect("read");
        let meta = root.strong(pid::ROOT_META_DICTIONARY).expect("meta dictionary");
        for set in [pid::META_CLASS_DEFINITIONS, pid::META_TYPE_DEFINITIONS] {
            assert!(matches!(meta.get(set), Some(Value::StrongSet(items, k)) if items.is_empty() && *k == pid::META_IDENTIFICATION), "{set:#06x}");
        }
    }
}

/// The checks every written file must pass, whatever the session.
fn check_structure(bytes: &[u8], small: bool) -> (Obj, Vec<Obj>) {
    // compound file level
    let cf = soundcraft_cfb::CompoundFile::open(bytes).expect("open");
    assert_eq!(cf.version, if small { 3 } else { 4 });
    assert_eq!(cf.root().clsid, ids::ROOT);
    assert!(cf.read_path("referenced properties").is_ok());
    let h = cf.find("Header-2").expect("header");
    assert_eq!(cf.entries[h].clsid, cls::HEADER);
    assert!(cf.read_path("Header-2/properties").expect("properties").starts_with(&[0x4C, 0x20]));

    // object level
    let root = store::read(bytes).expect("read");
    assert!(root.strong(pid::ROOT_META_DICTIONARY).is_some());
    let header = root.strong(pid::ROOT_HEADER).expect("header");
    for req in [pid::BYTE_ORDER, pid::LAST_MODIFIED, pid::CONTENT, pid::DICTIONARY, pid::VERSION, pid::IDENTIFICATION_LIST, pid::OBJECT_MODEL_VERSION]
    {
        assert!(header.get(req).is_some(), "Header property {req:#06x}");
    }
    assert_eq!(header.auid(pid::OPERATIONAL_PATTERN), Some(def::OP_EDIT_PROTOCOL));
    let ident = &header.objs(pid::IDENTIFICATION_LIST)[0];
    for req in [pid::COMPANY_NAME, pid::PRODUCT_NAME, pid::PRODUCT_VERSION_STRING, pid::PRODUCT_ID, pid::DATE, pid::GENERATION_AUID] {
        assert!(ident.get(req).is_some(), "Identification property {req:#06x}");
    }
    // the product is identified by this application's brand, not the donor's
    assert_eq!(ident.string(pid::PRODUCT_NAME).as_deref(), Some(soundcraft_brand::APP_NAME));
    assert_eq!(ident.string(pid::COMPANY_NAME).as_deref(), Some(soundcraft_brand::VENDOR));

    let dict = header.strong(pid::DICTIONARY).expect("dictionary");
    let ddefs = keys(dict, pid::DATA_DEFINITIONS);
    let opdefs = keys(dict, pid::OPERATION_DEFINITIONS);
    let idefs = keys(dict, pid::INTERPOLATION_DEFINITIONS);
    let cdefs = keys(dict, pid::CONTAINER_DEFINITIONS);
    let pdefs = keys(dict, pid::PARAMETER_DEFINITIONS);
    let content = header.strong(pid::CONTENT).expect("content");
    let mobs = content.objs(pid::MOBS);
    let mob_ids: Vec<[u8; 32]> = mobs.iter().map(|m| m.mob_id(pid::MOB_ID).expect("mob id")).collect();
    assert_eq!(mob_ids.iter().collect::<HashSet<_>>().len(), mob_ids.len(), "unique mob ids");
    assert!(matches!(content.get(pid::MOBS), Some(Value::StrongSet(_, k)) if *k == pid::MOB_ID));
    let comps: Vec<&Obj> = mobs.iter().filter(|m| m.class == cls::COMPOSITION_MOB).collect();
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0].auid(pid::USAGE_CODE), Some(def::USAGE_TOP_LEVEL));
    for m in mobs {
        for req in [pid::MOB_ID, pid::MOB_NAME, pid::SLOTS, pid::MOB_LAST_MODIFIED, pid::MOB_CREATION_TIME] {
            assert!(m.get(req).is_some(), "Mob property {req:#06x}");
        }
        if m.class == cls::SOURCE_MOB {
            let d = m.strong(pid::ESSENCE_DESCRIPTION).expect("SourceMob::EssenceDescription");
            if d.class != cls::TAPE_DESCRIPTOR {
                for req in [pid::SAMPLE_RATE, pid::FILE_LENGTH, pid::CONTAINER_FORMAT] {
                    assert!(d.get(req).is_some(), "FileDescriptor property {req:#06x}");
                }
                assert!(cdefs.contains(d.weak_key(pid::CONTAINER_FORMAT).expect("container")));
                if m.stream_of_essence(content).is_none() {
                    let url = d.objs(pid::LOCATOR)[0].string(pid::URL_STRING).expect("url");
                    // absolute paths are written as file URLs, a relative one (a session with no folder) as it is
                    assert!(url.starts_with("file:///") || !url.contains("://"), "{url}");
                }
            }
        }
        for slot in m.objs(pid::SLOTS) {
            assert!(slot.get(pid::SLOT_ID).is_some() && slot.get(pid::SEGMENT).is_some());
            if slot.class == cls::TIMELINE_MOB_SLOT {
                assert!(slot.rational(pid::EDIT_RATE).is_some_and(|r| r.0 > 0 && r.1 > 0));
                assert!(slot.get(pid::ORIGIN).is_some());
            }
        }
        let mut all = Vec::new();
        walk(m, &mut all);
        for o in all {
            let is_component =
                [cls::FILLER, cls::SOURCE_CLIP, cls::SEQUENCE, cls::TRANSITION, cls::OPERATION_GROUP, cls::TIMECODE, cls::COMMENT_MARKER]
                    .contains(&o.class);
            if is_component {
                let dd = o.weak_key(pid::DATA_DEFINITION).expect("Component::DataDefinition");
                assert!(ddefs.contains(dd), "data definition in the dictionary");
                let event_seq = o.class == cls::SEQUENCE && dd == ddef::DESCRIPTIVE_METADATA;
                assert!(event_seq || o.get(pid::LENGTH).is_some(), "Component::Length");
            }
            if o.class == cls::SOURCE_CLIP {
                let id = o.mob_id(pid::SOURCE_ID).expect("source id");
                let slot = o.u32(pid::SOURCE_MOB_SLOT_ID).expect("slot id");
                assert!(o.get(pid::START_TIME).is_some());
                if id != [0; 32] {
                    let t = mobs.iter().find(|m| m.mob_id(pid::MOB_ID) == Some(id)).expect("source clip target in the file");
                    assert!(t.objs(pid::SLOTS).iter().any(|s| s.u32(pid::SLOT_ID) == Some(slot)), "target slot exists");
                }
            }
            if o.class == cls::OPERATION_GROUP {
                assert!(opdefs.contains(o.weak_key(pid::OPERATION).expect("operation")));
                for prm in o.objs(pid::PARAMETERS) {
                    assert!(pdefs.contains(prm.auid(pid::PARAMETER_DEFINITION).expect("parameter").as_slice()));
                    if prm.class == cls::VARYING_VALUE {
                        assert!(idefs.contains(prm.weak_key(pid::INTERPOLATION).expect("interpolation")));
                        assert!(!prm.objs(pid::POINT_LIST).is_empty());
                    }
                }
            }
            if o.class == cls::SEQUENCE && o.get(pid::LENGTH).is_some() {
                let cs = o.objs(pid::COMPONENTS);
                let mut sum = 0i64;
                for (i, c) in cs.iter().enumerate() {
                    let l = c.i64(pid::LENGTH).expect("length");
                    if c.class == cls::TRANSITION {
                        // Edit Protocol: a transition sits between two segments at least as long
                        assert!(i > 0 && i + 1 < cs.len(), "transition at a sequence end");
                        assert!(cs[i - 1].class != cls::TRANSITION && cs[i + 1].class != cls::TRANSITION);
                        assert!(cs[i - 1].i64(pid::LENGTH).expect("length") >= l && cs[i + 1].i64(pid::LENGTH).expect("length") >= l);
                        let cut = c.i64(pid::CUT_POINT).expect("cut point");
                        assert!((0..=l).contains(&cut));
                        assert!(c.strong(pid::OPERATION_GROUP).is_some());
                        sum -= l;
                    } else {
                        sum += l;
                    }
                }
                assert_eq!(sum, o.i64(pid::LENGTH).expect("length"), "sequence length = segments - transitions");
            }
        }
    }
    // weak reference paths are registered
    let rp = cf.read_path("referenced properties").expect("referenced properties");
    let pids: Vec<u16> = rp[7..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    assert!(pids.windows(3).any(|w| w == path::DATA_DEFS));
    let owned = mobs.to_vec();
    (root, owned)
}

impl Obj {
    /// Whether the file mob `self` has its essence in the file (an `EssenceData` object names it).
    fn stream_of_essence(&self, content: &Obj) -> Option<()> {
        let id = self.mob_id(pid::MOB_ID)?;
        content.objs(pid::ESSENCE_DATA).iter().any(|e| e.mob_id(pid::ESSENCE_MOB_ID) == Some(id)).then_some(())
    }
}

#[test]
fn written_files_have_the_required_structure() {
    for small in [false, true] {
        let s = demo_like();
        let bytes = aaf(&s, small);
        let (_, mobs) = check_structure(&bytes, small);
        // one master and one file mob per audio source, and no tape mobs (no source timecode)
        assert_eq!(mobs.iter().filter(|m| m.class == cls::MASTER_MOB).count(), 5);
        assert_eq!(mobs.iter().filter(|m| m.class == cls::SOURCE_MOB).count(), 5);

        let comp = mobs.iter().find(|m| m.class == cls::COMPOSITION_MOB).expect("composition");
        let slots = comp.objs(pid::SLOTS);
        // the timecode slot: 25 fps, starting at 01:00:00:00
        let tc = slots[0].strong(pid::SEGMENT).expect("timecode");
        assert_eq!(tc.class, cls::TIMECODE);
        assert_eq!(tc.u16(pid::TC_FPS), Some(25));
        assert_eq!(tc.i64(pid::TC_START), Some(3600 * 25));
        // five audio slots at the sample rate, in track order, and the event slot of markers
        let names: Vec<String> = slots.iter().filter_map(|s| s.string(pid::SLOT_NAME)).collect();
        assert_eq!(names, ["TC1", "Kick", "Snare", "Hats", "Bass", "Pad", "Markers"]);
        assert_eq!(slots[1].rational(pid::EDIT_RATE), Some((i64::from(SR), 1)));
        assert_eq!(slots[6].class, cls::EVENT_MOB_SLOT);

        // the snare crossfade is a transition between its two clips; the segments keep their
        // lengths and the sequence is shorter by the overlap
        let snare = slots[2].strong(pid::SEGMENT).expect("snare").objs(pid::COMPONENTS);
        let classes: Vec<_> = snare.iter().map(|c| c.class).collect();
        assert_eq!(classes, [cls::SOURCE_CLIP, cls::TRANSITION, cls::SOURCE_CLIP]);
        assert_eq!(snare[0].i64(pid::LENGTH), Some(48_000));
        assert_eq!(snare[1].i64(pid::LENGTH), Some(8_000));
        assert_eq!(snare[2].i64(pid::LENGTH), Some(48_000));
        assert_eq!(snare[1].strong(pid::OPERATION_GROUP).expect("op").weak_key(pid::OPERATION), Some(&def::MONO_AUDIO_DISSOLVE[..]));

        // the bass: a constant gain around the clip, and fades as transitions
        let bass = slots[4].strong(pid::SEGMENT).expect("bass").objs(pid::COMPONENTS);
        let gain = bass.iter().find(|c| c.class == cls::OPERATION_GROUP).expect("gain group");
        assert_eq!(gain.weak_key(pid::OPERATION), Some(&def::MONO_AUDIO_GAIN[..]));
        let amp = gain.objs(pid::PARAMETERS)[0].data(pid::CONSTANT_VALUE).expect("amplitude");
        let (n, d) = store::rational_of(&amp[17..]).expect("rational");
        assert!((n as f64 / d as f64 - 10f64.powf(-4.5 / 20.0)).abs() < 1e-4);
        assert_eq!(bass.iter().filter(|c| c.class == cls::TRANSITION).count(), 2);

        // the pad: its envelope is a varying value with a linear interpolation
        let pad = slots[5].strong(pid::SEGMENT).expect("pad").objs(pid::COMPONENTS);
        let g = pad.iter().find(|c| c.class == cls::OPERATION_GROUP).expect("pad gain");
        let v = &g.objs(pid::PARAMETERS)[0];
        assert_eq!(v.class, cls::VARYING_VALUE);
        assert!(v.objs(pid::POINT_LIST).len() >= 3);

        // the markers are comment markers
        let events = slots[6].strong(pid::SEGMENT).expect("events").objs(pid::COMPONENTS);
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|e| e.class == cls::COMMENT_MARKER));
    }
}

#[test]
fn embedded_and_video_files_have_the_required_structure() {
    let s = video_session();
    for (media, small) in [(crate::MediaMode::Link, false), (crate::MediaMode::Embed, true)] {
        let bytes = export(&s, Format::Aaf, &ExportOptions { media, small_sectors: small, ..Default::default() }).expect("export").0;
        let (root, mobs) = check_structure(&bytes, small);
        // the picture source has a CDCI descriptor and a picture slot
        let content = root.strong(pid::ROOT_HEADER).and_then(|h| h.strong(pid::CONTENT)).expect("content");
        let picture = mobs
            .iter()
            .filter(|m| m.class == cls::SOURCE_MOB)
            .filter_map(|m| m.strong(pid::ESSENCE_DESCRIPTION))
            .find(|d| d.class == cls::CDCI_DESCRIPTOR)
            .expect("picture descriptor");
        assert_eq!(picture.u32(pid::STORED_WIDTH), Some(1280));
        assert_eq!(picture.u32(pid::STORED_HEIGHT), Some(720));
        let essence = content.objs(pid::ESSENCE_DATA).len();
        assert_eq!(essence, if media == crate::MediaMode::Embed { 5 } else { 0 }, "embedded audio is EssenceData, linked is not");
        if media == crate::MediaMode::Embed {
            for e in content.objs(pid::ESSENCE_DATA) {
                // raw interleaved PCM, as long as its file mob's descriptor says
                let pcm = e.stream(pid::ESSENCE_STREAM).expect("stream");
                let id = e.mob_id(pid::ESSENCE_MOB_ID).expect("mob id");
                let mob = mobs.iter().find(|m| m.mob_id(pid::MOB_ID) == Some(id)).expect("its file mob");
                let d = mob.strong(pid::ESSENCE_DESCRIPTION).expect("descriptor");
                assert_eq!(d.class, cls::PCM_DESCRIPTOR);
                let frames = usize::try_from(d.i64(pid::FILE_LENGTH).expect("length")).expect("positive");
                let bytes = usize::try_from(d.u32(pid::CHANNELS).expect("channels")).expect("small")
                    * (d.u32(pid::QUANTIZATION_BITS).expect("bits") as usize / 8);
                assert_eq!(pcm.len(), frames * bytes);
            }
        }
    }
}

#[test]
fn store_round_trips_every_stored_form() {
    let leaf = |n: u8| Obj::new([n; 16]).data_prop(0x1B01, vec![n; 16]);
    let root = Obj::new(ids::ROOT)
        .data_prop(0x0003, vec![1, 2, 3])
        .with(0x0004, Value::Strong(Box::new(leaf(1).with(0x0005, Value::StrongVec(vec![leaf(2), leaf(3)])))))
        .with(
            0x0002,
            Value::Strong(Box::new(
                Obj::new([9; 16])
                    .with(0x3B04, Value::Strong(Box::new(Obj::new([8; 16]).with(0x2605, Value::StrongSet(vec![leaf(4), leaf(5)], 0x1B01))))),
            )),
        )
        .with(0x0006, Value::Weak(path::DATA_DEFS.to_vec(), 0x1B01, vec![5; 16]))
        .with(0x0007, Value::WeakVec(path::DATA_DEFS.to_vec(), 0x1B01, vec![vec![4; 16], vec![5; 16]]))
        .with(0x0008, Value::Stream((0..10_000u32).map(|i| i as u8).collect()));
    for v in [soundcraft_cfb::Version::V3, soundcraft_cfb::Version::V4] {
        let bytes = store::write(&root, v).expect("write");
        assert_eq!(store::read(&bytes).expect("read"), root);
    }
    // a data value longer than a property can hold is an error, not a panic
    assert!(store::write(&Obj::new(ids::ROOT).data_prop(1, vec![0; 70_000]), soundcraft_cfb::Version::V4).is_err());
    // so is a weak key longer than its one-byte size field
    assert!(store::write(&Obj::new(ids::ROOT).with(1, Value::Weak(vec![], 1, vec![0; 300])), soundcraft_cfb::Version::V4).is_err());
}

/// The property `p` of `o` as a list of objects, to change.
fn objs_mut(o: &mut Obj, p: u16) -> &mut Vec<Obj> {
    match o.props.iter_mut().find(|(k, _)| *k == p).map(|(_, v)| v) {
        Some(Value::StrongVec(v)) | Some(Value::StrongSet(v, _)) => v,
        _ => panic!("no object list {p:#06x}"),
    }
}

fn strong_mut(o: &mut Obj, p: u16) -> &mut Obj {
    match o.props.iter_mut().find(|(k, _)| *k == p).map(|(_, v)| v) {
        Some(Value::Strong(v)) => v,
        _ => panic!("no object {p:#06x}"),
    }
}

fn is_picture(o: &Obj) -> bool {
    o.weak_key(pid::DATA_DEFINITION) == Some(&ddef::PICTURE[..])
}

/// How Premiere Pro lays a sequence with several video tracks out in AAF (seen in a file it
/// exported): all video tracks of a composition are in ONE slot, a nested scope with a segment per
/// track, lowest first. Our own file is rewritten into that layout and must read back as the same
/// edit.
#[test]
fn premiere_style_layered_video_is_read() {
    let mut s = video_session();
    // two more video tracks
    for (name, start) in [("Layer 2", 2 * 48_000), ("Layer 3", 3 * 48_000)] {
        let id = SourceId(s.alloc());
        s.videos.push(VideoSource {
            id,
            name: name.into(),
            path: format!("/media/{name}.mov"),
            width: 1280,
            height: 720,
            frame_rate: 25.0,
            duration: 6.0,
            codec: "h264".into(),
            offline: None,
        });
        let t = s.add_track(TrackKind::Video, soundcraft_model::ChannelFormat::Mono, Some(name));
        let cid = ClipId(s.alloc());
        if let Some(p) = s.track_mut(t).and_then(|t| t.playlist_mut()) {
            p.clips.push(Clip::video(cid, name, id, 0, start, 2 * 48_000));
        }
    }
    let (ours, report) = export(&s, Format::Aaf, &ExportOptions::default()).expect("export");
    assert!(!report.mentions("video"), "{report}");

    let mut root = store::read(&ours).expect("read");
    let content = strong_mut(strong_mut(&mut root, pid::ROOT_HEADER), pid::CONTENT);
    for mob in objs_mut(content, pid::MOBS).iter_mut().filter(|m| m.class == cls::COMPOSITION_MOB) {
        let slots = objs_mut(mob, pid::SLOTS);
        let picture: Vec<usize> = slots.iter().enumerate().filter(|(_, s)| s.strong(pid::SEGMENT).is_some_and(is_picture)).map(|(i, _)| i).collect();
        assert_eq!(picture.len(), 3);
        let layers: Vec<Obj> = picture.iter().map(|&i| slots[i].strong(pid::SEGMENT).expect("segment").clone()).collect();
        let length = layers.iter().filter_map(|l| l.i64(pid::LENGTH)).max().unwrap_or(0);
        let scope = Obj::new(cls::NESTED_SCOPE)
            .with(pid::DATA_DEFINITION, Value::Weak(path::DATA_DEFS.to_vec(), pid::IDENTIFICATION, ddef::PICTURE.to_vec()))
            .data_prop(pid::LENGTH, length.to_le_bytes().to_vec())
            .with(pid::NESTED_SLOTS, Value::StrongVec(layers));
        slots[picture[0]].set(pid::SEGMENT, Value::Strong(Box::new(scope)));
        for &i in picture[1..].iter().rev() {
            slots.remove(i);
        }
    }
    let theirs = store::write(&root, soundcraft_cfb::Version::V4).expect("write");
    assert_ne!(theirs, ours);

    let opts = ImportOptions { sample_rate: Some(SR), ..Default::default() };
    let (want, _) = import(&ours, Format::Aaf, &opts).expect("import ours");
    let (got, report) = import(&theirs, Format::Aaf, &opts).expect("import theirs");
    assert!(!report.has_warnings() || !report.mentions("layer"), "{report}");
    let shape = |s: &Session| {
        s.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .map(|t| t.clips().iter().map(|c| (c.start, c.length, c.source_offset())).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&got.session), shape(&want.session));
    assert_eq!(shape(&got.session).len(), 3, "three video tracks");
    assert!(shape(&got.session).iter().all(|t| t.len() == 1));
}

/// `f` on `o` and on every object under it.
fn each_mut(o: &mut Obj, f: &mut dyn FnMut(&mut Obj)) {
    f(o);
    for (_, v) in o.props.iter_mut() {
        match v {
            Value::Strong(c) => each_mut(c, f),
            Value::StrongVec(cs) | Value::StrongSet(cs, _) => cs.iter_mut().for_each(|c| each_mut(c, f)),
            _ => {}
        }
    }
}

/// A mob id no exported file uses.
fn fresh_id(n: usize) -> Vec<u8> {
    let mut id = vec![0u8; 32];
    id[0] = 0xEE;
    id[1] = (n >> 8) as u8;
    id[2] = n as u8;
    id
}

/// `n` compositions: the first is the exported one and each of the others is a copy of it under a
/// new id; `target(i)` is the composition the clips of composition `i` point at (all of them, in
/// place of the media they used).
fn compositions_pointing(n: usize, target: impl Fn(usize) -> usize) -> Vec<u8> {
    let (bytes, _) = export(&demo_like(), Format::Aaf, &ExportOptions::default()).expect("export");
    let mut root = store::read(&bytes).expect("read");
    let content = strong_mut(strong_mut(&mut root, pid::ROOT_HEADER), pid::CONTENT);
    let mobs = objs_mut(content, pid::MOBS);
    let original = mobs.iter().find(|m| m.class == cls::COMPOSITION_MOB).expect("a composition").clone();
    let ids: Vec<Vec<u8>> = (0..n).map(|i| if i == 0 { original.data(pid::MOB_ID).expect("id").to_vec() } else { fresh_id(i) }).collect();
    mobs.retain(|m| m.class != cls::COMPOSITION_MOB);
    for (i, id) in ids.iter().enumerate() {
        let mut comp = original.clone();
        comp.set(pid::MOB_ID, Value::Data(id.clone()));
        let to = ids[target(i)].clone();
        each_mut(&mut comp, &mut |o| {
            if o.class == cls::SOURCE_CLIP {
                o.set(pid::SOURCE_ID, Value::Data(to.clone()));
            }
        });
        mobs.push(comp);
    }
    store::write(&root, soundcraft_cfb::Version::V4).expect("write")
}

/// A composition whose clips name the composition itself: the clips become gaps, with a warning.
#[test]
fn a_composition_that_uses_itself_is_imported_with_a_warning() {
    let bytes = compositions_pointing(1, |_| 0);
    let (imp, report) = import(&bytes, Format::Aaf, &ImportOptions::default()).expect("import");
    assert!(report.mentions("nested in itself"), "{report}");
    assert!(imp.session.tracks.iter().all(|t| t.clips().is_empty()), "no clip can come from a composition that is its own source");
}

/// Two compositions that use each other.
#[test]
fn compositions_that_use_each_other_are_imported_with_a_warning() {
    let bytes = compositions_pointing(2, |i| 1 - i);
    let (_, report) = import(&bytes, Format::Aaf, &ImportOptions::default()).expect("import");
    assert!(report.mentions("nested in itself"), "{report}");
}

/// A chain of forty compositions, each with dozens of clips that all use the next: a reader that
/// read the next composition once per clip would do more work than there are atoms. The nesting
/// limit ends it, and each composition is read once.
#[test]
fn a_long_chain_of_compositions_is_cut_off_and_read_once_each() {
    let bytes = compositions_pointing(40, |i| (i + 1) % 40);
    let start = std::time::Instant::now();
    let (_, report) = import(&bytes, Format::Aaf, &ImportOptions::default()).expect("import");
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
    assert!(report.mentions("nested") && report.has_warnings(), "{report}");
}
