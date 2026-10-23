//! Structural checks of written OMF files (the Bento label and TOC, and the required properties of
//! the OMF Interchange 2.0 classes that are written), and hostile object graphs.
//!
//! (Ported from the donor's tests; the fixtures are Pa Tools sessions.)

use std::collections::HashMap;

use super::bento::{self, Container, Seg};
use crate::support::{SR, demo_like};
use crate::{ExportOptions, Format, ImportOptions, MediaMode, export, import};

fn omf(media: MediaMode) -> Vec<u8> {
    export(&demo_like(), Format::Omf, &ExportOptions { media, ..Default::default() }).expect("export").0
}

#[test]
fn written_files_have_the_required_structure() {
    let bytes = omf(MediaMode::Embed);
    // label
    let l = &bytes[bytes.len() - bento::LABEL_SIZE..];
    assert_eq!(&l[..8], &bento::MAGIC);
    assert_eq!(u16::from_be_bytes([l[12], l[13]]), 1, "Bento major version");
    let c = Container::open(&bytes).expect("open");
    let class = |o: u32| c.get(o, "OMFI:OOBJ:ObjClass").map(|v| String::from_utf8_lossy(&v).to_string());
    let mut by_class: HashMap<String, Vec<u32>> = HashMap::new();
    for &o in c.objects.keys() {
        if let Some(k) = class(o) {
            by_class.entry(k).or_default().push(o);
        }
    }
    let required: &[(&str, &[&str])] = &[
        (
            "HEAD",
            &[
                "OMFI:HEAD:ByteOrder",
                "OMFI:HEAD:LastModified",
                "OMFI:HEAD:Version",
                "OMFI:HEAD:Mobs",
                "OMFI:HEAD:MediaData",
                "OMFI:HEAD:DefinitionObjects",
            ],
        ),
        ("CMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:MOBJ:LastModified", "OMFI:MOBJ:CreationTime"]),
        ("MMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:MOBJ:LastModified", "OMFI:MOBJ:CreationTime"]),
        ("SMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:SMOB:MediaDescription"]),
        ("MSLT", &["OMFI:MSLT:Segment", "OMFI:MSLT:EditRate", "OMFI:MSLT:TrackDesc"]),
        ("TRKD", &["OMFI:TRKD:Origin", "OMFI:TRKD:TrackID"]),
        ("SEQU", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:SEQU:Components"]),
        ("SCLP", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:SCLP:SourceID", "OMFI:SCLP:SourceTrackID", "OMFI:SCLP:StartTime"]),
        ("FILL", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length"]),
        ("TRAN", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:TRAN:CutPoint", "OMFI:TRAN:Effect"]),
        ("EFFE", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:EFFE:EffectKind"]),
        ("ESLT", &["OMFI:ESLT:ArgID", "OMFI:ESLT:ArgValue"]),
        ("CVAL", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:CVAL:Value"]),
        ("VVAL", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:VVAL:Interpolation", "OMFI:VVAL:PointList"]),
        ("CTLP", &["OMFI:CTLP:Time", "OMFI:CTLP:Value"]),
        ("EDEF", &["OMFI:EDEF:EffectID"]),
        ("DDEF", &["OMFI:DDEF:DataKindID"]),
        ("TCCP", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:TCCP:Start", "OMFI:TCCP:FPS", "OMFI:TCCP:Drop"]),
        ("WAVD", &["OMFI:MDFL:IsOMFI", "OMFI:MDFL:SampleRate", "OMFI:MDFL:Length", "OMFI:WAVD:Summary"]),
        ("WAVE", &["OMFI:MDAT:MobID", "OMFI:WAVE:Data"]),
        ("IDNT", &["OMFI:IDNT:CompanyName", "OMFI:IDNT:ProductName"]),
    ];
    for (k, props) in required {
        let Some(objs) = by_class.get(*k) else {
            assert_eq!(*k, "FILL", "no {k} object");
            continue;
        };
        for &o in objs {
            for prop in *props {
                assert!(c.get(o, prop).is_some(), "{k} {o} lacks {prop}");
            }
        }
    }
    assert_eq!(by_class["HEAD"].len(), 1);
    assert_eq!(by_class["CMOB"].len(), 1);
    assert_eq!(by_class["MMOB"].len(), 5, "one master mob per source");
    let head = by_class["HEAD"][0];
    assert_eq!(c.get(head, "OMFI:HEAD:Version"), Some(vec![2, 0]));
    assert_eq!(c.get(head, "OMFI:HEAD:ByteOrder"), Some(b"MM".to_vec()));
    // the product is identified by this application's brand, not the donor's
    let ident = by_class["IDNT"][0];
    assert_eq!(c.get(ident, "OMFI:IDNT:ProductName"), Some([soundcraft_brand::APP_NAME.as_bytes(), &[0]].concat()));
    // object references resolve to objects of the right class
    let refs = |o: u32, p: &str| -> Vec<u32> {
        let v = c.get(o, p).expect("list");
        let n = u16::from_be_bytes([v[0], v[1]]) as usize;
        v[2..].chunks_exact(4).take(n).map(|k| c.resolve(o, u32::from_be_bytes([k[0], k[1], k[2], k[3]]))).collect()
    };
    let one = |o: u32, p: &str| -> u32 { c.resolve(o, u32::from_be_bytes(c.get(o, p).expect("ref")[..4].try_into().expect("4 bytes"))) };
    for m in refs(head, "OMFI:HEAD:Mobs") {
        assert!(matches!(class(m).as_deref(), Some("CMOB" | "MMOB" | "SMOB")));
        for s in refs(m, "OMFI:MOBJ:Slots") {
            assert_eq!(class(s).as_deref(), Some("MSLT"));
            assert_eq!(class(one(s, "OMFI:MSLT:TrackDesc")).as_deref(), Some("TRKD"));
        }
    }
    // embedded media: WAVE data is a WAVE file of the file mob it names
    assert_eq!(by_class["WAVE"].len(), 5);
    for w in &by_class["WAVE"] {
        let data = c.get(*w, "OMFI:WAVE:Data").expect("data");
        let (pcm, ch, sr, bits) = crate::wav::parse_wav(&data).expect("a WAVE file");
        assert_eq!((sr, bits), (SR, 24));
        assert!(ch == 1 || ch == 2);
        assert!(!pcm.is_empty());
        let id = c.get(*w, "OMFI:MDAT:MobID").expect("mob id");
        assert!(by_class["SMOB"].iter().any(|s| c.get(*s, "OMFI:MOBJ:MobID") == Some(id.clone())));
    }
    // the composition: a timecode slot, then a sound slot per track; the snare is clip, transition,
    // clip, and the bass has its gain effect around its clip
    let cm = by_class["CMOB"][0];
    let slots = refs(cm, "OMFI:MOBJ:Slots");
    assert_eq!(slots.len(), 6);
    assert_eq!(class(one(slots[0], "OMFI:MSLT:Segment")).as_deref(), Some("TCCP"));
    let len = |o: u32| i32::from_be_bytes(c.get(o, "OMFI:CPNT:Length").expect("length")[..4].try_into().expect("4 bytes")) as i64;
    let snare = refs(one(slots[2], "OMFI:MSLT:Segment"), "OMFI:SEQU:Components");
    let classes: Vec<String> = snare.iter().map(|&o| class(o).expect("class")).collect();
    assert_eq!(classes, ["SCLP", "TRAN", "SCLP"]);
    assert_eq!((len(snare[0]), len(snare[1]), len(snare[2])), (48_000, 8_000, 48_000));
    let bass = refs(one(slots[4], "OMFI:MSLT:Segment"), "OMFI:SEQU:Components");
    let effe = bass.iter().copied().find(|&o| class(o).as_deref() == Some("EFFE")).expect("gain effect");
    assert_eq!(c.get(one(effe, "OMFI:EFFE:EffectKind"), "OMFI:EDEF:EffectID"), Some(b"omfi:effect:MonoAudioGain\0".to_vec()));
}

#[test]
fn bento_values_round_trip() {
    let mut w = bento::Writer::new();
    let a = w.new_object();
    let b = w.new_object();
    w.set(a, "Test:Small", "test:Bytes", &[1, 2]);
    w.set(a, "Test:Empty", "test:Bytes", &[]);
    w.set(b, "Test:Big", "test:Bytes", &vec![9; 100_000]);
    w.set_references(a, &[b]);
    let bytes = w.finish().expect("finish");
    let c = Container::open(&bytes).expect("open");
    assert_eq!(c.get(a, "Test:Small"), Some(vec![1, 2]));
    assert_eq!(c.get(a, "Test:Empty"), Some(vec![]));
    assert_eq!(c.get(b, "Test:Big").map(|v| v.len()), Some(100_000));
    assert_eq!(c.resolve(a, b), b);
    assert_eq!(c.name_of(c.id_of("Test:Big").expect("id")), Some("Test:Big"));
    assert!(Container::open(&bytes[..bytes.len() - 1]).is_err());
}

// ---------------------------------------------------------------------------------------------
// Hostile files
// ---------------------------------------------------------------------------------------------

/// A Bento file around `heap` with `toc` as its table of contents.
fn raw_file(heap: &[u8], toc: &[u8], buf_kib: u16) -> Vec<u8> {
    let mut v = heap.to_vec();
    let toc_at = v.len() as u32;
    v.extend_from_slice(toc);
    v.extend_from_slice(&bento::MAGIC);
    v.extend_from_slice(&0u16.to_be_bytes());
    v.extend_from_slice(&buf_kib.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes());
    v.extend_from_slice(&toc_at.to_be_bytes());
    v.extend_from_slice(&(toc.len() as u32).to_be_bytes());
    v
}

#[test]
fn a_toc_outside_the_file_is_an_error() {
    let mut bytes = omf(MediaMode::Link);
    let n = bytes.len();
    // offset past the end, length past the end, both huge, offset + length overflowing
    for (off, len) in [(n as u32, 8), (0, n as u32 * 2), (u32::MAX, 24), (u32::MAX, u32::MAX), (u32::MAX - 4, 100)] {
        bytes[n - 8..n - 4].copy_from_slice(&off.to_be_bytes());
        bytes[n - 4..].copy_from_slice(&len.to_be_bytes());
        assert!(Container::open(&bytes).is_err(), "{off} {len}");
        assert!(import(&bytes, Format::Omf, &ImportOptions::default()).is_err(), "{off} {len}");
    }
}

#[test]
fn damaged_tocs_are_errors() {
    // an unknown operation; operands cut short; a value before any type
    for toc in [vec![0x77], vec![1, 0, 0], vec![5, 0, 0, 0, 1, 0, 0, 0], vec![13, 1, 2, 3, 4]] {
        assert!(Container::open(&raw_file(&[0; 64], &toc, 8)).is_err(), "{toc:?}");
    }
    // end-of-buffer in every position, with every buffer size that could overflow: no panic
    for buf in [0u16, 1, 8, u16::MAX] {
        for pad in 0..40 {
            let mut toc = vec![0xFF; pad];
            toc.push(24);
            toc.extend_from_slice(&[0xFF; 8]);
            let _ = Container::open(&raw_file(&[], &toc, buf));
        }
    }
}

/// A value of 100 000 segments that each cover the whole file would be 100 000 copies of it.
#[test]
fn segments_that_overlap_cannot_multiply_the_file() {
    let mut w = bento::Writer::new();
    let o = w.new_object();
    let heap = vec![7u8; 4096];
    w.set(o, "Test:Heap", "test:Bytes", &heap);
    let segs: Vec<Seg> = (0..100_000).map(|_| Seg::At(0, 4096)).collect();
    w.set_segments(o, "Test:Many", "test:Bytes", segs);
    let bytes = w.finish().expect("finish");
    let c = Container::open(&bytes).expect("open");
    assert!(!c.exhausted());
    // the value would be 400 MB; the budget is a small multiple of the file
    assert_eq!(c.get(o, "Test:Many"), None);
    assert!(c.exhausted());
    // a small value is still read after the budget is gone? no: once it is spent every read says so
    assert!(c.exhausted());
}

#[test]
fn a_value_that_runs_past_the_file_is_not_read() {
    let mut w = bento::Writer::new();
    let o = w.new_object();
    w.set(o, "Test:Small", "test:Bytes", &[1; 100]);
    let at = w.heap_len();
    w.set_segments(o, "Test:Past", "test:Bytes", vec![Seg::At(at + 10_000_000, 4)]);
    w.set_segments(o, "Test:Huge", "test:Bytes", vec![Seg::At(0, u32::MAX)]);
    w.set_segments(o, "Test:Wrap", "test:Bytes", vec![Seg::At(u64::MAX - 2, 8)]);
    let bytes = w.finish().expect("finish");
    let c = Container::open(&bytes).expect("open");
    for name in ["Test:Past", "Test:Huge", "Test:Wrap"] {
        assert_eq!(c.get(o, name), None, "{name}");
    }
    assert!(!c.exhausted(), "a value that is not whole is not a read");
    assert_eq!(c.get(o, "Test:Small").map(|v| v.len()), Some(100));
}

#[test]
fn a_reference_list_is_resolved_in_constant_time() {
    // 20 000 references in one list, each resolved: with a scan of the list per key this is 400M steps
    let mut w = bento::Writer::new();
    let o = w.new_object();
    let targets: Vec<u32> = (0..20_000).map(|_| w.new_object()).collect();
    w.set_references(o, &targets);
    let bytes = w.finish().expect("finish");
    let c = Container::open(&bytes).expect("open");
    let start = std::time::Instant::now();
    for &t in &targets {
        assert_eq!(c.resolve(o, t), t);
    }
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

/// Build an OMF file around a hostile graph of sequences: `sequences[i]` lists the sequences that
/// sequence `i` holds (indices into the list). The composition's track is sequence 0, or, with
/// `via_master`, a source clip of a master mob whose one slot is sequence 0.
fn graph_file(sequences: &[&[usize]], via_master: bool) -> Vec<u8> {
    let mut w = bento::Writer::new();
    let class = |w: &mut bento::Writer, o: u32, c: &[u8; 4]| w.set(o, "OMFI:OOBJ:ObjClass", "omfi:ClassID", c);
    let refs = |w: &mut bento::Writer, o: u32, p: &str, t: &[u32]| {
        let mut v = (t.len() as u16).to_be_bytes().to_vec();
        for x in t {
            v.extend_from_slice(&x.to_be_bytes());
        }
        w.set(o, p, "omfi:ObjRefArray", &v);
    };
    let ddef = w.new_object();
    class(&mut w, ddef, b"DDEF");
    w.set(ddef, "OMFI:DDEF:DataKindID", "omfi:UniqueName", b"omfi:data:Sound\0");
    let objs: Vec<u32> = sequences.iter().map(|_| w.new_object()).collect();
    for (o, comps) in objs.iter().zip(sequences) {
        class(&mut w, *o, b"SEQU");
        w.set(*o, "OMFI:CPNT:DataKind", "omfi:ObjRef", &ddef.to_be_bytes());
        w.set(*o, "OMFI:CPNT:Length", "omfi:Length32", &48_000i32.to_be_bytes());
        let t: Vec<u32> = comps.iter().filter_map(|&c| objs.get(c).copied()).collect();
        refs(&mut w, *o, "OMFI:SEQU:Components", &t);
    }
    let slot_to = |w: &mut bento::Writer, track: u32, segment: u32| {
        let trkd = w.new_object();
        class(w, trkd, b"TRKD");
        w.set(trkd, "OMFI:TRKD:TrackID", "omfi:UInt32", &track.to_be_bytes());
        let slot = w.new_object();
        class(w, slot, b"MSLT");
        w.set(slot, "OMFI:MSLT:EditRate", "omfi:Rational", &[0, 0, 0xBB, 0x80, 0, 0, 0, 1]);
        w.set(slot, "OMFI:MSLT:Segment", "omfi:ObjRef", &segment.to_be_bytes());
        w.set(slot, "OMFI:MSLT:TrackDesc", "omfi:ObjRef", &trkd.to_be_bytes());
        (slot, trkd)
    };
    let mut everything = vec![ddef];
    everything.extend(&objs);
    let mut mobs = Vec::new();
    let comp_segment = if via_master {
        let (mslot, mtrkd) = slot_to(&mut w, 1, objs[0]);
        let mmob = w.new_object();
        class(&mut w, mmob, b"MMOB");
        w.set(mmob, "OMFI:MOBJ:MobID", "omfi:UID", &[2; 12]);
        refs(&mut w, mmob, "OMFI:MOBJ:Slots", &[mslot]);
        let sclp = w.new_object();
        class(&mut w, sclp, b"SCLP");
        w.set(sclp, "OMFI:CPNT:DataKind", "omfi:ObjRef", &ddef.to_be_bytes());
        w.set(sclp, "OMFI:CPNT:Length", "omfi:Length32", &48_000i32.to_be_bytes());
        w.set(sclp, "OMFI:SCLP:SourceID", "omfi:UID", &[2; 12]);
        w.set(sclp, "OMFI:SCLP:SourceTrackID", "omfi:UInt32", &1u32.to_be_bytes());
        w.set(sclp, "OMFI:SCLP:StartTime", "omfi:Position32", &0i32.to_be_bytes());
        everything.extend([mslot, mtrkd, mmob, sclp]);
        mobs.push(mmob);
        sclp
    } else {
        objs[0]
    };
    let (slot, trkd) = slot_to(&mut w, 2, comp_segment);
    let cmob = w.new_object();
    class(&mut w, cmob, b"CMOB");
    w.set(cmob, "OMFI:MOBJ:MobID", "omfi:UID", &[1; 12]);
    refs(&mut w, cmob, "OMFI:MOBJ:Slots", &[slot]);
    mobs.insert(0, cmob);
    let head = w.new_object();
    class(&mut w, head, b"HEAD");
    w.set(head, "OMFI:HEAD:ByteOrder", "omfi:Int16", b"MM");
    refs(&mut w, head, "OMFI:HEAD:Mobs", &mobs);
    everything.extend([slot, trkd, cmob, head]);
    // every object can name every other (the keys are the object ids)
    for o in everything.clone() {
        w.set_references(o, &everything);
    }
    w.finish().expect("finish")
}

fn read_graph(sequences: &[&[usize]], via_master: bool) -> (crate::Result<(crate::Imported, crate::Report)>, std::time::Duration) {
    let bytes = graph_file(sequences, via_master);
    let start = std::time::Instant::now();
    let r = import(&bytes, Format::Omf, &ImportOptions::default());
    (r, start.elapsed())
}

/// A sequence that contains itself, two that contain each other, and a sequence 40 levels deep in
/// which every level holds the next one twice (2^40 reads if each use were followed).
#[test]
fn cyclic_and_shared_object_graphs_do_not_hang() {
    let (r, _) = read_graph(&[&[0]], false);
    let (_, report) = r.expect("a sequence inside itself is a gap");
    assert!(report.mentions("used twice"), "{report}");
    let (r, _) = read_graph(&[&[1], &[0]], false);
    assert!(r.expect("a ring is a gap").1.mentions("used twice"));
    // diamonds, 40 deep
    let mut seqs: Vec<Vec<usize>> = (0..40).map(|i| vec![i + 1, i + 1]).collect();
    seqs.push(vec![]);
    let refs: Vec<&[usize]> = seqs.iter().map(Vec::as_slice).collect();
    let (r, took) = read_graph(&refs, false);
    assert!(r.expect("a shared segment is a gap").1.mentions("used twice"));
    assert!(took.as_secs() < 5, "{took:?}");
}

/// The same diamonds under a master mob: finding the first source clip in them is a search that
/// would visit 2^40 paths; the work budget ends it, and the file is refused.
#[test]
fn a_search_through_a_shared_graph_runs_out_of_budget() {
    let mut seqs: Vec<Vec<usize>> = (0..40).map(|i| vec![i + 1, i + 1]).collect();
    seqs.push(vec![]);
    let refs: Vec<&[usize]> = seqs.iter().map(Vec::as_slice).collect();
    let (r, took) = read_graph(&refs, true);
    match r {
        Err(e) => assert!(e.to_string().contains("loop or a blow-up"), "{e}"),
        Ok(_) => panic!("the search should have been refused"),
    }
    assert!(took.as_secs() < 5, "{took:?}");
    // and a ring under a master mob
    let (r, took) = read_graph(&[&[1], &[0]], true);
    assert!(r.is_ok() || r.is_err());
    assert!(took.as_secs() < 5, "{took:?}");
}
