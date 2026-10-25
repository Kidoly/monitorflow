//! Kinds this platform cannot run yet, like package updates on Windows
//! (ADR-0047): refused without running anything, with a reason the API
//! shows, rather than the dispatcher's "unknown kind".

use super::{Handler, Output};

pub struct Unsupported {
    reason: String,
}

impl Unsupported {
    /// `what` names the feature, like `package updates`.
    pub fn new(what: &str, platform: &str) -> Self {
        Self {
            reason: format!("{what} are not supported on {platform} yet"),
        }
    }
}

#[async_trait::async_trait]
impl Handler for Unsupported {
    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected(self.reason.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_with_its_reason() {
        let output = Unsupported::new("package updates", "Windows")
            .run(b"anything")
            .await;
        assert!(output.rejected);
        assert_eq!(
            output.error.as_deref(),
            Some("package updates are not supported on Windows yet")
        );
    }
}
