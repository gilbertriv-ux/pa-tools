//! The Whisper network (encoder–decoder transformer), written against candle from the published
//! architecture (Radford et al. 2022, "Robust Speech Recognition via Large-Scale Weak
//! Supervision", and OpenAI's MIT-licensed model description), reading the tensor names of the
//! Hugging Face safetensors release.
//!
//! Encoder: two 1-D convolutions (k = 3, the second with stride 2) with GELU, learned/sinusoidal
//! position table, pre-LayerNorm self-attention blocks, final LayerNorm. Decoder: token and
//! position embeddings, pre-LayerNorm blocks of causal self-attention, cross-attention to the
//! audio and an MLP, final LayerNorm, logits tied to the token embedding.
//!
//! The decoder keeps a key/value cache for incremental decoding and can return the cross-attention
//! logits of chosen heads (word alignment).

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::{Conv1d, Conv1dConfig, Embedding, LayerNorm, Linear, VarBuilder};

/// `config.json` fields we use.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub num_mel_bins: usize,
    pub d_model: usize,
    pub encoder_layers: usize,
    pub encoder_attention_heads: usize,
    pub decoder_layers: usize,
    pub decoder_attention_heads: usize,
    pub max_source_positions: usize,
    pub max_target_positions: usize,
    pub vocab_size: usize,
}

/// Largest values a `config.json` may declare. Real models are far inside these (the biggest has
/// 1280 channels, 32 layers, 20 heads, 128 mel bins, about 52 000 tokens); they exist so that a
/// damaged or hostile file cannot size a table, a loop or a division from its numbers.
pub const MAX_MELS: usize = 256;
pub const MAX_WIDTH: usize = 4096;
pub const MAX_HEADS: usize = 128;
pub const MAX_LAYERS: usize = 64;
pub const MAX_POSITIONS: usize = 16_384;
/// The decoder needs room for its prompt, the longest window of text and the end marker.
pub const MIN_TARGET_POSITIONS: usize = 16;

impl Config {
    /// Refuse a configuration that cannot describe a Whisper network (see the limits above).
    /// Returns what is wrong, ready to follow `config.json: `.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let within = |name: &str, v: usize, lo: usize, hi: usize| {
            if (lo..=hi).contains(&v) { Ok(()) } else { Err(format!("{name} is {v}, outside {lo}..={hi}")) }
        };
        within("num_mel_bins", self.num_mel_bins, 1, MAX_MELS)?;
        within("d_model", self.d_model, 1, MAX_WIDTH)?;
        within("encoder_layers", self.encoder_layers, 1, MAX_LAYERS)?;
        within("decoder_layers", self.decoder_layers, 1, MAX_LAYERS)?;
        within("encoder_attention_heads", self.encoder_attention_heads, 1, MAX_HEADS)?;
        within("decoder_attention_heads", self.decoder_attention_heads, 1, MAX_HEADS)?;
        // The encoder always sees 30 s: 3000 mel frames, halved by the stride-2 convolution.
        within("max_source_positions", self.max_source_positions, super::N_FRAMES / 2, MAX_POSITIONS)?;
        within("max_target_positions", self.max_target_positions, MIN_TARGET_POSITIONS, MAX_POSITIONS)?;
        within("vocab_size", self.vocab_size, 1, super::tokenizer::MAX_VOCAB)?;
        for (name, heads) in [("encoder_attention_heads", self.encoder_attention_heads), ("decoder_attention_heads", self.decoder_attention_heads)] {
            if !self.d_model.is_multiple_of(heads) {
                return Err(format!("d_model {} is not a multiple of {name} {heads}", self.d_model));
            }
        }
        Ok(())
    }
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
    heads: usize,
    /// Channels per head (`d / heads`, checked at construction: never zero, never a remainder).
    head_dim: usize,
    /// cached keys/values (self-attention: grows; cross-attention: the audio's, computed once)
    cache: Option<(Tensor, Tensor)>,
}

impl Attention {
    fn new(d: usize, heads: usize, vb: VarBuilder) -> Result<Self> {
        if heads == 0 || !d.is_multiple_of(heads) {
            candle_core::bail!("{heads} attention heads do not divide the model width {d}");
        }
        Ok(Self {
            q: candle_nn::linear(d, d, vb.pp("q_proj"))?,
            k: candle_nn::linear_no_bias(d, d, vb.pp("k_proj"))?,
            v: candle_nn::linear(d, d, vb.pp("v_proj"))?,
            out: candle_nn::linear(d, d, vb.pp("out_proj"))?,
            heads,
            head_dim: d / heads,
            cache: None,
        })
    }

    fn split(&self, x: &Tensor) -> Result<Tensor> {
        let (b, t, _) = x.dims3()?;
        x.reshape((b, t, self.heads, self.head_dim))?.transpose(1, 2)?.contiguous()
    }

    /// `x`: queries (b, t, d). `kv`: None = self-attention on `x` (appending to the cache when
    /// `cached`), Some(audio) = cross-attention (keys/values cached after the first call when
    /// `cached`). Returns the output and the attention logits (b, heads, t, tk).
    fn forward(&mut self, x: &Tensor, kv: Option<&Tensor>, mask: Option<&Tensor>, cached: bool) -> Result<(Tensor, Tensor)> {
        let (_, t, d) = x.dims3()?;
        let q = (self.split(&self.q.forward(x)?)? * (self.head_dim as f64).powf(-0.5))?;
        let (k, v) = match kv {
            None => {
                let k = self.split(&self.k.forward(x)?)?;
                let v = self.split(&self.v.forward(x)?)?;
                if cached {
                    let (k, v) = match &self.cache {
                        Some((pk, pv)) => (Tensor::cat(&[pk, &k], 2)?, Tensor::cat(&[pv, &v], 2)?),
                        None => (k, v),
                    };
                    self.cache = Some((k.clone(), v.clone()));
                    (k, v)
                } else {
                    (k, v)
                }
            }
            Some(xa) => match (&self.cache, cached) {
                (Some((k, v)), true) => (k.clone(), v.clone()),
                _ => {
                    let k = self.split(&self.k.forward(xa)?)?;
                    let v = self.split(&self.v.forward(xa)?)?;
                    if cached {
                        self.cache = Some((k.clone(), v.clone()));
                    }
                    (k, v)
                }
            },
        };
        let mut qk = q.matmul(&k.transpose(2, 3)?.contiguous()?)?;
        if let Some(m) = mask {
            qk = qk.broadcast_add(m)?;
        }
        let w = candle_nn::ops::softmax_last_dim(&qk)?;
        let o = w.matmul(&v)?.transpose(1, 2)?.contiguous()?;
        let (b, _, _, _) = o.dims4()?;
        Ok((self.out.forward(&o.reshape((b, t, d))?)?, qk))
    }
}

struct Block {
    attn: Attention,
    attn_ln: LayerNorm,
    cross: Option<(Attention, LayerNorm)>,
    fc1: Linear,
    fc2: Linear,
    mlp_ln: LayerNorm,
}

impl Block {
    fn new(d: usize, heads: usize, cross: bool, vb: VarBuilder) -> Result<Self> {
        let cross = if cross {
            Some((Attention::new(d, heads, vb.pp("encoder_attn"))?, candle_nn::layer_norm(d, 1e-5, vb.pp("encoder_attn_layer_norm"))?))
        } else {
            None
        };
        Ok(Self {
            attn: Attention::new(d, heads, vb.pp("self_attn"))?,
            attn_ln: candle_nn::layer_norm(d, 1e-5, vb.pp("self_attn_layer_norm"))?,
            cross,
            fc1: candle_nn::linear(d, 4 * d, vb.pp("fc1"))?,
            fc2: candle_nn::linear(4 * d, d, vb.pp("fc2"))?,
            mlp_ln: candle_nn::layer_norm(d, 1e-5, vb.pp("final_layer_norm"))?,
        })
    }

    fn forward(&mut self, x: &Tensor, xa: Option<&Tensor>, mask: Option<&Tensor>, cached: bool) -> Result<(Tensor, Option<Tensor>)> {
        let (a, _) = self.attn.forward(&self.attn_ln.forward(x)?, None, mask, cached)?;
        let mut x = (x + a)?;
        let mut qk = None;
        if let (Some((c, ln)), Some(xa)) = (&mut self.cross, xa) {
            let (a, w) = c.forward(&ln.forward(&x)?, Some(xa), None, cached)?;
            x = (x + a)?;
            qk = Some(w);
        }
        let h = self.fc2.forward(&self.fc1.forward(&self.mlp_ln.forward(&x)?)?.gelu_erf()?)?;
        Ok(((x + h)?, qk))
    }

    fn reset(&mut self) {
        self.attn.cache = None;
        if let Some((c, _)) = &mut self.cross {
            c.cache = None;
        }
    }
}

pub struct Encoder {
    conv1: Conv1d,
    conv2: Conv1d,
    pos: Tensor,
    blocks: Vec<Block>,
    ln: LayerNorm,
}

impl Encoder {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let d = cfg.d_model;
        let c1 = Conv1dConfig { padding: 1, stride: 1, ..Default::default() };
        let c2 = Conv1dConfig { padding: 1, stride: 2, ..Default::default() };
        Ok(Self {
            conv1: candle_nn::conv1d(cfg.num_mel_bins, d, 3, c1, vb.pp("conv1"))?,
            conv2: candle_nn::conv1d(d, d, 3, c2, vb.pp("conv2"))?,
            pos: vb.get((cfg.max_source_positions, d), "embed_positions.weight")?,
            blocks: (0..cfg.encoder_layers)
                .map(|i| Block::new(d, cfg.encoder_attention_heads, false, vb.pp(format!("layers.{i}"))))
                .collect::<Result<_>>()?,
            ln: candle_nn::layer_norm(d, 1e-5, vb.pp("layer_norm"))?,
        })
    }

    /// `mel`: (1, n_mels, 3000) → (1, 1500, d).
    pub fn forward(&mut self, mel: &Tensor) -> Result<Tensor> {
        let x = self.conv1.forward(mel)?.gelu_erf()?;
        let x = self.conv2.forward(&x)?.gelu_erf()?;
        let x = x.transpose(1, 2)?;
        let t = x.dim(1)?;
        let mut x = x.broadcast_add(&self.pos.narrow(0, 0, t)?)?;
        for b in &mut self.blocks {
            x = b.forward(&x, None, None, false)?.0;
        }
        self.ln.forward(&x)
    }
}

pub struct Decoder {
    tok: Embedding,
    /// The token embedding transposed (d, vocab), contiguous: the output projection.
    tok_t: Tensor,
    pos: Tensor,
    blocks: Vec<Block>,
    ln: LayerNorm,
    /// tokens already in the self-attention cache
    offset: usize,
}

impl Decoder {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let d = cfg.d_model;
        let tok = candle_nn::embedding(cfg.vocab_size, d, vb.pp("embed_tokens"))?;
        Ok(Self {
            tok_t: tok.embeddings().t()?.contiguous()?,
            tok,
            pos: vb.get((cfg.max_target_positions, d), "embed_positions.weight")?,
            blocks: (0..cfg.decoder_layers)
                .map(|i| Block::new(d, cfg.decoder_attention_heads, true, vb.pp(format!("layers.{i}"))))
                .collect::<Result<_>>()?,
            ln: candle_nn::layer_norm(d, 1e-5, vb.pp("layer_norm"))?,
            offset: 0,
        })
    }

    /// Forget the cached keys/values (new audio window or new token sequence).
    pub fn reset(&mut self) {
        self.offset = 0;
        for b in &mut self.blocks {
            b.reset();
        }
    }

    fn mask(t: usize, offset: usize, dev: &Device) -> Result<Tensor> {
        let m: Vec<f32> = (0..t).flat_map(|i| (0..offset + t).map(move |j| if j > offset + i { f32::NEG_INFINITY } else { 0.0 })).collect();
        Tensor::from_vec(m, (t, offset + t), dev)
    }

    /// Logits (1, t, vocab) for `tokens` appended to the cache. With `qk_heads`, also returns the
    /// cross-attention logits of those (layer, head) pairs: (heads, t, audio frames).
    pub fn forward(&mut self, tokens: &[u32], xa: &Tensor, cached: bool, qk_heads: Option<&[(usize, usize)]>) -> Result<(Tensor, Option<Tensor>)> {
        let dev = xa.device();
        let t = tokens.len();
        let offset = if cached { self.offset } else { 0 };
        let ids = Tensor::new(tokens, dev)?.unsqueeze(0)?;
        let mut x = self.tok.forward(&ids)?.broadcast_add(&self.pos.narrow(0, offset, t)?)?;
        let mask = if t > 1 { Some(Self::mask(t, offset, dev)?) } else { None };
        let mut picked = Vec::new();
        for (li, b) in self.blocks.iter_mut().enumerate() {
            let (y, qk) = b.forward(&x, Some(xa), mask.as_ref(), cached)?;
            x = y;
            if let (Some(heads), Some(qk)) = (qk_heads, qk) {
                for &(l, h) in heads {
                    if l == li {
                        picked.push(qk.get(0)?.get(h)?);
                    }
                }
            }
        }
        if cached {
            self.offset += t;
        }
        let x = self.ln.forward(&x)?;
        let logits = x.broadcast_matmul(&self.tok_t)?;
        let qk = if picked.is_empty() { None } else { Some(Tensor::stack(&picked, 0)?) };
        Ok((logits, qk))
    }
}

pub struct Model {
    pub cfg: Config,
    pub encoder: Encoder,
    pub decoder: Decoder,
    pub device: Device,
}

/// Refuse weights that hold NaN or infinity: a network built from them runs, and answers with
/// noise (a model of NaN transcribed 11 s of speech as `!!!!!!!!!!`). One pass over each
/// floating-point tensor's bytes, no copy; the first tensor (in name order) that has one is named.
/// Integer tensors are not looked at.
fn check_finite(weights: &[u8]) -> Result<()> {
    let st = candle_core::safetensors::SliceSafetensors::new(weights)?;
    let mut tensors = st.tensors();
    tensors.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, view) in tensors {
        let data = view.data();
        // The exponent field all ones is infinity (mantissa zero) or NaN (mantissa not): not finite.
        let non_finite = match DType::try_from(view.dtype()) {
            Ok(DType::F32) => data.chunks_exact(4).any(|c| <[u8; 4]>::try_from(c).map_or(true, |b| !f32::from_le_bytes(b).is_finite())),
            Ok(DType::F64) => data.chunks_exact(8).any(|c| <[u8; 8]>::try_from(c).map_or(true, |b| !f64::from_le_bytes(b).is_finite())),
            Ok(DType::F16) => data.chunks_exact(2).any(|c| <[u8; 2]>::try_from(c).map_or(true, |b| (u16::from_le_bytes(b) >> 10) & 0x1F == 0x1F)),
            Ok(DType::BF16) => data.chunks_exact(2).any(|c| <[u8; 2]>::try_from(c).map_or(true, |b| (u16::from_le_bytes(b) >> 7) & 0xFF == 0xFF)),
            _ => false,
        };
        if non_finite {
            candle_core::bail!("tensor `{name}` holds NaN or infinity");
        }
    }
    Ok(())
}

impl Model {
    /// Build the network from the weights file's bytes. Refuses weights that are not finite
    /// ([`check_finite`]) before building anything from them.
    pub fn load(cfg: Config, weights: Vec<u8>) -> Result<Self> {
        check_finite(&weights)?;
        let device = Device::Cpu;
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &device)?;
        let vb = vb.pp("model");
        Ok(Self { encoder: Encoder::new(&cfg, vb.pp("encoder"))?, decoder: Decoder::new(&cfg, vb.pp("decoder"))?, cfg, device })
    }

    pub fn last_logits(logits: &Tensor) -> Result<Vec<f32>> {
        let t = logits.dim(1)?;
        let Some(last) = t.checked_sub(1) else { candle_core::bail!("the decoder returned no logits") };
        logits.get(0)?.get(last)?.to_dtype(DType::F32)?.to_vec1()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn tiny() -> Config {
        Config {
            num_mel_bins: 80,
            d_model: 384,
            encoder_layers: 4,
            encoder_attention_heads: 6,
            decoder_layers: 4,
            decoder_attention_heads: 6,
            max_source_positions: 1500,
            max_target_positions: 448,
            vocab_size: 51_865,
        }
    }

    /// A safetensors file holding `tensors` as `(name, dtype, bytes)`.
    fn safetensors(tensors: &[(&str, &str, Vec<u8>)]) -> Vec<u8> {
        let mut header = serde_json::Map::new();
        let mut body = Vec::new();
        for (name, dtype, bytes) in tensors {
            let width = match *dtype {
                "F64" | "I64" => 8,
                "F32" => 4,
                _ => 2,
            };
            let start = body.len();
            body.extend_from_slice(bytes);
            header.insert(
                (*name).to_string(),
                serde_json::json!({"dtype": dtype, "shape": [bytes.len() / width], "data_offsets": [start, body.len()]}),
            );
        }
        let header = serde_json::to_vec(&header).unwrap();
        let mut file = (header.len() as u64).to_le_bytes().to_vec();
        file.extend_from_slice(&header);
        file.extend_from_slice(&body);
        file
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn weights_with_nan_or_infinity_are_refused_naming_the_tensor() {
        let fine = || ("model.decoder.ok", "F32", f32s(&[0.5, -2.0, 0.0, f32::MAX, f32::MIN_POSITIVE / 2.0]));
        assert!(check_finite(&safetensors(&[fine()])).is_ok(), "ordinary values, zero, the largest and a subnormal are finite");
        for (what, bad) in [
            ("NaN", f32s(&[1.0, f32::NAN])),
            ("+inf", f32s(&[f32::INFINITY, 1.0])),
            ("-inf", f32s(&[0.0, 0.0, f32::NEG_INFINITY])),
            ("all 0xFF bytes", vec![0xFF; 16]),
        ] {
            let err = check_finite(&safetensors(&[fine(), ("model.encoder.conv1.weight", "F32", bad)])).unwrap_err().to_string();
            assert!(err.contains("`model.encoder.conv1.weight`") && err.contains("NaN or infinity"), "{what}: {err}");
        }
        // The first offender in name order is the one named.
        let both = safetensors(&[("z.last", "F32", f32s(&[f32::NAN])), ("a.first", "F32", f32s(&[f32::INFINITY]))]);
        assert!(check_finite(&both).unwrap_err().to_string().contains("`a.first`"));
    }

    #[test]
    fn half_precision_and_double_weights_are_checked_too() {
        let half = |bits: &[u16]| bits.iter().flat_map(|b| b.to_le_bytes()).collect::<Vec<u8>>();
        // f16: 0x3C00 = 1.0, 0x7BFF = largest finite, 0x7C00 = +inf, 0x7E00 = NaN, 0xFC00 = -inf.
        assert!(check_finite(&safetensors(&[("h", "F16", half(&[0x3C00, 0x7BFF, 0x0001, 0x0000]))])).is_ok());
        for bits in [0x7C00u16, 0x7E00, 0xFC00, 0xFFFF] {
            assert!(check_finite(&safetensors(&[("h", "F16", half(&[0x3C00, bits]))])).is_err(), "f16 {bits:#06x}");
        }
        // bf16: 0x3F80 = 1.0, 0x7F7F = largest finite, 0x7F80 = +inf, 0x7FC0 = NaN, 0xFF80 = -inf.
        assert!(check_finite(&safetensors(&[("b", "BF16", half(&[0x3F80, 0x7F7F, 0x0000]))])).is_ok());
        for bits in [0x7F80u16, 0x7FC0, 0xFF80, 0xFFFF] {
            assert!(check_finite(&safetensors(&[("b", "BF16", half(&[0x3F80, bits]))])).is_err(), "bf16 {bits:#06x}");
        }
        let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        assert!(check_finite(&safetensors(&[("d", "F64", doubles(&[1.0, -1e300]))])).is_ok());
        assert!(check_finite(&safetensors(&[("d", "F64", doubles(&[1.0, f64::NAN]))])).is_err());
        // Integer tensors (token tables) hold any bit pattern.
        assert!(check_finite(&safetensors(&[("i", "I64", vec![0xFF; 16])])).is_ok());
        // Not a safetensors file: an error, not a panic.
        assert!(check_finite(b"not a safetensors file").is_err());
        assert!(check_finite(&[]).is_err());
    }

    #[test]
    fn a_real_shaped_config_validates() {
        tiny().validate().unwrap();
    }

    #[test]
    fn zero_heads_and_indivisible_widths_are_refused() {
        for edit in [
            |c: &mut Config| c.decoder_attention_heads = 0,
            |c: &mut Config| c.encoder_attention_heads = 0,
            |c: &mut Config| c.decoder_attention_heads = 5,
            |c: &mut Config| c.d_model = 0,
            |c: &mut Config| c.d_model = usize::MAX,
            |c: &mut Config| c.num_mel_bins = 0,
            |c: &mut Config| c.encoder_layers = usize::MAX,
            |c: &mut Config| c.max_source_positions = 1499,
            |c: &mut Config| c.max_target_positions = 3,
            |c: &mut Config| c.vocab_size = 0,
            |c: &mut Config| c.vocab_size = usize::MAX,
        ] {
            let mut c = tiny();
            edit(&mut c);
            assert!(c.validate().is_err(), "{c:?}");
        }
    }
}
