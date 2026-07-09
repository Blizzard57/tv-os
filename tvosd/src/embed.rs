//! On-box text embeddings (PLAN.md §6): a sentence-transformer (all-MiniLM-L6,
//! 384-dim) run locally via ONNX. One embedding space for games *and* video, so
//! taste transfers across domains. The model is downloaded once into the TV OS
//! data dir and cached; everything else (tokenize → infer → mean-pool →
//! normalize) is handled by fastembed.
//!
//! Loading is slow (and needs the network once), so the daemon warms it on a
//! background thread; until it's ready the recommender falls back to the simple
//! content-based row. All entry points degrade gracefully if the model can't
//! load (offline / unsupported), so playback and the home screen never break.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

static MODEL: OnceLock<Mutex<Option<TextEmbedding>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<TextEmbedding>> {
    MODEL.get_or_init(|| Mutex::new(None))
}

fn cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/tvos/models")
}

/// Loads the model (downloading it the first time). Idempotent; returns whether
/// embeddings are available. Safe to call from a background warmup thread.
pub fn init() -> bool {
    let mut guard = cell().lock().unwrap();
    if guard.is_some() {
        return true;
    }
    let opts = InitOptions::new(EmbeddingModel::AllMiniLML6V2Q)
        .with_cache_dir(cache_dir())
        .with_show_download_progress(false);
    match TextEmbedding::try_new(opts) {
        Ok(model) => {
            *guard = Some(model);
            true
        }
        Err(e) => {
            crate::log_warn!("embeddings unavailable ({e}); using fallback recommender");
            false
        }
    }
}

/// Whether the model is loaded and ready to embed.
pub fn ready() -> bool {
    cell().lock().unwrap().is_some()
}

/// Embeds texts into normalized 384-dim vectors. Returns None if the model
/// isn't loaded or inference fails.
pub fn embed(texts: Vec<String>) -> Option<Vec<Vec<f32>>> {
    let mut guard = cell().lock().unwrap();
    let model = guard.as_mut()?;
    model.embed(texts, None).ok()
}

/// Cosine similarity. fastembed returns normalized vectors, so this is just the
/// dot product, but we normalize defensively.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default (downloads ~25 MB). Run with: cargo test embeds -- --ignored
    #[test]
    #[ignore]
    fn embeds_capture_meaning() {
        assert!(init(), "model should load");
        let v = embed(vec![
            "cat".to_string(),
            "kitten".to_string(),
            "car".to_string(),
        ])
        .unwrap();
        assert_eq!(v[0].len(), 384);
        let cat_kitten = cosine(&v[0], &v[1]);
        let cat_car = cosine(&v[0], &v[2]);
        assert!(cat_kitten > cat_car, "{cat_kitten} !> {cat_car}");
    }
}
