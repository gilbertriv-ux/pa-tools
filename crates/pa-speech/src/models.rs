//! The speech model catalogue and the model store.
//!
//! The catalogue lists the models that exist: each file is pinned to a revision of the model
//! author's own repository, with its SHA-256, size and licence. Weights are never bundled with Pa
//! Tools or committed to the repository. The program only *uses* a model that is already on disk,
//! found by [`crate::discover`]; the verified downloader here (feature `download`) fills
//! `<data dir>/models/<id>/`, but nothing in the app calls it yet, so today a model is put there by
//! hand (or `$PA_SPEECH_MODELS_DIR` points at one).
//!
//! Licence: OpenAI released the Whisper code and model weights under the MIT licence
//! (<https://github.com/openai/whisper/blob/main/LICENSE>); the safetensors conversions OpenAI
//! publishes at `huggingface.co/openai/whisper-*` are labelled Apache-2.0. Both are permissive. The
//! author, licence and source fields below are attribution data and are kept as such; the
//! catalogue's display names are neutral.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One file of a model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// A downloadable speech model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// Recognises languages other than English (and detects the language).
    pub multilingual: bool,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    /// Human-readable source page.
    pub source: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

const LICENSE: &str = "MIT (OpenAI Whisper weights); Hugging Face conversion: Apache-2.0";
const LICENSE_URL: &str = "https://github.com/openai/whisper/blob/main/LICENSE";
const AUTHOR: &str = "OpenAI";

macro_rules! hf {
    ($repo:literal, $rev:literal, $name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: concat!("https://huggingface.co/openai/", $repo, "/resolve/", $rev, "/", $name), sha256: $sha, size: $size }
    };
}

const TOKENIZER_MULTI: &str = "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566";

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "whisper-tiny",
        name: "Speech model, tiny (multilingual)",
        multilingual: true,
        description: "39 M parameters. Fastest; fine for clear speech, rough on noisy audio.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-tiny",
        files: &[
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "config.json",
                "ffdccec4f3211f4c63310f2b7098f309fe70f3952cedc5e4d11e43f5b2379b98",
                1983
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "generation_config.json",
                "a5d5325911f16e74001a72fa13d6e208eee51548f994646de1f4b4cc8b35b512",
                3747
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-tiny",
                "169d4a4341b33bc18d8881c4b69c2e104e1cc0af",
                "model.safetensors",
                "7ebd0e69e78190ffe1438491fa05cc1f5c1aa3a4c4db3bc1723adbb551ea2395",
                151061672
            ),
        ],
    },
    ModelInfo {
        id: "whisper-base",
        name: "Speech model, base (multilingual)",
        multilingual: true,
        description: "74 M parameters. The default: a good balance of speed and accuracy.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-base",
        files: &[
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "config.json",
                "a153c53883a6799b6f056b4a8d1a515c9926d03994682ba88a7616618d7da0c1",
                1983
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "generation_config.json",
                "444b3f636d2fff89dd9ecf549e2a085b61f7ff0fa0246d4628bac6a3b8cc9ba4",
                3807
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-base",
                "e37978b90ca9030d5170a5c07aadb050351a65bb",
                "model.safetensors",
                "07cadb9f25677c8d50df603e66a98fbd842cce45047139baeb16e6219a1e807b",
                290403936
            ),
        ],
    },
    ModelInfo {
        id: "whisper-small",
        name: "Speech model, small (multilingual)",
        multilingual: true,
        description: "244 M parameters. Most accurate here; about 4× slower than base.",
        license: LICENSE,
        license_url: LICENSE_URL,
        author: AUTHOR,
        source: "https://huggingface.co/openai/whisper-small",
        files: &[
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "config.json",
                "e6a2b489da1b5aed65a8eb8d1e7466fa867ad5643a8bc138ba708bd56b2875c4",
                1967
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "generation_config.json",
                "71565b8ef50d0bf7a1193ed4bbed195b94e70c18894d81bba2f1233dcec3ab53",
                3868
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "tokenizer.json",
                "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
                2480466
            ),
            hf!(
                "whisper-small",
                "973afd24965f72e36ca33b3055d56a652f456b4d",
                "model.safetensors",
                "1d7734884874f1a1513ed9aa760a4f8e97aaa02fd6d93a3a85d27b2ae9ca596b",
                966995080
            ),
        ],
    },
];

/// The default model.
pub const DEFAULT_MODEL: &str = "whisper-base";

pub fn catalogue() -> &'static [ModelInfo] {
    CATALOGUE
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOGUE.iter().find(|m| m.id == id)
}

/// Where model `m` lives under `models_dir`.
pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.id)
}

/// All files present with their catalogue sizes (contents were verified when downloaded).
pub fn installed(models_dir: &Path, m: &ModelInfo) -> bool {
    let d = model_dir(models_dir, m);
    m.files.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size))
}

/// Bytes still to download for `m` (files already present are skipped).
pub fn missing_bytes(models_dir: &Path, m: &ModelInfo) -> u64 {
    let d = model_dir(models_dir, m);
    m.files.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size)).map(|f| f.size).sum()
}

/// The tokenizer shared by the multilingual models has one checksum.
pub fn shared_tokenizer_sha() -> &'static str {
    TOKENIZER_MULTI
}

/// Download progress: `(bytes done, bytes total, file)`; return false to cancel.
#[cfg(feature = "download")]
pub type DownloadProgress<'a> = &'a mut dyn FnMut(u64, u64, &str) -> bool;

/// Download the missing files of `m` into `models_dir/<id>/`: each file streams into `<name>.part`,
/// its SHA-256 is checked, then it is renamed into place. A file that is too big, has a wrong
/// checksum, stalls, or fails any other way deletes its part file and fails. Pure Rust TLS (rustls
/// + RustCrypto) with the operating system's certificate verifier.
#[cfg(feature = "download")]
pub fn download(models_dir: &Path, m: &ModelInfo, progress: DownloadProgress) -> Result<(), crate::SpeechError> {
    download_files(&model_dir(models_dir, m), m.files, progress)
}

/// [`download`] for any pinned file list: missing files of `files` go into `dir`, each verified
/// against its SHA-256 and size before it is renamed into place.
#[cfg(feature = "download")]
pub fn download_files(dir: &Path, files: &[ModelFile], progress: DownloadProgress) -> Result<(), crate::SpeechError> {
    download_with(&agent(CONNECT_TIMEOUT, READ_TIMEOUT, true), dir, files, progress)
}

/// Give up on a server that does not accept the connection within this long.
#[cfg(feature = "download")]
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Give up on a transfer in which no data arrives for this long (each read restarts it, so a slow
/// but moving download is fine).
#[cfg(feature = "download")]
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(feature = "download")]
fn download_with(agent: &ureq::Agent, dir: &Path, files: &[ModelFile], progress: DownloadProgress) -> Result<(), crate::SpeechError> {
    std::fs::create_dir_all(dir)?;
    let present = |f: &ModelFile| std::fs::metadata(dir.join(f.name)).is_ok_and(|md| md.len() == f.size);
    let total: u64 = files.iter().filter(|f| !present(f)).map(|f| f.size).sum();
    let mut done = 0u64;
    for f in files {
        if present(f) {
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let result = download_one(agent, f, &part, &dir.join(f.name), done, total, progress);
        if result.is_err() {
            // Whatever went wrong (network, size, checksum, cancel, disk), no part file is left.
            let _ = std::fs::remove_file(&part);
        }
        result?;
        done = done.saturating_add(f.size);
    }
    Ok(())
}

/// Fetch one file into `part`, verify it, and rename it to `dest`. On `Err` the caller removes `part`.
#[cfg(feature = "download")]
fn download_one(
    agent: &ureq::Agent,
    f: &ModelFile,
    part: &Path,
    dest: &Path,
    done: u64,
    total: u64,
    progress: DownloadProgress,
) -> Result<(), crate::SpeechError> {
    use crate::SpeechError;
    use sha2::Digest;
    use std::io::{Read, Write};
    let resp = agent.get(f.url).call().map_err(|e| SpeechError::Download(format!("{}: {e}", f.name)))?;
    if resp.body().content_length().is_some_and(|n| n > f.size) {
        return Err(SpeechError::Download(format!("{}: the server announces more than the {} bytes expected", f.name, f.size)));
    }
    let mut body = resp.into_body();
    // Never read more than one byte past the expected size, whatever the server sends.
    let mut reader = body.as_reader().take(f.size.saturating_add(1));
    let mut out = std::io::BufWriter::new(std::fs::File::create(part)?);
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| SpeechError::Download(format!("{}: {e}", f.name)))?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n as u64);
        if got > f.size {
            return Err(SpeechError::Download(format!("{}: more than the {} bytes expected", f.name, f.size)));
        }
        let chunk = buf.get(..n).unwrap_or_default();
        hasher.update(chunk);
        out.write_all(chunk)?;
        if !progress(done.saturating_add(got), total, f.name) {
            return Err(SpeechError::Cancelled);
        }
    }
    out.flush()?;
    drop(out);
    let sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if sha != f.sha256 || got != f.size {
        return Err(SpeechError::Download(format!(
            "{}: checksum mismatch (got {sha}, {got} bytes; expected {}, {} bytes)",
            f.name, f.sha256, f.size
        )));
    }
    std::fs::rename(part, dest)?;
    Ok(())
}

/// The HTTP client: pure-Rust TLS with the operating system's certificate verifier, a connect
/// timeout and a per-read timeout. `use_env_proxy` follows the proxy environment variables.
#[cfg(feature = "download")]
fn agent(connect: std::time::Duration, read: std::time::Duration, use_env_proxy: bool) -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    let mut config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(connect))
        .timeout_per_read(Some(read));
    if !use_env_proxy {
        config = config.proxy(None);
    }
    config.build().into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_pinned_and_checksummed() {
        assert!(find(DEFAULT_MODEL).is_some());
        for m in catalogue() {
            assert!(m.size() > 100_000_000, "{}", m.id);
            for f in m.files {
                assert!(f.url.starts_with("https://huggingface.co/openai/whisper-"), "{}", f.url);
                assert!(f.url.contains("/resolve/") && f.url.ends_with(f.name));
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                if f.name == "tokenizer.json" {
                    assert_eq!(f.sha256, shared_tokenizer_sha());
                }
            }
        }
    }

    #[cfg(feature = "download")]
    mod download {
        use super::super::*;
        use sha2::Digest;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        /// What the test server does with a request for a path.
        #[derive(Clone)]
        enum Reply {
            /// 200 with this body (Content-Length set).
            Body(Vec<u8>),
            /// 200 announcing `announce` bytes, sending only `body`, then closing.
            Short {
                announce: usize,
                body: Vec<u8>,
            },
            /// 200 with no Content-Length (read until close): `body`.
            Unannounced(Vec<u8>),
            /// 200 announcing `announce` bytes, sending `body` and then going quiet for a while.
            Stall {
                announce: usize,
                body: Vec<u8>,
            },
            Status(u16),
        }

        /// Serve `reply` for every request on a loopback port; returns the URL of `/file`.
        fn serve(reply: Reply) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(mut c) = conn else { continue };
                    let reply = reply.clone();
                    std::thread::spawn(move || {
                        let mut req = [0u8; 4096];
                        let mut n = 0;
                        while n < req.len() {
                            let Ok(k) = c.read(&mut req[n..]) else { return };
                            if k == 0 {
                                return;
                            }
                            n += k;
                            if req[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let head = |status: &str, len: Option<usize>| {
                            let len = len.map(|l| format!("Content-Length: {l}\r\n")).unwrap_or_default();
                            format!("HTTP/1.1 {status}\r\n{len}Connection: close\r\n\r\n")
                        };
                        let _ = match reply {
                            Reply::Body(b) => c.write_all(head("200 OK", Some(b.len())).as_bytes()).and_then(|_| c.write_all(&b)),
                            Reply::Short { announce, body } => {
                                c.write_all(head("200 OK", Some(announce)).as_bytes()).and_then(|_| c.write_all(&body))
                            }
                            Reply::Unannounced(b) => c.write_all(head("200 OK", None).as_bytes()).and_then(|_| c.write_all(&b)),
                            Reply::Stall { announce, body } => {
                                let r =
                                    c.write_all(head("200 OK", Some(announce)).as_bytes()).and_then(|_| c.write_all(&body)).and_then(|_| c.flush());
                                std::thread::sleep(Duration::from_secs(5));
                                r
                            }
                            Reply::Status(code) => c.write_all(head(&format!("{code} Nope"), Some(0)).as_bytes()),
                        };
                    });
                }
            });
            format!("http://127.0.0.1:{port}/file")
        }

        fn sha(bytes: &[u8]) -> &'static str {
            let hex: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
            Box::leak(hex.into_boxed_str())
        }

        /// A catalogue entry for `want` served at `url` (the strings are leaked: it is a test).
        fn file(url: String, want: &[u8]) -> ModelFile {
            ModelFile { name: "weights.bin", url: Box::leak(url.into_boxed_str()), sha256: sha(want), size: want.len() as u64 }
        }

        struct Dir(PathBuf);
        impl Dir {
            fn new(tag: &str) -> Dir {
                let d = std::env::temp_dir().join(format!("pa-speech-dl-{tag}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&d);
                Dir(d)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn run(dir: &Dir, f: ModelFile, read: Duration) -> Result<(), crate::SpeechError> {
            download_with(&agent(Duration::from_secs(5), read, false), &dir.0, &[f], &mut |_, _, _| true)
        }

        fn leftovers(dir: &Dir) -> Vec<String> {
            let mut names: Vec<String> =
                std::fs::read_dir(&dir.0).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            names.sort();
            names
        }

        #[test]
        fn a_good_download_lands_in_place_with_no_part_file() {
            let want = vec![7u8; 100_000];
            let dir = Dir::new("good");
            run(&dir, file(serve(Reply::Body(want.clone())), &want), Duration::from_secs(5)).unwrap();
            assert_eq!(leftovers(&dir), ["weights.bin"]);
            assert_eq!(std::fs::read(dir.0.join("weights.bin")).unwrap(), want);
        }

        #[test]
        fn a_body_larger_than_the_catalogue_size_is_cut_off_and_removed() {
            let want = vec![1u8; 1_000];
            let dir = Dir::new("big");
            // announced and unannounced: the first is refused up front, the second after one extra byte
            for reply in [Reply::Body(vec![1u8; 5_000_000]), Reply::Unannounced(vec![1u8; 5_000_000])] {
                let err = run(&dir, file(serve(reply), &want), Duration::from_secs(5)).unwrap_err();
                assert!(matches!(err, crate::SpeechError::Download(_)), "{err:?}");
                let text = err.to_string();
                assert!(text.contains("more than") && !text.contains("checksum"), "refused for its size, not after reading it all: {text}");
                assert_eq!(leftovers(&dir), Vec::<String>::new(), "no part file and no destination");
            }
        }

        #[test]
        fn a_wrong_checksum_or_a_short_body_leaves_nothing_behind() {
            let want = vec![2u8; 4_000];
            let dir = Dir::new("sum");
            // same size, different content
            let err = run(&dir, file(serve(Reply::Body(vec![3u8; 4_000])), &want), Duration::from_secs(5)).unwrap_err();
            assert!(err.to_string().contains("checksum mismatch"), "{err}");
            assert_eq!(leftovers(&dir), Vec::<String>::new());
            // the server hangs up early
            let err = run(&dir, file(serve(Reply::Short { announce: 4_000, body: vec![2u8; 1_000] }), &want), Duration::from_secs(5)).unwrap_err();
            assert!(matches!(err, crate::SpeechError::Download(_)), "{err:?}");
            assert_eq!(leftovers(&dir), Vec::<String>::new());
            // an error status
            let err = run(&dir, file(serve(Reply::Status(404)), &want), Duration::from_secs(5)).unwrap_err();
            assert!(matches!(err, crate::SpeechError::Download(_)), "{err:?}");
            assert_eq!(leftovers(&dir), Vec::<String>::new());
        }

        #[test]
        fn a_stalled_transfer_times_out_and_removes_the_part_file() {
            let want = vec![4u8; 4_000];
            let dir = Dir::new("stall");
            let url = serve(Reply::Stall { announce: 4_000, body: vec![4u8; 1_000] });
            let started = std::time::Instant::now();
            let err = run(&dir, file(url, &want), Duration::from_millis(400)).unwrap_err();
            assert!(matches!(err, crate::SpeechError::Download(_)), "{err:?}");
            assert!(started.elapsed() < Duration::from_secs(4), "gave up promptly, not after the server's 5 s");
            assert_eq!(leftovers(&dir), Vec::<String>::new());
        }

        #[test]
        fn cancelling_removes_the_part_file() {
            let want = vec![5u8; 50_000];
            let dir = Dir::new("cancel");
            let f = file(serve(Reply::Body(want.clone())), &want);
            let err = download_with(&agent(Duration::from_secs(5), Duration::from_secs(5), false), &dir.0, &[f], &mut |_, _, _| false).unwrap_err();
            assert_eq!(err, crate::SpeechError::Cancelled);
            assert_eq!(leftovers(&dir), Vec::<String>::new());
        }

        #[test]
        fn the_real_agent_has_timeouts_set() {
            // 30 s to connect and 30 s without data: pinned so they are not quietly removed.
            assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(30));
            assert_eq!(READ_TIMEOUT, Duration::from_secs(30));
        }
    }

    #[test]
    fn display_text_names_no_third_party_product() {
        // The author, licence and source fields carry the (required) attribution; what a person is
        // shown to pick a model does not name a product or company.
        for m in catalogue() {
            for text in [m.name, m.description] {
                let lower = text.to_lowercase();
                assert!(!["whisper", "openai", "hugging"].iter().any(|n| lower.contains(n)), "{}: {text:?}", m.id);
            }
            assert!(m.id.starts_with("whisper-"), "the id is a file-system name and stays");
            assert_eq!(m.author, "OpenAI");
            assert!(m.license.contains("MIT") && m.license_url.starts_with("https://"));
            assert!(m.source.starts_with("https://"));
        }
    }

    #[test]
    fn installed_checks_sizes() {
        let dir = std::env::temp_dir().join(format!("pa-speech-models-{}", std::process::id()));
        let m = find("whisper-tiny").expect("whisper-tiny in catalogue");
        assert!(!installed(&dir, m));
        assert_eq!(missing_bytes(&dir, m), m.size());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
