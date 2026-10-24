# soundcraft-interchange

Milestone M1 of AAF interchange: **container reading and identification only.**

```rust
use soundcraft_interchange::Cfb;

let cfb = Cfb::open("session.aaf")?;                 // or Cfb::from_bytes(vec)
for e in cfb.entries() { println!("{:?} {} ({} bytes)", e.kind, e.path, e.size); }
let id = cfb.identify();                              // AafIdentity { confidence, .. }
if id.is_aaf() { let h = cfb.aaf_header()?; println!("little endian: {}", h.little_endian); }
```

## Supported
- Reading Microsoft Compound File (CFB) v3 (512-byte) and v4 (4096-byte sector) containers, including
  DIFAT, mini streams, storages and streams, with sizes and class ids. Hostile-input safe: all
  allocations are bounded by the input length, chains and directory trees are cycle-checked, entry
  count and nesting are capped, no panics, no `unsafe`.
- Identification (`identify`): root class id equals `{B3B398A5-1C90-11D4-8053-080036210804}`, and/or the
  root holds storages `Header-<n>` and `MetaDictionary-<n>`. Reported as a `Confidence`, not a certainty.
- `aaf_header`: the byte-order marker (`L`/`B`), the version marker byte (raw) and the key/form/length
  index of `Header-<n>/properties`.

## Not supported
- Writing containers. AAF object model decoding, property values, the header's version/identification
  records, composition/mob mapping, essence extraction, and import into sessions.
- The class id and stream names are recalled from public documentation and have **not been verified
  against a real AAF file** (none was available when this was written). Test with a real file before
  relying on detection.
