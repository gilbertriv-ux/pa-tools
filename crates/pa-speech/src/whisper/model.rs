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
            if self.d_model % heads != 0 {
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
        if heads == 0 || d % heads != 0 {
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

impl Model {
    pub fn load(cfg: Config, weights: Vec<u8>) -> Result<Self> {
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
