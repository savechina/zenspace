use std::sync::{Mutex, OnceLock};

use fastembed::{TextEmbedding, TextInitOptions};
use tracing::{info, warn};

static CACHE: OnceLock<Mutex<Option<TextEmbedding>>> = OnceLock::new();

pub fn try_local_embed(text: &str) -> Option<Vec<f32>> {
    let model = CACHE.get_or_init(|| Mutex::new(None));

    {
        let mut guard = model.lock().unwrap();
        if guard.is_none() {
            match TextEmbedding::try_new(TextInitOptions::default()) {
                Ok(embed) => {
                     *guard = Some(embed);
                      info!("LocalModel initialized");
                  }
                  Err(e) => {
                    warn!("LocalModel init failed: {}", e);
                      return None;
                  }
             }
         }
     }

    let input = vec![text.to_string()];
    let mut guard = model.lock().unwrap();
    let embed_inner = match guard.as_mut() {
        Some(e) => e,
        None => return None,
     };

    let vecs = match embed_inner.embed(input, None) {
        Ok(v) => v,
          Err(e) => {
            warn!("LocalModel embed error: {}", e);
             return None;
         }
      };

     Some(vecs.into_iter().flatten().collect())
}
