//! Tests build minimal CFB v3 containers with a small in-test writer (no external crate).
use soundcraft_interchange::{AAF_ROOT_CLSID, Cfb, Confidence, EntryKind, InterchangeError};

const FREE: u32 = 0xFFFF_FFFF;
const EOC: u32 = 0xFFFF_FFFE;
const FATSECT: u32 = 0xFFFF_FFFD;

enum Node {
    Storage(&'static str, Vec<Node>),
    Stream(&'static str, Vec<u8>),
}

struct Flat {
    name: String,
    ty: u8,
    clsid: [u8; 16],
    child: u32,
    right: u32,
    start: u32,
    size: u64,
    data: Vec<u8>,
}

fn flatten(nodes: &[Node], out: &mut Vec<Flat>) -> u32 {
    let ids: Vec<u32> = nodes
        .iter()
        .map(|n| {
            let id = out.len() as u32;
            match n {
                Node::Storage(name, _) => {
                    out.push(Flat { name: name.to_string(), ty: 1, clsid: [0; 16], child: FREE, right: FREE, start: 0, size: 0, data: vec![] })
                }
                Node::Stream(name, d) => out.push(Flat {
                    name: name.to_string(),
                    ty: 2,
                    clsid: [0; 16],
                    child: FREE,
                    right: FREE,
                    start: 0,
                    size: d.len() as u64,
                    data: d.clone(),
                }),
            }
            if let Node::Storage(_, kids) = n {
                let c = flatten(kids, out);
                out[id as usize].child = c;
            }
            id
        })
        .collect();
    for w in ids.windows(2) {
        out[w[0] as usize].right = w[1];
    }
    ids.first().copied().unwrap_or(FREE)
}

fn build(root_clsid: [u8; 16], nodes: Vec<Node>) -> Vec<u8> {
    let mut flat = vec![Flat { name: "Root Entry".into(), ty: 5, clsid: root_clsid, child: FREE, right: FREE, start: EOC, size: 0, data: vec![] }];
    let first = flatten(&nodes, &mut flat);
    flat[0].child = first;
    let mut fat: Vec<u32> = Vec::new();
    let mut sectors: Vec<Vec<u8>> = Vec::new();
    let alloc = |bytes: &[u8], fat: &mut Vec<u32>, sectors: &mut Vec<Vec<u8>>| -> u32 {
        let n = bytes.len().div_ceil(512).max(1);
        let start = sectors.len() as u32;
        for i in 0..n {
            let mut s = vec![0u8; 512];
            let chunk = &bytes[(i * 512).min(bytes.len())..((i + 1) * 512).min(bytes.len())];
            s[..chunk.len()].copy_from_slice(chunk);
            sectors.push(s);
            fat.push(if i + 1 == n { EOC } else { start + i as u32 + 1 });
        }
        start
    };
    // mini stream
    let mut mini = Vec::new();
    let mut minifat: Vec<u32> = Vec::new();
    for f in flat.iter_mut().filter(|f| f.ty == 2 && f.size < 4096) {
        if f.size == 0 {
            f.start = EOC;
            continue;
        }
        let n = f.data.len().div_ceil(64);
        f.start = minifat.len() as u32;
        for i in 0..n {
            minifat.push(if i + 1 == n { EOC } else { f.start + i as u32 + 1 });
        }
        let mut d = f.data.clone();
        d.resize(n * 64, 0);
        mini.extend(d);
    }
    let mut mini_start = EOC;
    if !mini.is_empty() {
        flat[0].size = mini.len() as u64;
        mini_start = alloc(&mini, &mut fat, &mut sectors);
        flat[0].start = mini_start;
    }
    let (mut minifat_start, mut nminifat) = (EOC, 0u32);
    if !minifat.is_empty() {
        let mut b: Vec<u8> = minifat.iter().flat_map(|v| v.to_le_bytes()).collect();
        b.resize(b.len().div_ceil(512) * 512, 0xFF);
        nminifat = (b.len() / 512) as u32;
        minifat_start = alloc(&b, &mut fat, &mut sectors);
    }
    for f in flat.iter_mut().filter(|f| f.ty == 2 && f.size >= 4096) {
        let d = f.data.clone();
        f.start = alloc(&d, &mut fat, &mut sectors);
    }
    let _ = mini_start;
    let mut dir = Vec::new();
    for f in &flat {
        let mut e = vec![0u8; 128];
        let units: Vec<u16> = f.name.encode_utf16().collect();
        for (i, u) in units.iter().enumerate() {
            e[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        e[64..66].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
        e[66] = f.ty;
        e[67] = 1;
        e[68..72].copy_from_slice(&FREE.to_le_bytes());
        e[72..76].copy_from_slice(&f.right.to_le_bytes());
        e[76..80].copy_from_slice(&f.child.to_le_bytes());
        e[80..96].copy_from_slice(&f.clsid);
        e[116..120].copy_from_slice(&f.start.to_le_bytes());
        e[120..128].copy_from_slice(&f.size.to_le_bytes());
        dir.extend(e);
    }
    while dir.len() % 512 != 0 {
        let mut e = vec![0u8; 128];
        e[68..80].copy_from_slice(&[0xFF; 12]);
        dir.extend(e);
    }
    let dir_start = alloc(&dir, &mut fat, &mut sectors);
    let data_sectors = sectors.len();
    let mut nfat = 1;
    while data_sectors + nfat > nfat * 128 {
        nfat += 1;
    }
    let fat_first = sectors.len();
    fat.extend(std::iter::repeat_n(FATSECT, nfat));
    fat.resize(nfat * 128, FREE);
    for i in 0..nfat {
        sectors.push(fat[i * 128..(i + 1) * 128].iter().flat_map(|v| v.to_le_bytes()).collect());
    }
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    h[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
    h[26..28].copy_from_slice(&3u16.to_le_bytes());
    h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
    h[30..32].copy_from_slice(&9u16.to_le_bytes());
    h[32..34].copy_from_slice(&6u16.to_le_bytes());
    h[44..48].copy_from_slice(&(nfat as u32).to_le_bytes());
    h[48..52].copy_from_slice(&dir_start.to_le_bytes());
    h[56..60].copy_from_slice(&4096u32.to_le_bytes());
    h[60..64].copy_from_slice(&minifat_start.to_le_bytes());
    h[64..68].copy_from_slice(&nminifat.to_le_bytes());
    h[68..72].copy_from_slice(&EOC.to_le_bytes());
    for i in 0..109 {
        let v = if i < nfat { (fat_first + i) as u32 } else { FREE };
        h[76 + 4 * i..80 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    let mut out = h;
    for s in sectors {
        out.extend(s);
    }
    out
}

fn aaf_bytes() -> Vec<u8> {
    // marker 'L', version 0x20, 2 index entries (key, form, len)
    let mut props = vec![0x4C, 0x20, 2, 0];
    for (k, f, l) in [(0x3B03u16, 0x0082u16, 8u16), (0x3B04, 0x0082, 4)] {
        props.extend(k.to_le_bytes());
        props.extend(f.to_le_bytes());
        props.extend(l.to_le_bytes());
    }
    build(
        AAF_ROOT_CLSID.0,
        vec![
            Node::Storage("Header-2", vec![Node::Stream("properties", props)]),
            Node::Storage("MetaDictionary-1", vec![Node::Stream("properties", vec![0x4C, 0x20, 0, 0])]),
            Node::Stream("referenced properties", vec![1, 2, 3]),
        ],
    )
}

#[test]
fn lists_storages_and_streams_with_sizes() {
    let big = vec![7u8; 10_000];
    let b = build(
        [0; 16],
        vec![Node::Storage("A", vec![Node::Stream("small", vec![1; 100]), Node::Stream("big", big.clone())]), Node::Stream("empty", vec![])],
    );
    let c = Cfb::from_bytes(b).unwrap();
    let e = c.find("A/small").unwrap();
    assert_eq!((e.kind, e.size), (EntryKind::Stream, 100));
    assert_eq!(c.find("A/big").unwrap().size, 10_000);
    assert_eq!(c.find("A").unwrap().kind, EntryKind::Storage);
    assert_eq!(c.find("empty").unwrap().size, 0);
    assert_eq!(c.read_stream("A/big").unwrap(), big);
    assert_eq!(c.read_stream("A/small").unwrap(), vec![1; 100]);
    assert!(c.read_stream("empty").unwrap().is_empty());
    assert!(matches!(c.read_stream("nope"), Err(InterchangeError::NoSuchStream(_))));
    assert!(matches!(c.read_stream("A"), Err(InterchangeError::NoSuchStream(_))));
    assert_eq!(c.entries().len(), 5);
    assert_eq!((c.major_version(), c.sector_size()), (3, 512));
}

#[test]
fn plain_cfb_is_not_aaf() {
    let c = Cfb::from_bytes(build([0; 16], vec![Node::Stream("x", vec![1])])).unwrap();
    let id = c.identify();
    assert!(!id.is_aaf());
    assert_eq!(id.confidence, Confidence::None);
    assert!(c.aaf_header().is_err());
}

#[test]
fn aaf_detected_strong_and_header_parsed() {
    let c = Cfb::from_bytes(aaf_bytes()).unwrap();
    let id = c.identify();
    assert_eq!(id.confidence, Confidence::Strong);
    assert!(id.root_clsid_matches && id.has_header_storage && id.has_meta_dictionary);
    assert_eq!(c.root_clsid().to_string(), "{B3B398A5-1C90-11D4-8053-080036210804}");
    let h = c.aaf_header().unwrap();
    assert!(h.little_endian);
    assert_eq!(h.version_marker, 0x20);
    assert_eq!(h.stream_path, "Header-2/properties");
    assert_eq!(h.index.len(), 2);
    assert_eq!((h.index[0].key, h.index[0].length), (0x3B03, 8));
}

#[test]
fn clsid_only_and_structure_only() {
    let c = Cfb::from_bytes(build(AAF_ROOT_CLSID.0, vec![Node::Stream("x", vec![1])])).unwrap();
    assert_eq!(c.identify().confidence, Confidence::ClassId);
    let c = Cfb::from_bytes(build([0; 16], vec![Node::Storage("Header-2", vec![]), Node::Storage("MetaDictionary-1", vec![])])).unwrap();
    assert_eq!(c.identify().confidence, Confidence::Structural);
    // a stream (not storage) or non-numeric suffix does not count
    let c = Cfb::from_bytes(build([0; 16], vec![Node::Stream("Header-2", vec![1]), Node::Storage("MetaDictionary-x", vec![])])).unwrap();
    assert_eq!(c.identify().confidence, Confidence::None);
}

#[test]
fn bad_header_streams_are_errors() {
    for props in [vec![], vec![0x4C, 0x20], vec![0x00, 0x20, 0, 0], vec![0x4C, 0x20, 0xFF, 0xFF]] {
        let b = build(
            AAF_ROOT_CLSID.0,
            vec![Node::Storage("Header-2", vec![Node::Stream("properties", props)]), Node::Storage("MetaDictionary-1", vec![])],
        );
        let c = Cfb::from_bytes(b).unwrap();
        assert!(c.aaf_header().is_err());
    }
    let b = build(AAF_ROOT_CLSID.0, vec![Node::Storage("Header-2", vec![]), Node::Storage("MetaDictionary-1", vec![])]);
    assert!(Cfb::from_bytes(b).unwrap().aaf_header().is_err());
}

#[test]
fn big_endian_marker() {
    let b =
        build(AAF_ROOT_CLSID.0, vec![Node::Storage("Header-2", vec![Node::Stream("properties", vec![0x42, 0x20, 0, 1, 0x3B, 0x03, 0, 0x82, 0, 8])])]);
    let h = Cfb::from_bytes(b).unwrap().aaf_header().unwrap();
    assert!(!h.little_endian);
    assert_eq!(h.index[0].key, 0x3B03);
}

#[test]
fn zero_length_garbage_and_truncated_fail() {
    assert!(Cfb::from_bytes(vec![]).is_err());
    assert!(Cfb::from_bytes(vec![0; 4096]).is_err());
    assert!(Cfb::from_bytes(b"not a compound file at all".to_vec()).is_err());
    assert!(Cfb::from_bytes(vec![0xFF; 100_000]).is_err());
    let good = aaf_bytes();
    assert!(Cfb::from_bytes(good[..8].to_vec()).is_err());
    assert!(Cfb::from_bytes(good[..511].to_vec()).is_err());
    for cut in [512, 513, 1024, good.len() / 2, good.len() - 1] {
        // must not panic; truncation anywhere is either an error or a still-consistent file
        let _ = Cfb::from_bytes(good[..cut].to_vec());
    }
    assert!(Cfb::from_bytes(good[..512].to_vec()).is_err());
    assert!(Cfb::open("/nonexistent/path/file.aaf").is_err());
}

#[test]
fn every_single_byte_corruption_never_panics() {
    let good = aaf_bytes();
    for i in (0..good.len()).step_by(7) {
        for v in [0x00u8, 0xFF, 0x7F] {
            let mut b = good.clone();
            b[i] = v;
            if let Ok(c) = Cfb::from_bytes(b) {
                let _ = c.identify();
                let _ = c.aaf_header();
                for e in c.entries().to_vec() {
                    let _ = c.read_stream(&e.path);
                }
            }
        }
    }
}

fn patch(b: &mut [u8], off: usize, v: &[u8]) {
    b[off..off + v.len()].copy_from_slice(v);
}

#[test]
fn hostile_header_fields() {
    let good = aaf_bytes();
    for (off, val) in [(44usize, 0xFFFF_FFF0u32), (64, 0xFFFF_FFF0), (72, 0xFFFF_FFF0), (48, 0x00FF_FFFF), (48, 0xFFFF_FFFA), (56, 0)] {
        let mut b = good.clone();
        patch(&mut b, off, &val.to_le_bytes());
        assert!(Cfb::from_bytes(b).is_err(), "off {off}");
    }
    let mut b = good.clone();
    patch(&mut b, 26, &9u16.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
    let mut b = good.clone();
    patch(&mut b, 28, &0xFEFFu16.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
    let mut b = good;
    patch(&mut b, 30, &12u16.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
}

fn dir_offset(b: &[u8]) -> usize {
    let s = u32::from_le_bytes([b[48], b[49], b[50], b[51]]) as usize;
    (s + 1) * 512
}

#[test]
fn huge_declared_stream_size_is_rejected() {
    let good = build([0; 16], vec![Node::Stream("s", vec![1; 50]), Node::Stream("t", vec![2; 9000])]);
    let d = dir_offset(&good);
    for entry in [1usize, 2] {
        for size in [u64::MAX, 0xFFFF_FFFF, (1u64 << 40) | 0xFFFF_FFF0] {
            let mut b = good.clone();
            patch(&mut b, d + entry * 128 + 120, &size.to_le_bytes());
            assert!(Cfb::from_bytes(b).is_err(), "entry {entry} size {size}");
        }
    }
    // declared slightly bigger than its chain but under the file size: read fails, no panic
    let mut b = good.clone();
    patch(&mut b, d + 2 * 128 + 120, &9100u64.to_le_bytes());
    if let Ok(c) = Cfb::from_bytes(b) {
        let _ = c.read_stream("t");
    }
}

#[test]
fn cyclic_chain_and_directory_cycle_are_rejected() {
    let good = build([0; 16], vec![Node::Stream("t", vec![2; 9000])]);
    let d = dir_offset(&good);
    // directory: root child -> itself
    let mut b = good.clone();
    patch(&mut b, d + 76, &0u32.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
    // sibling loop: entry 1 right -> entry 1
    let mut b = good.clone();
    patch(&mut b, d + 128 + 72, &1u32.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
    // out-of-range link
    let mut b = good.clone();
    patch(&mut b, d + 128 + 72, &999_999u32.to_le_bytes());
    assert!(Cfb::from_bytes(b).is_err());
    // FAT cycle on the stream's chain: stream starts at sector 0 -> make sector 0 point to itself
    let c = Cfb::from_bytes(good.clone()).unwrap();
    assert_eq!(c.read_stream("t").unwrap().len(), 9000);
    let fat_sector = u32::from_le_bytes([good[76], good[77], good[78], good[79]]) as usize;
    let mut b = good;
    patch(&mut b, (fat_sector + 1) * 512, &0u32.to_le_bytes());
    match Cfb::from_bytes(b) {
        Err(_) => {}
        Ok(c) => assert!(c.read_stream("t").is_err()),
    }
}

#[test]
fn open_from_path() {
    let p = std::env::temp_dir().join(format!("sc-interchange-{}.aaf", std::process::id()));
    std::fs::write(&p, aaf_bytes()).unwrap();
    let c = Cfb::open(&p).unwrap();
    assert!(c.identify().is_aaf());
    let _ = std::fs::remove_file(&p);
}
