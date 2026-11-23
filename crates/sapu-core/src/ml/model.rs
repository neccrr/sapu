//! The classifier: multinomial logistic regression over hashed features,
//! trained with stochastic gradient descent.
//!
//! Why this model: it trains in well under a second on a laptop, needs no
//! GPU or Python, and is *interpretable*: each feature has one weight per
//! class, so "why did it say Safe?" is just "which features pushed hardest
//! towards Safe", which the UI shows as the explanation.

use super::features::{Feature, DIM};
use crate::judge::Verdict;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

pub const CLASSES: [Verdict; 4] = [Verdict::Safe, Verdict::Likely, Verdict::Review, Verdict::Keep];
const K: usize = CLASSES.len();

pub fn class_index(v: Verdict) -> usize {
    CLASSES.iter().position(|c| *c == v).unwrap_or(K - 1)
}

#[derive(Debug, Clone)]
pub struct Example {
    pub features: Vec<Feature>,
    pub label: usize,
    /// Importance (user feedback counts more than rule-derived labels).
    pub weight: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub examples: usize,
    pub from_seed: usize,
    pub from_rules: usize,
    pub from_feedback: usize,
    pub per_class: [usize; K],
    /// Accuracy on a 10% held-out split the model never trained on.
    pub holdout_accuracy: f32,
    /// confusion[actual][predicted] on the held-out split.
    pub confusion: [[usize; K]; K],
    pub trained_at: u64,
    pub train_ms: u64,
}

#[derive(Clone)]
pub struct Model {
    /// DIM × K weights, row-major by feature slot.
    w: Vec<f32>,
    pub metrics: Metrics,
    /// Feature names seen in training (for reports; not saved to disk).
    pub vocab: Vec<String>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model").field("metrics", &self.metrics).finish()
    }
}

pub struct TrainConfig {
    pub epochs: usize,
    pub learning_rate: f32,
    pub l2: f32,
    pub holdout: f32,
    pub seed: u64,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self { epochs: 8, learning_rate: 0.3, l2: 1e-5, holdout: 0.1, seed: 42 }
    }
}

/// Tiny deterministic RNG (xorshift) so training is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

fn softmax(z: [f32; K]) -> [f32; K] {
    let m = z.iter().cloned().fold(f32::MIN, f32::max);
    let e = z.map(|x| (x - m).exp());
    let s: f32 = e.iter().sum();
    e.map(|x| x / s)
}

impl Model {
    fn zeros() -> Self {
        Self { w: vec![0.0; DIM * K], metrics: Metrics::default(), vocab: Vec::new() }
    }

    fn logits(&self, f: &[Feature]) -> [f32; K] {
        let mut z = [0.0f32; K];
        for ft in f {
            let row = ft.idx as usize * K;
            for (k, zk) in z.iter_mut().enumerate() {
                *zk += self.w[row + k] * ft.val;
            }
        }
        z
    }

    /// Probability per class (Safe, Likely, Review, Keep).
    pub fn predict(&self, f: &[Feature]) -> [f32; K] {
        softmax(self.logits(f))
    }

    /// Features that pushed hardest towards `class`, strongest first.
    /// Each weight is compared with the average over all classes, so a
    /// feature only counts if it favours this class over the others.
    pub fn explain<'a>(&self, f: &'a [Feature], class: usize) -> Vec<(&'a str, f32)> {
        let mut out: Vec<(&str, f32)> = f
            .iter()
            .filter(|ft| ft.name != "bias")
            .map(|ft| {
                let row = &self.w[ft.idx as usize * K..ft.idx as usize * K + K];
                let mean = row.iter().sum::<f32>() / K as f32;
                (ft.name.as_str(), (row[class] - mean) * ft.val)
            })
            .filter(|(_, c)| *c > 0.0)
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }

    /// The words that most strongly mean `class`, across the training vocabulary.
    pub fn top_features(&self, class: usize, n: usize) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = self
            .vocab
            .iter()
            .filter(|name| *name != "bias")
            .map(|name| {
                let idx = super::features::slot(name) as usize * K;
                let row = &self.w[idx..idx + K];
                let mean = row.iter().sum::<f32>() / K as f32;
                (name.clone(), row[class] - mean)
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out.truncate(n);
        out
    }

    fn fit(examples: &[Example], idx: &[usize], cfg: &TrainConfig, rng: &mut Rng) -> Self {
        let mut m = Self::zeros();
        // Balance classes so a flood of "Safe" cache files doesn't drown the rest.
        let mut counts = [0usize; K];
        for &i in idx {
            counts[examples[i].label] += 1;
        }
        let n = idx.len().max(1) as f32;
        let class_w = counts.map(|c| if c == 0 { 0.0 } else { (n / (K as f32 * c as f32)).clamp(0.2, 5.0) });

        let mut order = idx.to_vec();
        for epoch in 0..cfg.epochs {
            rng.shuffle(&mut order);
            let lr = cfg.learning_rate / (1.0 + epoch as f32 * 0.5);
            for &i in &order {
                let ex = &examples[i];
                let p = m.predict(&ex.features);
                let scale = lr * ex.weight * class_w[ex.label];
                for ft in &ex.features {
                    let row = ft.idx as usize * K;
                    for (k, pk) in p.iter().enumerate() {
                        let target = if k == ex.label { 1.0 } else { 0.0 };
                        let grad = (pk - target) * ft.val + cfg.l2 * m.w[row + k];
                        m.w[row + k] -= scale * grad;
                    }
                }
            }
        }
        m
    }

    /// Train with a held-out evaluation, then refit on everything.
    pub fn train(examples: &[Example], cfg: &TrainConfig) -> Self {
        let started = std::time::Instant::now();
        let mut rng = Rng(cfg.seed.max(1));
        let mut all: Vec<usize> = (0..examples.len()).collect();
        rng.shuffle(&mut all);

        let n_hold = ((examples.len() as f32 * cfg.holdout) as usize).min(examples.len().saturating_sub(1));
        let (hold, train) = all.split_at(n_hold);

        let mut confusion = [[0usize; K]; K];
        let mut correct = 0usize;
        if !hold.is_empty() {
            let m = Self::fit(examples, train, cfg, &mut rng);
            for &i in hold {
                let p = m.predict(&examples[i].features);
                let pred = argmax(&p);
                confusion[examples[i].label][pred] += 1;
                correct += (pred == examples[i].label) as usize;
            }
        }

        let mut model = Self::fit(examples, &all, cfg, &mut rng);
        let mut vocab: Vec<String> = examples.iter().flat_map(|e| e.features.iter().map(|f| f.name.clone())).collect();
        vocab.sort_unstable();
        vocab.dedup();
        model.vocab = vocab;
        let mut per_class = [0usize; K];
        for e in examples {
            per_class[e.label] += 1;
        }
        model.metrics = Metrics {
            examples: examples.len(),
            per_class,
            holdout_accuracy: if hold.is_empty() { 0.0 } else { correct as f32 / hold.len() as f32 },
            confusion,
            trained_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            train_ms: started.elapsed().as_millis() as u64,
            ..Default::default()
        };
        model
    }

    // ── persistence: magic, metrics as JSON, then raw f32 weights ──

    const MAGIC: &'static [u8; 8] = b"SAPUML01";

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let meta = serde_json::to_vec(&self.metrics).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            f.write_all(Self::MAGIC)?;
            f.write_all(&(meta.len() as u32).to_le_bytes())?;
            f.write_all(&meta)?;
            for w in &self.w {
                f.write_all(&w.to_le_bytes())?;
            }
            f.flush()?;
        }
        std::fs::rename(tmp, path)
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != Self::MAGIC {
            return Err(std::io::Error::other("not a Sapu model file"));
        }
        let mut len = [0u8; 4];
        f.read_exact(&mut len)?;
        let mut meta = vec![0u8; u32::from_le_bytes(len) as usize];
        f.read_exact(&mut meta)?;
        let metrics = serde_json::from_slice(&meta).map_err(std::io::Error::other)?;
        let mut raw = vec![0u8; DIM * K * 4];
        f.read_exact(&mut raw)?;
        let w = raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        Ok(Self { w, metrics, vocab: Vec::new() })
    }
}

pub fn argmax(p: &[f32; K]) -> usize {
    p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap_or(0)
}
