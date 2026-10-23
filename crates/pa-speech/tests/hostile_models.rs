//! Hostile model folders: files of exactly the catalogue sizes (which is all `models::installed`
//! checks) whose *content* is crafted to crash a careless loader. `load` must refuse each one with
//! an error, never panic, abort or allocate from a number in the file.
//!
//! Every case is written as valid JSON padded with spaces up to the catalogue size, so that the
//! parse succeeds and it is the validation that has to say no. The weights are a sparse zero file:
//! validation runs before they are read, so a rejection that comes from the weights (the
//! `model.safetensors` prefix in the message) would mean the validation let the case through. The
//! baseline test shows that a well-formed folder does get as far as the weights.
#![cfg(feature = "whisper")]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};
use soundcraft_pa_speech::{SpeechError, load, models};

const MODEL: &str = "whisper-tiny";
const VOCAB: u64 = 51_865;
const EOT: u64 = 50_257;
const SOT: u64 = 50_258;
const TRANSLATE: u64 = 50_358;
const TRANSCRIBE: u64 = 50_359;
const NO_SPEECH: u64 = 50_362;
const NO_TIMESTAMPS: u64 = 50_363;
const TS_BEGIN: u64 = 50_364;

/// A scratch folder removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("pa-speech-hostile-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch folder");
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A well-formed `config.json` (the shape of the smallest model's).
fn config() -> Value {
    json!({
        "num_mel_bins": 80, "d_model": 384,
        "encoder_layers": 4, "encoder_attention_heads": 6,
        "decoder_layers": 4, "decoder_attention_heads": 6,
        "max_source_positions": 1500, "max_target_positions": 448,
        "vocab_size": VOCAB
    })
}

fn generation_config() -> Value {
    json!({
        "alignment_heads": [[2, 2], [3, 0], [3, 2]],
        "suppress_tokens": [1, 2, 7],
        "begin_suppress_tokens": [220, EOT]
    })
}

/// Added tokens as `(content, id)`: the specials of a multilingual model, in their usual places.
fn added() -> Vec<(String, u64)> {
    let mut v = vec![
        ("<|endoftext|>".to_string(), EOT),
        ("<|startoftranscript|>".to_string(), SOT),
        ("<|en|>".to_string(), SOT + 1),
        ("<|de|>".to_string(), SOT + 2),
        ("<|translate|>".to_string(), TRANSLATE),
        ("<|transcribe|>".to_string(), TRANSCRIBE),
        ("<|nospeech|>".to_string(), NO_SPEECH),
        ("<|notimestamps|>".to_string(), NO_TIMESTAMPS),
        ("<|0.00|>".to_string(), TS_BEGIN),
    ];
    v.push(("<|0.02|>".to_string(), TS_BEGIN + 1));
    v
}

fn tokenizer_of(vocab: Value, added: &[(String, u64)]) -> Value {
    let added: Vec<Value> = added.iter().map(|(c, id)| json!({"id": id, "content": c, "special": true})).collect();
    json!({"model": {"type": "BPE", "vocab": vocab, "merges": []}, "added_tokens": added})
}

fn tokenizer() -> Value {
    tokenizer_of(json!({"!": 0, "a": 1, "b": 2, "\u{120}the": 3}), &added())
}

/// Write `value` as `name`, padded with spaces to the catalogue size of that file.
fn write_padded(dir: &Path, name: &str, value: &Value) {
    let m = models::find(MODEL).expect("model in the catalogue");
    let size = m.files.iter().find(|f| f.name == name).expect("file in the catalogue").size;
    let mut bytes = serde_json::to_vec(value).expect("serialise");
    assert!(bytes.len() as u64 <= size, "{name}: {} bytes do not fit the catalogue size {size}", bytes.len());
    bytes.resize(usize::try_from(size).expect("size fits"), b' ');
    std::fs::write(dir.join(name), bytes).expect("write file");
}

/// Install the model under a scratch `models_dir` with the given JSON files; sparse weights.
fn install(tag: &str, config: &Value, generation: &Value, tokenizer: &Value) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(tag);
    let models_dir = scratch.path().to_path_buf();
    let m = models::find(MODEL).expect("model in the catalogue");
    let dir = models::model_dir(&models_dir, m);
    std::fs::create_dir_all(&dir).expect("create model folder");
    write_padded(&dir, "config.json", config);
    write_padded(&dir, "generation_config.json", generation);
    write_padded(&dir, "tokenizer.json", tokenizer);
    let weights = m.files.iter().find(|f| f.name == "model.safetensors").expect("weights in the catalogue");
    File::create(dir.join(weights.name)).and_then(|f| f.set_len(weights.size)).expect("sparse weights");
    assert!(models::installed(&models_dir, m), "the hostile folder must pass `installed`: that is the whole point");
    (scratch, models_dir)
}

/// `load` must fail with a model error whose text contains `why` and which did not come from the
/// weights (validation runs first).
fn refuses(tag: &str, config: &Value, generation: &Value, tokenizer: &Value, why: &str) {
    let (_scratch, models_dir) = install(tag, config, generation, tokenizer);
    let err = load(&models_dir, MODEL).err().unwrap_or_else(|| panic!("{tag}: the hostile model was accepted"));
    let SpeechError::Model(text) = &err else { panic!("{tag}: expected a model error, got {err:?}") };
    assert!(text.contains(why), "{tag}: expected {why:?} in {text:?}");
    assert!(!text.contains("model.safetensors"), "{tag}: the case got as far as the weights: {text:?}");
}

#[test]
fn a_well_formed_folder_gets_as_far_as_the_weights() {
    // Baseline: the synthetic config and tokenizer are accepted; only the (zero) weights are bad.
    let (_scratch, models_dir) = install("baseline", &config(), &generation_config(), &tokenizer());
    let err = load(&models_dir, MODEL).err().expect("zero weights cannot load");
    let SpeechError::Model(text) = &err else { panic!("expected a model error, got {err:?}") };
    assert!(text.contains("model.safetensors"), "{text:?}");
}

#[test]
fn a_vocabulary_id_far_beyond_the_vocabulary_is_refused_not_allocated() {
    // Used to allocate a table of 10^15 entries (about 24 TB) and abort the process.
    let tok = tokenizer_of(json!({"!": 1_000_000_000_000_000u64, "a": 1}), &added());
    refuses("hugeid", &config(), &generation_config(), &tok, "vocab id");
}

#[test]
fn a_vocabulary_larger_than_vocab_size_is_refused() {
    let mut cfg = config();
    cfg["vocab_size"] = json!(3);
    let tok = tokenizer_of(json!({"a": 0, "b": 1, "c": 2, "d": 3}), &[]);
    refuses("bigvocab", &cfg, &generation_config(), &tok, "vocab");
}

#[test]
fn an_added_token_id_that_wraps_a_u32_is_refused() {
    let mut a = added();
    a.push(("<|zz|>".to_string(), (1u64 << 32) + 5));
    refuses("wrapid", &config(), &generation_config(), &tokenizer_of(json!({"a": 0}), &a), "outside");
}

#[test]
fn a_short_added_token_between_the_specials_is_refused_not_sliced() {
    // `<|>` starts with `<|` and ends with `|>` but has no body: slicing it panicked.
    let a: Vec<(String, u64)> = added().into_iter().map(|(c, id)| if c == "<|en|>" { ("<|>".to_string(), id) } else { (c, id) }).collect();
    refuses("angle", &config(), &generation_config(), &tokenizer_of(json!({"a": 0}), &a), "language token");
}

#[test]
fn a_timestamp_token_beyond_vocab_size_is_refused() {
    // `<|0.00|>` at 60000 with 51865 logits: indexed the logits out of range.
    let a: Vec<(String, u64)> = added().into_iter().map(|(c, id)| if c == "<|0.00|>" { (c, 60_000) } else { (c, id) }).collect();
    refuses("tsbegin", &config(), &generation_config(), &tokenizer_of(json!({"a": 0}), &a), "outside");
}

#[test]
fn every_special_token_must_be_inside_the_vocabulary() {
    for target in ["<|endoftext|>", "<|startoftranscript|>", "<|transcribe|>", "<|notimestamps|>", "<|nospeech|>", "<|translate|>"] {
        let a: Vec<(String, u64)> = added().into_iter().map(|(c, id)| if c == target { (c, VOCAB + 7) } else { (c, id) }).collect();
        refuses(&format!("special-{}", &target[2..5]), &config(), &generation_config(), &tokenizer_of(json!({"a": 0}), &a), "outside");
    }
}

#[test]
fn zero_attention_heads_are_refused_not_divided_by() {
    for key in ["decoder_attention_heads", "encoder_attention_heads"] {
        let mut cfg = config();
        cfg[key] = json!(0);
        refuses(&format!("heads0-{key}"), &cfg, &generation_config(), &tokenizer(), "attention_heads");
    }
}

#[test]
fn heads_must_divide_the_model_width() {
    let mut cfg = config();
    cfg["decoder_attention_heads"] = json!(7); // 384 % 7 != 0
    refuses("heads7", &cfg, &generation_config(), &tokenizer(), "attention_heads");
}

#[test]
fn absurd_dimensions_are_refused() {
    for (key, value) in [
        ("d_model", json!(1u64 << 40)),
        ("num_mel_bins", json!(0)),
        ("num_mel_bins", json!(100_000)),
        ("encoder_layers", json!(0)),
        ("decoder_layers", json!(1_000_000_000u64)),
        ("max_source_positions", json!(10)),
        ("max_source_positions", json!(1u64 << 40)),
        ("max_target_positions", json!(2)),
        ("max_target_positions", json!(1u64 << 40)),
        ("vocab_size", json!(0)),
        ("vocab_size", json!(1u64 << 40)),
        ("vocab_size", json!(-5)),
    ] {
        let mut cfg = config();
        cfg[key] = value.clone();
        let (_scratch, models_dir) = install("dims", &cfg, &generation_config(), &tokenizer());
        let err = load(&models_dir, MODEL).err().unwrap_or_else(|| panic!("{key}={value} was accepted"));
        let SpeechError::Model(text) = &err else { panic!("{key}={value}: expected a model error, got {err:?}") };
        assert!(!text.contains("model.safetensors"), "{key}={value}: got as far as the weights: {text:?}");
    }
}

#[test]
fn alignment_heads_must_exist() {
    for bad in [json!([[99, 0]]), json!([[0, 99]]), json!([[u64::MAX, 0]])] {
        let mut generation = generation_config();
        generation["alignment_heads"] = bad.clone();
        refuses("alignhead", &config(), &generation, &tokenizer(), "alignment head");
    }
}

#[test]
fn missing_or_unreadable_files_are_errors() {
    let (_scratch, models_dir) = install("garbage", &config(), &generation_config(), &tokenizer());
    let m = models::find(MODEL).expect("model in the catalogue");
    let dir = models::model_dir(&models_dir, m);
    // Same size, not JSON at all.
    let size = m.files.iter().find(|f| f.name == "config.json").expect("config in the catalogue").size;
    std::fs::write(dir.join("config.json"), vec![0xFFu8; usize::try_from(size).expect("size fits")]).expect("overwrite");
    assert!(matches!(load(&models_dir, MODEL).err(), Some(SpeechError::Model(_))));
}
