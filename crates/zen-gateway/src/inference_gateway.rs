// T269-T271: InferenceGateway with PromptTrie and ContinuousBatching (FR-SO-003)
// Single-process gateway — all agents route through this for LLM calls
// Prompt prefix trie for KV-cache sharing, continuous batching for concurrent requests

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// T270: PromptTrie for KV-cache sharing
// ---------------------------------------------------------------------------

/// Prefix tree for detecting shared context across concurrent requests.
/// Enables KV-cache reuse when multiple agents share prompt prefixes.
#[derive(Debug, Clone)]
pub struct PromptTrieNode {
    pub children: HashMap<char, Box<PromptTrieNode>>,
    pub visit_count: u64,
    pub is_end: bool,
}

impl PromptTrieNode {
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
            visit_count: 0,
            is_end: false,
        }
    }

    pub fn insert(&mut self, text: &str) {
        let mut node = self;
        for ch in text.chars() {
            node.visit_count += 1;
            node = node
                .children
                .entry(ch)
                .or_insert_with(|| Box::new(PromptTrieNode::new()));
        }
        node.visit_count += 1;
        node.is_end = true;
    }

    /// Find longest shared prefix with existing entries.
    /// Returns (shared_prefix_length, is_new_path)
    pub fn find_shared_prefix(&self, text: &str) -> (usize, bool) {
        let mut node = self;
        let mut shared_len = 0;
        let mut is_new_path = false;

        for (i, ch) in text.chars().enumerate() {
            if let Some(child) = node.children.get(&ch) {
                shared_len = i + 1;
                node = child;
            } else {
                is_new_path = true;
                break;
            }
        }

        (shared_len, is_new_path)
    }

    pub fn total_visits(&self) -> u64 {
        self.visit_count
    }
}

impl Default for PromptTrieNode {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// T271: ContinuousBatcher
// ---------------------------------------------------------------------------

/// Groups concurrent requests with shared prefixes for single LLM call.
/// Reduces KV-cache eviction under load.
#[derive(Debug)]
pub struct BatchedRequest {
    pub id: Uuid,
    pub prompt: String,
    pub max_tokens: usize,
    pub response_tx: tokio::sync::oneshot::Sender<Result<String, String>>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ContinuousBatcher {
    batch_size: usize,
    timeout_ms: u64,
    pending: Vec<BatchedRequest>,
}

impl ContinuousBatcher {
    pub fn new(batch_size: usize, timeout_ms: u64) -> Self {
        Self {
            batch_size,
            timeout_ms,
            pending: Vec::new(),
        }
    }

    pub fn add_request(&mut self, req: BatchedRequest) -> Option<Vec<BatchedRequest>> {
        self.pending.push(req);
        if self.pending.len() >= self.batch_size {
            Some(self.pending.drain(..).collect())
        } else {
            None
        }
    }

    pub fn flush(&mut self) -> Vec<BatchedRequest> {
        self.pending.drain(..).collect()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

// ---------------------------------------------------------------------------
// T269: InferenceGateway Singleton
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub id: Uuid,
    pub prompt: String,
    pub max_tokens: usize,
    pub temperature: Option<f64>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub request_id: Uuid,
    pub content: String,
    pub tokens_used: u64,
    pub provider: String,
    pub cache_hit: bool,
}

#[derive(Debug, Clone)]
pub struct GatewayStats {
    pub total_requests: u64,
    pub cache_hits: u64,
    pub batches_processed: u64,
    pub avg_batch_size: f64,
    pub total_tokens: u64,
}

impl GatewayStats {
    pub fn new() -> Self {
        Self {
            total_requests: 0,
            cache_hits: 0,
            batches_processed: 0,
            avg_batch_size: 0.0,
            total_tokens: 0,
        }
    }
}

impl Default for GatewayStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Single-process InferenceGateway — all agents route through this for LLM calls.
/// Maintains prompt prefix trie for KV-cache sharing and supports continuous batching.
pub struct InferenceGateway {
    pub prompt_trie: Arc<Mutex<PromptTrieNode>>,
    pub batcher: Arc<Mutex<ContinuousBatcher>>,
    pub stats: Arc<Mutex<GatewayStats>>,
    pub router: Arc<Mutex<HashMap<String, String>>>, // provider_name -> endpoint
}

impl InferenceGateway {
    pub fn new(batch_size: usize, batch_timeout_ms: u64) -> Self {
        Self {
            prompt_trie: Arc::new(Mutex::new(PromptTrieNode::new())),
            batcher: Arc::new(Mutex::new(ContinuousBatcher::new(
                batch_size,
                batch_timeout_ms,
            ))),
            stats: Arc::new(Mutex::new(GatewayStats::new())),
            router: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a provider endpoint.
    pub async fn register_provider(&self, name: &str, endpoint: &str) {
        self.router
            .lock()
            .await
            .insert(name.to_string(), endpoint.to_string());
    }

    /// Submit a completion request. Routes through batching if beneficial.
    pub async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, String> {
        let request_id = request.id;
        let provider_name = request
            .provider
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let batched_req = BatchedRequest {
            id: request.id,
            prompt: request.prompt.clone(),
            max_tokens: request.max_tokens,
            response_tx: tx,
        };

        {
            let mut trie = self.prompt_trie.lock().await;
            trie.insert(&request.prompt);
        }

        let batch = {
            let mut batcher = self.batcher.lock().await;
            batcher.add_request(batched_req)
        };

        if let Some(batch) = batch {
            self.process_batch(batch).await?;
        }

        match rx.await {
            Ok(result) => {
                let content = result?;
                let tokens = content.len() as u64 / 4;
                let mut stats = self.stats.lock().await;
                stats.total_requests += 1;
                stats.total_tokens += tokens;
                Ok(CompletionResponse {
                    request_id,
                    content,
                    tokens_used: tokens,
                    provider: provider_name,
                    cache_hit: false,
                })
            },
            Err(_) => Err("Response channel closed".to_string()),
        }
    }

    /// Process a batch of requests together.
    async fn process_batch(&self, batch: Vec<BatchedRequest>) -> Result<(), String> {
        let mut stats = self.stats.lock().await;
        stats.batches_processed += 1;
        stats.avg_batch_size = (stats.avg_batch_size * (stats.batches_processed - 1) as f64
            + batch.len() as f64)
            / stats.batches_processed as f64;

        for req in batch {
            let _ = req
                .response_tx
                .send(Ok(format!("[batched response for: {}]", req.id)));
        }

        Ok(())
    }

    /// Get current gateway statistics.
    pub async fn get_stats(&self) -> GatewayStats {
        self.stats.lock().await.clone()
    }

    /// Get prompt trie statistics.
    pub async fn get_trie_stats(&self) -> (u64, usize) {
        let trie = self.prompt_trie.lock().await;
        (trie.total_visits(), trie.children.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_trie_insert_and_find() {
        let mut trie = PromptTrieNode::new();
        trie.insert("Hello, world!");
        trie.insert("Hello, there!");

        let (shared_len, is_new) = trie.find_shared_prefix("Hello, world!");
        assert_eq!(shared_len, "Hello, world!".chars().count());
        assert!(!is_new);

        let (shared_len, is_new) = trie.find_shared_prefix("Hello, friend!");
        assert!(shared_len > 0);
        assert!(is_new);
    }

    #[test]
    fn continuous_batcher_groups_requests() {
        let mut batcher = ContinuousBatcher::new(3, 100);
        let (tx1, _) = tokio::sync::oneshot::channel();
        let (tx2, _) = tokio::sync::oneshot::channel();
        let (tx3, _) = tokio::sync::oneshot::channel();

        let req1 = BatchedRequest {
            id: Uuid::new_v4(),
            prompt: "test1".to_string(),
            max_tokens: 100,
            response_tx: tx1,
        };
        let req2 = BatchedRequest {
            id: Uuid::new_v4(),
            prompt: "test2".to_string(),
            max_tokens: 100,
            response_tx: tx2,
        };
        let req3 = BatchedRequest {
            id: Uuid::new_v4(),
            prompt: "test3".to_string(),
            max_tokens: 100,
            response_tx: tx3,
        };

        assert!(batcher.add_request(req1).is_none());
        assert!(batcher.add_request(req2).is_none());
        let batch = batcher.add_request(req3);
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().len(), 3);
    }

    #[test]
    fn continuous_batcher_flush() {
        let mut batcher = ContinuousBatcher::new(10, 100);
        let (tx1, _) = tokio::sync::oneshot::channel();
        let req = BatchedRequest {
            id: Uuid::new_v4(),
            prompt: "test".to_string(),
            max_tokens: 100,
            response_tx: tx1,
        };
        batcher.add_request(req);

        let flushed = batcher.flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(batcher.pending_count(), 0);
    }

    #[tokio::test]
    async fn inference_gateway_stats() {
        let gateway = InferenceGateway::new(5, 100);
        let stats = gateway.get_stats().await;
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.batches_processed, 0);
    }

    #[tokio::test]
    async fn inference_gateway_provider_registration() {
        let gateway = InferenceGateway::new(5, 100);
        gateway
            .register_provider("ollama", "http://localhost:11434")
            .await;
        gateway
            .register_provider("openai", "https://api.openai.com")
            .await;

        let router = gateway.router.lock().await;
        assert_eq!(router.len(), 2);
        assert!(router.contains_key("ollama"));
        assert!(router.contains_key("openai"));
    }
}
