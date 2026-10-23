//! Hostile object trees: a crafted file must give an error, never a crash, a hang or an
//! allocation sized by a count in the file (AGENTS.md section 0).

use soundcraft_cfb::{Version, Writer};

use super::store::{self, SF_STRONG, SF_STRONG_SET, SF_STRONG_VECTOR, SF_WEAK_SET, SF_WEAK_VECTOR, Value, utf16z};

const PID: u16 = 0x0007;

/// A `properties` stream with one entry { PID, stored form `sf`, value }.
fn props_one(sf: u16, value: &[u8]) -> Vec<u8> {
    // byte order 'L', format version, 1 entry { pid, stored form, value length }, the value
    let mut props = vec![0x4C, 0x20];
    for x in [1, PID, sf, value.len() as u16] {
        props.extend_from_slice(&x.to_le_bytes());
    }
    props.extend_from_slice(value);
    props
}

/// A root object whose one property `PID` is a collection (stored form `sf`) named `Keys`, with
/// `index` as its `Keys index` stream.
fn collection_file(sf: u16, index: Vec<u8>) -> Vec<u8> {
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", props_one(sf, &utf16z("Keys"))).unwrap();
    w.stream(Writer::ROOT, "Keys index", index).unwrap();
    w.finish().unwrap()
}

/// A weak collection index: u32 count, u16 tag, u16 key pid, u8 key size, then the keys.
fn index(count: u32, size: u8, keys: &[u8]) -> Vec<u8> {
    let mut v = count.to_le_bytes().to_vec();
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&0x1B01u16.to_le_bytes());
    v.push(size);
    v.extend_from_slice(keys);
    v
}

fn read_keys(sf: u16, index: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    match store::read(&collection_file(sf, index))?.get(PID) {
        Some(Value::WeakVec(_, _, keys)) => Ok(keys.clone()),
        other => panic!("not a weak collection: {other:?}"),
    }
}

/// With a key size of 0 every key fits, so the count alone drove the loop: up to 4.29 billion
/// empty keys (about 100 GB) from a 9-byte index. A million is enough to show it, cheaply.
#[test]
fn zero_size_weak_collection_keys_are_an_error() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        match read_keys(sf, index(1_000_000, 0, &[])) {
            Err(e) => assert!(e.contains("zero-size"), "{e}"),
            Ok(keys) => panic!("read {} empty keys instead of an error", keys.len()),
        }
    }
}

#[test]
fn weak_collection_count_beyond_the_index_is_an_error() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        let e = read_keys(sf, index(u32::MAX, 4, &[1, 2, 3, 4, 5, 6, 7, 8])).unwrap_err();
        assert!(e.contains("short weak collection index"), "{e}");
    }
}

#[test]
fn weak_collections_within_the_index_are_read() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        assert_eq!(read_keys(sf, index(0, 0, &[])).unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(read_keys(sf, index(2, 4, &[1, 2, 3, 4, 5, 6, 7, 8])).unwrap(), [vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }
}

/// A strong vector or set claiming 4 billion elements in a 12-byte index.
#[test]
fn strong_collection_counts_beyond_the_index_are_an_error() {
    for sf in [SF_STRONG_VECTOR, SF_STRONG_SET] {
        let mut idx = u32::MAX.to_le_bytes().to_vec();
        idx.extend_from_slice(&[0; 8]);
        // a set index also has its key pid and key size
        idx.extend_from_slice(&[1, 0, 16]);
        let e = store::read(&collection_file(sf, idx)).unwrap_err();
        assert!(e.contains("short"), "{e}");
    }
}

/// Two local keys that name the same child: the child would be read twice, so a chain of such
/// collections would multiply the work. It is refused at the second use.
#[test]
fn a_storage_named_twice_is_an_error() {
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", props_one(SF_STRONG_VECTOR, &utf16z("Items"))).unwrap();
    // two entries, both key 0
    let mut idx = 2u32.to_le_bytes().to_vec();
    idx.extend_from_slice(&[0; 8]);
    idx.extend_from_slice(&0u32.to_le_bytes());
    idx.extend_from_slice(&0u32.to_le_bytes());
    w.stream(Writer::ROOT, "Items index", idx).unwrap();
    let child = w.storage(Writer::ROOT, "Items{0}", [1; 16]).unwrap();
    w.stream(child, "properties", vec![0x4C, 0x20, 0, 0]).unwrap();
    let e = store::read(&w.finish().unwrap()).unwrap_err();
    assert!(e.contains("referenced twice"), "{e}");
}

/// Two properties that name one data stream.
#[test]
fn a_stream_named_twice_is_an_error() {
    let mut props = vec![0x4C, 0x20];
    props.extend_from_slice(&2u16.to_le_bytes());
    let name = [vec![0x4C], utf16z("S")].concat();
    for pid in [1u16, 2] {
        for x in [pid, 0x42, name.len() as u16] {
            props.extend_from_slice(&x.to_le_bytes());
        }
    }
    props.extend_from_slice(&name);
    props.extend_from_slice(&name);
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", props).unwrap();
    w.stream(Writer::ROOT, "S", vec![1; 64]).unwrap();
    let e = store::read(&w.finish().unwrap()).unwrap_err();
    assert!(e.contains("referenced twice"), "{e}");
}

/// A storage that contains itself through a strong reference (a cycle in the directory is not
/// possible, but the property can name the parent by a path of one name; a child may not be its
/// ancestor): a chain deeper than the limit is refused rather than recursed into.
#[test]
fn nesting_deeper_than_the_limit_is_an_error() {
    let chain = |depth: usize| {
        let mut o = Obj::new([0; 16]);
        for i in 0..depth {
            o = Obj::new([i as u8; 16]).with(1, Value::Strong(Box::new(o)));
        }
        o
    };
    use super::store::Obj;
    // within the limit: read back
    let ok = chain(store::MAX_DEPTH - 2);
    assert_eq!(store::read(&store::write(&ok, Version::V3).unwrap()).unwrap(), ok);
    // past it: an error, on a default-size stack
    let deep = chain(store::MAX_DEPTH + 40);
    let e = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || store::read(&store::write(&deep, Version::V3).unwrap()).unwrap_err())
        .unwrap()
        .join()
        .unwrap();
    assert!(e.contains("too deeply"), "{e}");
}

/// A property whose declared value length runs past the stream, and an index that is cut short.
#[test]
fn damaged_property_streams_are_errors() {
    for props in [
        vec![0x4C, 0x20, 0xFF, 0xFF],                      // 65535 entries, none present
        vec![0x4C, 0x20, 1, 0, 7, 0, 0x82, 0, 0xFF, 0xFF], // one entry, value of 65535 bytes, none present
        vec![0x4C, 0x20, 1, 0, 7, 0, 0x82],                // entry cut short
        vec![0x42, 0x20, 0, 0],                            // big-endian
        vec![],                                            // empty
        vec![0x4C],                                        // one byte
    ] {
        let mut w = Writer::new(Version::V3);
        w.stream(Writer::ROOT, "properties", props).unwrap();
        assert!(store::read(&w.finish().unwrap()).is_err());
    }
}

/// A weak reference path longer than the limit, and more paths than the limit.
#[test]
fn the_referenced_properties_table_is_bounded() {
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", vec![0x4C, 0x20, 0, 0]).unwrap();
    // one path of 100000 property ids
    let mut rp = vec![0x4C];
    rp.extend_from_slice(&1u16.to_le_bytes());
    rp.extend_from_slice(&100_001u32.to_le_bytes());
    for _ in 0..100_000 {
        rp.extend_from_slice(&5u16.to_le_bytes());
    }
    rp.extend_from_slice(&0u16.to_le_bytes());
    w.stream(Writer::ROOT, "referenced properties", rp).unwrap();
    let e = store::read(&w.finish().unwrap()).unwrap_err();
    assert!(e.contains("path longer"), "{e}");

    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", vec![0x4C, 0x20, 0, 0]).unwrap();
    let mut rp = vec![0x4C];
    rp.extend_from_slice(&u16::MAX.to_le_bytes());
    rp.extend_from_slice(&0u32.to_le_bytes());
    w.stream(Writer::ROOT, "referenced properties", rp).unwrap();
    let e = store::read(&w.finish().unwrap()).unwrap_err();
    assert!(e.contains("weak-reference paths"), "{e}");
}

/// The same object used as the target of many weak references is not copied per reference: a
/// property per reference to a shared path costs one clone of a short path each.
#[test]
fn strong_references_are_a_tree() {
    // a root whose property names a child that names the root's child again is a double use
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", props_one(SF_STRONG, &utf16z("Child"))).unwrap();
    let c = w.storage(Writer::ROOT, "Child", [2; 16]).unwrap();
    w.stream(c, "properties", props_one(SF_STRONG, &utf16z("Child"))).unwrap();
    // the child's own "Child" is not there
    assert!(store::read(&w.finish().unwrap()).is_err());
}
