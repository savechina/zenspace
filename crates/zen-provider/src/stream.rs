use tokio::sync::mpsc;

pub struct StreamResponse {
    pub token_rx: mpsc::UnboundedReceiver<String>,
    pub done_rx: tokio::sync::oneshot::Receiver<Result<(), String>>,
}

impl StreamResponse {
    pub fn new() -> (
        Self,
        mpsc::UnboundedSender<String>,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ) {
        let (token_tx, token_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        (Self { token_rx, done_rx }, token_tx, done_tx)
    }
}
