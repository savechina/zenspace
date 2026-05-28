pub struct CronScheduler;

impl Default for CronScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl CronScheduler {
    pub fn new() -> Self {
        Self
    }

    pub fn schedule(&self, expression: &str) {
        tracing::info!("Cron scheduler stub: expression={expression}");
    }
}
