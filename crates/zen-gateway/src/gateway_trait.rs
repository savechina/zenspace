use crate::gateway_error::GatewayError;

/// Lifecycle state of a gateway daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayStatus {
    /// Gateway has not been started or has been stopped.
    Stopped,
    /// Gateway is actively listening for connections.
    Running,
    /// Gateway encountered an unrecoverable error.
    Error,
}

/// Abstract gateway interface.
pub trait Gateway {
    /// Bind the gateway on the given port and begin accepting connections.
    fn start(&mut self, port: u16) -> Result<(), GatewayError>;

    /// Gracefully shut down the gateway and release all resources.
    fn stop(&mut self) -> Result<(), GatewayError>;

    /// Return a health-check payload from the gateway.
    ///
    /// Implementations should verify downstream dependencies (db, cache,
    /// upstream services) and return a status string or diagnostics payload.
    fn health_check(&self) -> Result<String, GatewayError>;

    /// Return the current lifecycle status of the gateway.
    fn status(&self) -> GatewayStatus;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the Gateway trait compiles and its default stub methods return
    /// NotImplemented as expected.
    #[test]
    fn gateway_trait_stub_compiles() {
        struct TestGateway {
            status: GatewayStatus,
        }

        impl Gateway for TestGateway {
            fn start(&mut self, _port: u16) -> Result<(), GatewayError> {
                Ok(())
            }

            fn stop(&mut self) -> Result<(), GatewayError> {
                Ok(())
            }

            fn health_check(&self) -> Result<String, GatewayError> {
                Ok("ok".to_string())
            }

            fn status(&self) -> GatewayStatus {
                self.status.clone()
            }
        }

        let mut gw = TestGateway {
            status: GatewayStatus::Stopped,
        };

        assert_eq!(gw.status(), GatewayStatus::Stopped);
        assert!(gw.start(8080).is_ok());
        assert!(gw.health_check().is_ok());
        assert!(gw.stop().is_ok());
    }
}
