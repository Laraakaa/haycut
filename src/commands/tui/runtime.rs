//! Runtime boundary used by the application layer.
//!
//! Keeping the protocol here makes the application independently testable:
//! tests can provide a deterministic runtime without starting an agent worker.

use super::app::{WorkerCommand, WorkerUpdate};

pub(super) trait AgentRuntime {
    fn send(&self, command: WorkerCommand) -> Result<(), String>;
    fn try_recv(&self) -> Result<WorkerUpdate, std::sync::mpsc::TryRecvError>;
    fn shutdown(&mut self);
}
