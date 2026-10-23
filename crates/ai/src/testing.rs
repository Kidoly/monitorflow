//! A scripted provider for tests of the remediation loop.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{AiError, AiProvider, Step, StepRequest};

/// Returns queued answers in order and records every request.
///
/// Panics when asked for a step with none queued: a test that gets there
/// made more calls than it scripted.
#[derive(Debug, Default)]
pub struct MockProvider {
    answers: Mutex<VecDeque<Result<Step, AiError>>>,
    requests: Mutex<Vec<StepRequest>>,
}

impl MockProvider {
    pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        let mock = Self::default();
        for step in steps {
            mock.push(Ok(step));
        }
        mock
    }

    pub fn push(&self, answer: Result<Step, AiError>) {
        self.answers.lock().unwrap().push_back(answer);
    }

    /// Requests received so far, oldest first.
    pub fn requests(&self) -> Vec<StepRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn remaining(&self) -> usize {
        self.answers.lock().unwrap().len()
    }
}

#[async_trait]
impl AiProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn next_step(&self, request: &StepRequest) -> Result<Step, AiError> {
        self.requests.lock().unwrap().push(request.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("MockProvider has no scripted answer left")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandOutcome, DataBlock, Exchange, ProposedCommand, StopOutcome};

    #[tokio::test]
    async fn answers_in_order_and_records_requests() {
        let mock = MockProvider::new([Step::run("disk full", "df -h", "check usage")]);
        mock.push(Ok(Step::stop(
            "fixed",
            StopOutcome::Resolved,
            "space freed",
        )));
        mock.push(Err(AiError::Truncated));
        let provider: &dyn AiProvider = &mock;
        assert_eq!(provider.name(), "mock");

        let mut request = StepRequest {
            system_prompt: "prompt".into(),
            context: vec![DataBlock {
                title: "logs".into(),
                content: "ENOSPC".into(),
            }],
            history: Vec::new(),
        };
        let first = provider.next_step(&request).await.unwrap();
        assert_eq!(first, Step::run("disk full", "df -h", "check usage"));
        request.history.push(Exchange {
            diagnosis: first.diagnosis,
            proposal: ProposedCommand {
                command: "df -h".into(),
                reasoning: "check usage".into(),
            },
            outcome: CommandOutcome::Executed {
                command: "df -h".into(),
                exit_code: Some(0),
                output: "100%".into(),
            },
        });
        let second = provider.next_step(&request).await.unwrap();
        assert!(matches!(second.action, crate::Action::Stop(_)));
        assert!(matches!(
            provider.next_step(&request).await,
            Err(AiError::Truncated)
        ));
        assert_eq!(mock.remaining(), 0);
        let seen = mock.requests();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].history.is_empty());
        assert_eq!(seen[1].history.len(), 1);
    }

    #[tokio::test]
    #[should_panic(expected = "no scripted answer")]
    async fn panics_when_unscripted() {
        let request = StepRequest {
            system_prompt: String::new(),
            context: Vec::new(),
            history: Vec::new(),
        };
        let _ = MockProvider::default().next_step(&request).await;
    }
}
