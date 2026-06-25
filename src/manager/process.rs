//! Process spawning and management

mod imp;

use tokio::process::Child;

use crate::units::Service;

pub use imp::{SpawnError, SpawnOptions};

pub fn spawn_service_via_executor(
    service: &Service,
    options: &SpawnOptions,
    executor_path: &str,
    command_index: usize,
) -> Result<Child, SpawnError> {
    imp::spawn_service_via_executor(service, options, executor_path, command_index)
}
