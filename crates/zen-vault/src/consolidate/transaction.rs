use anyhow::Result;
use tracing::info;

/// Transactional scope for consolidation operations.
///
/// Provides begin/commit/rollback lifecycle hooks. Currently
/// stubbed — real transaction management deferred.
pub struct TransactionScope {
    name: String,
}

impl TransactionScope {
    /// Create a new named transaction scope.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }

    /// Begin the transaction.
    pub fn begin(&self) -> Result<()> {
        info!("Transaction begin stub: {}", self.name);
        Ok(())
    }

    /// Commit the transaction.
    pub fn commit(&self) -> Result<()> {
        info!("Transaction commit stub: {}", self.name);
        Ok(())
    }

    /// Rollback the transaction.
    pub fn rollback(&self) -> Result<()> {
        info!("Transaction rollback stub: {}", self.name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_begin_is_stub() {
        let txn = TransactionScope::new("test");
        assert!(txn.begin().is_ok());
    }

    #[test]
    fn test_commit_is_stub() {
        let txn = TransactionScope::new("test");
        assert!(txn.commit().is_ok());
    }

    #[test]
    fn test_rollback_is_stub() {
        let txn = TransactionScope::new("test");
        assert!(txn.rollback().is_ok());
    }
}
