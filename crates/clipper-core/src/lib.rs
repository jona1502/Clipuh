//! Platform-independent application lifecycle: state, commands and backend events.
#![forbid(unsafe_code)]

pub mod hotkey;
pub mod pipeline;
pub mod recording;
pub mod storage_estimate;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// Coarse lifecycle state for replay and recording operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Preparing,
    Replaying,
    Recording,
    Error,
}

/// Requests from the application to the platform/backend layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    StartReplay,
    StopReplay,
    SaveReplay,
    StartRecording,
    StopRecording,
    Shutdown,
}

/// Work for an adapter to submit to a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    StartReplay,
    StopReplay,
    SaveReplay,
    StartRecording,
    StopRecording,
    Shutdown,
}

/// Backend reports. Lifecycle states change only after matching completion events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    ReplayStarted,
    ReplayStopped,
    ReplaySaved { path: PathBuf },
    ReplaySaveFailed { message: String },
    RecordingStarted,
    RecordingStopped,
    ShutdownComplete,
    BackendFailed { operation: Command, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    InvalidCommand { state: State, command: Command },
    UnexpectedEvent { state: State, event: Event },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCommand { state, command } => {
                write!(f, "command {command:?} is invalid while {state:?}")
            }
            Self::UnexpectedEvent { state, event } => {
                write!(f, "event {event:?} is unexpected while {state:?}")
            }
        }
    }
}

impl std::error::Error for CoreError {}

/// Pure lifecycle coordinator. It emits work requests and waits for backend events.
///
/// Backend adapters must deliver lifecycle completions once and in order, and
/// cancel or drain old lifecycle work and saves before reporting
/// `ShutdownComplete`. Events do not carry generation IDs in this foundation API.
#[derive(Debug)]
pub struct Core {
    state: State,
    pending: Option<Command>,
    save_pending: bool,
    last_error: Option<String>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            state: State::Idle,
            pending: None,
            save_pending: false,
            last_error: None,
        }
    }
}

impl Core {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Validate a command, update the in-flight state, and return backend work.
    /// No successful state is reported until `handle_event` receives completion.
    pub fn handle_command(&mut self, command: Command) -> Result<Effect, CoreError> {
        let effect = match (self.state, command) {
            (_, Command::Shutdown) => {
                self.state = State::Preparing;
                self.pending = Some(Command::Shutdown);
                Effect::Shutdown
            }
            (State::Idle, Command::StartReplay) => {
                self.state = State::Preparing;
                self.pending = Some(Command::StartReplay);
                Effect::StartReplay
            }
            (State::Replaying, Command::StopReplay) => {
                self.state = State::Preparing;
                self.pending = Some(Command::StopReplay);
                Effect::StopReplay
            }
            (State::Replaying, Command::SaveReplay) if !self.save_pending => {
                self.save_pending = true;
                Effect::SaveReplay
            }
            (State::Idle, Command::StartRecording) => {
                self.state = State::Preparing;
                self.pending = Some(Command::StartRecording);
                Effect::StartRecording
            }
            (State::Recording, Command::StopRecording) => {
                self.state = State::Preparing;
                self.pending = Some(Command::StopRecording);
                Effect::StopRecording
            }
            (state, command) => return Err(CoreError::InvalidCommand { state, command }),
        };

        Ok(effect)
    }

    /// Apply a backend report and return it for the caller to forward to the UI.
    pub fn handle_event(&mut self, event: Event) -> Result<Event, CoreError> {
        let next_state = match &event {
            Event::ReplayStarted if self.pending == Some(Command::StartReplay) => {
                Some(State::Replaying)
            }
            Event::ReplayStopped if self.pending == Some(Command::StopReplay) => Some(State::Idle),
            Event::RecordingStarted if self.pending == Some(Command::StartRecording) => {
                Some(State::Recording)
            }
            Event::RecordingStopped if self.pending == Some(Command::StopRecording) => {
                Some(State::Idle)
            }
            Event::ShutdownComplete if self.pending == Some(Command::Shutdown) => Some(State::Idle),
            Event::ReplaySaved { .. } | Event::ReplaySaveFailed { .. } if self.save_pending => None,
            Event::BackendFailed { operation, .. } if self.pending == Some(*operation) => {
                Some(State::Error)
            }
            _ => {
                return Err(CoreError::UnexpectedEvent {
                    state: self.state,
                    event,
                });
            }
        };

        if let Some(state) = next_state {
            self.state = state;
            match &event {
                Event::BackendFailed { message, .. } => {
                    self.last_error = Some(message.clone());
                    self.pending = None;
                }
                _ => {
                    self.last_error = None;
                    self.pending = None;
                }
            }
            if matches!(&event, Event::ShutdownComplete) {
                self.save_pending = false;
            }
        } else {
            self.save_pending = false;
        }

        Ok(event)
    }
}

/// Submission methods report whether work was accepted. Completion must still be
/// delivered to the core as an [`Event`].
pub trait CaptureBackend {
    type Error;

    fn start_replay(&mut self) -> Result<(), Self::Error>;
    fn stop_replay(&mut self) -> Result<(), Self::Error>;
    fn save_replay(&mut self) -> Result<(), Self::Error>;
}

/// Submission methods report whether work was accepted. Completion must still be
/// delivered to the core as an [`Event`].
pub trait RecordingBackend {
    type Error;

    fn start_recording(&mut self) -> Result<(), Self::Error>;
    fn stop_recording(&mut self) -> Result<(), Self::Error>;
}

/// Create a bounded standard-library channel without starting a worker thread.
pub fn bounded_channel<T>(capacity: usize) -> (SyncSender<T>, Receiver<T>) {
    sync_channel(capacity)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::TrySendError;

    use super::{Command, Core, CoreError, Effect, Event, State, bounded_channel};

    #[test]
    fn replay_changes_state_only_after_backend_completion() {
        let mut core = Core::new();

        assert_eq!(
            core.handle_command(Command::StartReplay),
            Ok(Effect::StartReplay)
        );
        assert_eq!(core.state(), State::Preparing);

        core.handle_event(Event::ReplayStarted).unwrap();
        assert_eq!(core.state(), State::Replaying);
    }

    #[test]
    fn unexpected_completion_does_not_advance_state() {
        let mut core = Core::new();
        core.handle_command(Command::StartReplay).unwrap();

        let result = core.handle_event(Event::RecordingStarted);

        assert!(matches!(result, Err(CoreError::UnexpectedEvent { .. })));
        assert_eq!(core.state(), State::Preparing);
    }

    #[test]
    fn recording_stops_only_after_backend_completion() {
        let mut core = Core::new();
        core.handle_command(Command::StartRecording).unwrap();
        assert_eq!(core.state(), State::Preparing);

        core.handle_event(Event::RecordingStarted).unwrap();
        assert_eq!(core.state(), State::Recording);

        assert_eq!(
            core.handle_command(Command::StopRecording),
            Ok(Effect::StopRecording)
        );
        assert_eq!(core.state(), State::Preparing);

        core.handle_event(Event::RecordingStopped).unwrap();
        assert_eq!(core.state(), State::Idle);
    }

    #[test]
    fn replay_save_is_an_effect_and_does_not_claim_a_state_change() {
        let mut core = Core::new();
        core.handle_command(Command::StartReplay).unwrap();
        core.handle_event(Event::ReplayStarted).unwrap();

        assert_eq!(
            core.handle_command(Command::SaveReplay),
            Ok(Effect::SaveReplay)
        );
        assert_eq!(core.state(), State::Replaying);
        assert!(matches!(
            core.handle_command(Command::SaveReplay),
            Err(CoreError::InvalidCommand { .. })
        ));

        core.handle_event(Event::ReplaySaved {
            path: "clip.mp4".into(),
        })
        .unwrap();
        assert_eq!(
            core.handle_command(Command::SaveReplay),
            Ok(Effect::SaveReplay)
        );
    }

    #[test]
    fn replay_save_failure_does_not_change_the_lifecycle_state() {
        let mut core = Core::new();
        core.handle_command(Command::StartReplay).unwrap();
        core.handle_event(Event::ReplayStarted).unwrap();
        core.handle_command(Command::SaveReplay).unwrap();

        core.handle_event(Event::ReplaySaveFailed {
            message: "disk full".to_owned(),
        })
        .unwrap();

        assert_eq!(core.state(), State::Replaying);
    }

    #[test]
    fn replay_save_result_is_accepted_after_replay_stops() {
        let mut core = Core::new();
        core.handle_command(Command::StartReplay).unwrap();
        core.handle_event(Event::ReplayStarted).unwrap();
        core.handle_command(Command::SaveReplay).unwrap();
        core.handle_command(Command::StopReplay).unwrap();
        core.handle_event(Event::ReplayStopped).unwrap();
        assert_eq!(core.state(), State::Idle);

        core.handle_event(Event::ReplaySaved {
            path: "clip.mp4".into(),
        })
        .unwrap();

        assert_eq!(core.state(), State::Idle);
        assert_eq!(
            core.handle_command(Command::StartReplay),
            Ok(Effect::StartReplay)
        );
    }

    #[test]
    fn backend_failure_enters_error_and_keeps_the_reason() {
        let mut core = Core::new();
        core.handle_command(Command::StartReplay).unwrap();
        core.handle_event(Event::BackendFailed {
            operation: Command::StartReplay,
            message: "device unavailable".to_owned(),
        })
        .unwrap();

        assert_eq!(core.state(), State::Error);
        assert_eq!(core.last_error(), Some("device unavailable"));
    }

    #[test]
    fn bounded_channel_reports_full_without_spawning_a_worker() {
        let (sender, receiver) = bounded_channel(1);
        sender.send(Command::StartReplay).unwrap();

        assert!(matches!(
            sender.try_send(Command::StopReplay),
            Err(TrySendError::Full(Command::StopReplay))
        ));
        assert_eq!(receiver.try_recv(), Ok(Command::StartReplay));
    }
}
