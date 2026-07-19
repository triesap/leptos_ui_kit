use std::fmt;
use std::io;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::Arc;

use super::fs::{FsOps, SystemFs};

/// The semantic reason a transaction requests unpredictable bytes.
///
/// Keeping purposes explicit lets deterministic tests exercise collision and
/// failure handling without process-wide environment variables or hidden RNG
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum EntropyPurpose {
    TransactionId,
    #[cfg(test)]
    LockBootstrapCandidate,
    #[cfg(test)]
    IgnoreBootstrapCandidate,
    #[cfg(test)]
    CapabilityProbeCandidate,
}

impl fmt::Display for EntropyPurpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TransactionId => "transaction identifier",
            #[cfg(test)]
            Self::LockBootstrapCandidate => "write-lock bootstrap candidate",
            #[cfg(test)]
            Self::IgnoreBootstrapCandidate => "coordination-ignore bootstrap candidate",
            #[cfg(test)]
            Self::CapabilityProbeCandidate => "filesystem-capability probe candidate",
        })
    }
}

pub(crate) trait EntropySource:
    fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe
{
    fn fill(&self, purpose: EntropyPurpose, destination: &mut [u8]) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct SystemEntropy;

impl EntropySource for SystemEntropy {
    fn fill(&self, purpose: EntropyPurpose, destination: &mut [u8]) -> io::Result<()> {
        getrandom::fill(destination).map_err(|error| {
            io::Error::other(format!("could not generate {purpose} entropy: {error}"))
        })
    }
}

/// Whether an observer is immediately before or durably after a semantic
/// transition. An `After` event must only be emitted after the operation and
/// its required data/parent-directory durability barriers have succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TransitionWindow {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TransactionOutcome {
    Commit,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum JournalRecordKind {
    Published,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RollbackAction {
    RemoveCreatedTarget,
    RestoreBackup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CleanupObjectKind {
    OwnedStage,
    PlacedStage,
    OwnedBackup,
    PlacedBackup,
    CreatedDirectory,
    OwnedDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PreparationArtifactKind {
    Directory,
    Stage,
    Backup,
}

/// Stable semantic observation points for the transaction crash-window
/// matrix. These keys intentionally describe durable protocol transitions,
/// not filesystem call names or source-code locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TransitionKey {
    BootstrapWorkspace {
        window: TransitionWindow,
    },
    PublishWorkspaceOwnership {
        window: TransitionWindow,
    },
    PrepareJournalPartial {
        sequence: u64,
        window: TransitionWindow,
    },
    PublishJournalRecord {
        sequence: u64,
        window: TransitionWindow,
    },
    OwnerPrepared {
        artifact: PreparationArtifactKind,
        ordinal: u32,
        window: TransitionWindow,
    },
    Placement {
        artifact: PreparationArtifactKind,
        ordinal: u32,
        window: TransitionWindow,
    },
    CancelPlacement {
        artifact: PreparationArtifactKind,
        ordinal: u32,
        window: TransitionWindow,
    },
    ReplaceTarget {
        ordinal: u32,
        window: TransitionWindow,
    },
    RollbackTarget {
        action: RollbackAction,
        ordinal: u32,
        window: TransitionWindow,
    },
    /// Publishing the first immutable journal record whose phase is
    /// `CommitComplete` is the only desired-state irreversible boundary. This
    /// key is used in place of `PublishJournalRecord` for that record.
    CommitBoundary {
        sequence: u64,
        window: TransitionWindow,
    },
    CleanupObject {
        outcome: TransactionOutcome,
        kind: CleanupObjectKind,
        ordinal: u32,
        window: TransitionWindow,
    },
    PublishFinalizationLease {
        outcome: TransactionOutcome,
        generation: u64,
        window: TransitionWindow,
    },
    PublishFinalizationProgress {
        outcome: TransactionOutcome,
        generation: u64,
        window: TransitionWindow,
    },
    RemoveWorkspaceOwnership {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
    RemoveJournalHistory {
        outcome: TransactionOutcome,
        kind: JournalRecordKind,
        sequence: u64,
        window: TransitionWindow,
    },
    RemoveTransactionWorkspace {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
    RemoveFinalizationLease {
        outcome: TransactionOutcome,
        generation: u64,
        window: TransitionWindow,
    },
    ArmRetirementAuthority {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
    MoveTransactionNamespaceToRetirement {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
    RetireTransactionNamespace {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
    RemoveRetirementAuthority {
        outcome: TransactionOutcome,
        window: TransitionWindow,
    },
}

pub(crate) trait TransitionObserver:
    fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe
{
    /// Observes a semantic boundary. Test implementations may block here, but
    /// production observation must not alter transaction behavior.
    fn observe(&self, key: TransitionKey);
}

#[derive(Debug, Default)]
pub(crate) struct NoopTransitionObserver;

impl TransitionObserver for NoopTransitionObserver {
    fn observe(&self, _key: TransitionKey) {}
}

/// Explicit dependencies for one transaction execution.
///
/// Owning all seams in one immutable, cloneable value avoids process-global
/// fault switches and makes entropy/transition behavior local to the command
/// under test.
#[derive(Debug, Clone)]
pub(crate) struct TransactionRuntime {
    fs: Arc<dyn FsOps>,
    entropy: Arc<dyn EntropySource>,
    transition_observer: Arc<dyn TransitionObserver>,
}

impl TransactionRuntime {
    pub(crate) fn system() -> Self {
        Self::new(
            Arc::new(SystemFs),
            Arc::new(SystemEntropy),
            Arc::new(NoopTransitionObserver),
        )
    }

    pub(crate) fn new(
        fs: Arc<dyn FsOps>,
        entropy: Arc<dyn EntropySource>,
        transition_observer: Arc<dyn TransitionObserver>,
    ) -> Self {
        Self {
            fs,
            entropy,
            transition_observer,
        }
    }

    pub(crate) fn fs(&self) -> &dyn FsOps {
        self.fs.as_ref()
    }

    pub(crate) fn fill_entropy(
        &self,
        purpose: EntropyPurpose,
        destination: &mut [u8],
    ) -> io::Result<()> {
        self.entropy.fill(purpose, destination)
    }

    pub(crate) fn observe(&self, key: TransitionKey) {
        #[cfg(test)]
        self.fs.observe_transition(key);
        self.transition_observer.observe(key);
    }
}

impl Default for TransactionRuntime {
    fn default() -> Self {
        Self::system()
    }
}

#[cfg(test)]
mod test_support {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct EntropyRequest {
        pub purpose: EntropyPurpose,
        pub byte_len: usize,
    }

    #[derive(Debug)]
    enum QueuedEntropy {
        Bytes {
            purpose: EntropyPurpose,
            bytes: Vec<u8>,
        },
        Failure {
            purpose: EntropyPurpose,
            kind: io::ErrorKind,
            message: String,
        },
    }

    /// A per-runtime FIFO entropy source. Every entry is purpose-bound and is
    /// consumed by exactly one matching request.
    #[derive(Debug, Default)]
    pub(crate) struct DeterministicEntropy {
        queue: Mutex<VecDeque<QueuedEntropy>>,
        requests: Mutex<Vec<EntropyRequest>>,
    }

    impl DeterministicEntropy {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn queue_bytes(
            &mut self,
            purpose: EntropyPurpose,
            bytes: impl Into<Vec<u8>>,
        ) -> &mut Self {
            self.queue
                .get_mut()
                .expect("entropy queue lock")
                .push_back(QueuedEntropy::Bytes {
                    purpose,
                    bytes: bytes.into(),
                });
            self
        }

        /// Queues the same bytes repeatedly so exclusive-create retry paths
        /// encounter deterministic name collisions.
        pub(crate) fn queue_collisions(
            &mut self,
            purpose: EntropyPurpose,
            bytes: impl Into<Vec<u8>>,
            repetitions: usize,
        ) -> &mut Self {
            assert!(repetitions >= 2, "a collision queue requires two attempts");
            let bytes = bytes.into();
            for _ in 0..repetitions {
                self.queue_bytes(purpose, bytes.clone());
            }
            self
        }

        pub(crate) fn queue_failure(
            &mut self,
            purpose: EntropyPurpose,
            kind: io::ErrorKind,
            message: impl Into<String>,
        ) -> &mut Self {
            self.queue
                .get_mut()
                .expect("entropy queue lock")
                .push_back(QueuedEntropy::Failure {
                    purpose,
                    kind,
                    message: message.into(),
                });
            self
        }

        pub(crate) fn remaining(&self) -> usize {
            self.queue.lock().expect("entropy queue lock").len()
        }

        pub(crate) fn requests(&self) -> Vec<EntropyRequest> {
            self.requests.lock().expect("entropy request lock").clone()
        }
    }

    impl EntropySource for DeterministicEntropy {
        fn fill(&self, purpose: EntropyPurpose, destination: &mut [u8]) -> io::Result<()> {
            self.requests
                .lock()
                .expect("entropy request lock")
                .push(EntropyRequest {
                    purpose,
                    byte_len: destination.len(),
                });

            let mut queue = self.queue.lock().expect("entropy queue lock");
            let Some(next) = queue.front() else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("no queued entropy remains for {purpose}"),
                ));
            };
            let queued_purpose = match next {
                QueuedEntropy::Bytes { purpose, .. } | QueuedEntropy::Failure { purpose, .. } => {
                    *purpose
                }
            };
            if queued_purpose != purpose {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "queued entropy is for {queued_purpose}, but the request is for {purpose}"
                    ),
                ));
            }
            if let QueuedEntropy::Bytes { bytes, .. } = next
                && bytes.len() != destination.len()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "queued {purpose} entropy has {} bytes, but {} were requested",
                        bytes.len(),
                        destination.len()
                    ),
                ));
            }

            match queue.pop_front().expect("front entry exists") {
                QueuedEntropy::Bytes { bytes, .. } => {
                    destination.copy_from_slice(&bytes);
                    Ok(())
                }
                QueuedEntropy::Failure { kind, message, .. } => Err(io::Error::new(kind, message)),
            }
        }
    }

    #[derive(Debug, Default)]
    pub(crate) struct RecordingTransitionObserver {
        events: Mutex<Vec<TransitionKey>>,
        changed: Condvar,
    }

    impl RecordingTransitionObserver {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn events(&self) -> Vec<TransitionKey> {
            self.events.lock().expect("transition event lock").clone()
        }

        /// Waits for an exact number of observations using a condition
        /// variable. There is no sleep loop, timeout, or filesystem sentinel.
        pub(crate) fn wait_for_count(&self, count: usize) -> Vec<TransitionKey> {
            let mut events = self.events.lock().expect("transition event lock");
            while events.len() < count {
                events = self.changed.wait(events).expect("transition event lock");
            }
            events.clone()
        }
    }

    impl TransitionObserver for RecordingTransitionObserver {
        fn observe(&self, key: TransitionKey) {
            self.events.lock().expect("transition event lock").push(key);
            self.changed.notify_all();
        }
    }

    #[derive(Debug)]
    struct BlockingState {
        matching_observations: usize,
        blocked: bool,
        released: bool,
        events: Vec<TransitionKey>,
    }

    /// Records every event and blocks exactly one configured semantic event.
    /// A controller waits/releases through a condition variable, so crash
    /// window tests need neither clock-based polling nor environment switches.
    #[derive(Debug)]
    pub(crate) struct BlockingTransitionObserver {
        key: TransitionKey,
        occurrence: usize,
        state: Mutex<BlockingState>,
        changed: Condvar,
    }

    impl BlockingTransitionObserver {
        pub(crate) fn new(key: TransitionKey) -> Self {
            Self::on_occurrence(key, 1)
        }

        pub(crate) fn on_occurrence(key: TransitionKey, occurrence: usize) -> Self {
            assert!(occurrence > 0, "transition occurrence is one-based");
            Self {
                key,
                occurrence,
                state: Mutex::new(BlockingState {
                    matching_observations: 0,
                    blocked: false,
                    released: false,
                    events: Vec::new(),
                }),
                changed: Condvar::new(),
            }
        }

        pub(crate) fn wait_until_blocked(&self) {
            let mut state = self.state.lock().expect("blocking transition lock");
            while !state.blocked {
                state = self.changed.wait(state).expect("blocking transition lock");
            }
        }

        pub(crate) fn release(&self) {
            let mut state = self.state.lock().expect("blocking transition lock");
            state.released = true;
            self.changed.notify_all();
        }

        pub(crate) fn events(&self) -> Vec<TransitionKey> {
            self.state
                .lock()
                .expect("blocking transition lock")
                .events
                .clone()
        }
    }

    impl TransitionObserver for BlockingTransitionObserver {
        fn observe(&self, key: TransitionKey) {
            let mut state = self.state.lock().expect("blocking transition lock");
            state.events.push(key);
            if key == self.key {
                state.matching_observations += 1;
                if state.matching_observations == self.occurrence {
                    state.blocked = true;
                    self.changed.notify_all();
                    while !state.released {
                        state = self.changed.wait(state).expect("blocking transition lock");
                    }
                }
            }
            self.changed.notify_all();
        }
    }
}

#[cfg(test)]
pub(crate) use test_support::{
    BlockingTransitionObserver, DeterministicEntropy, EntropyRequest, RecordingTransitionObserver,
};

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn deterministic_entropy_is_fifo_and_purpose_bound() {
        let mut entropy = DeterministicEntropy::new();
        entropy
            .queue_bytes(EntropyPurpose::TransactionId, [1, 2, 3, 4])
            .queue_bytes(EntropyPurpose::CapabilityProbeCandidate, [5, 6]);

        let mut transaction_id = [0; 4];
        entropy
            .fill(EntropyPurpose::TransactionId, &mut transaction_id)
            .expect("transaction entropy");
        let mut probe = [0; 2];
        entropy
            .fill(EntropyPurpose::CapabilityProbeCandidate, &mut probe)
            .expect("capability-probe entropy");

        assert_eq!(transaction_id, [1, 2, 3, 4]);
        assert_eq!(probe, [5, 6]);
        assert_eq!(entropy.remaining(), 0);
        assert_eq!(
            entropy.requests(),
            vec![
                EntropyRequest {
                    purpose: EntropyPurpose::TransactionId,
                    byte_len: 4,
                },
                EntropyRequest {
                    purpose: EntropyPurpose::CapabilityProbeCandidate,
                    byte_len: 2,
                },
            ]
        );
    }

    #[test]
    fn deterministic_entropy_repeats_collisions_and_surfaces_failures() {
        let mut entropy = DeterministicEntropy::new();
        entropy
            .queue_collisions(EntropyPurpose::LockBootstrapCandidate, [0xaa, 0xbb], 2)
            .queue_failure(
                EntropyPurpose::LockBootstrapCandidate,
                io::ErrorKind::Other,
                "injected entropy failure",
            );

        for _ in 0..2 {
            let mut bytes = [0; 2];
            entropy
                .fill(EntropyPurpose::LockBootstrapCandidate, &mut bytes)
                .expect("collision bytes");
            assert_eq!(bytes, [0xaa, 0xbb]);
        }
        let error = entropy
            .fill(EntropyPurpose::LockBootstrapCandidate, &mut [0; 2])
            .expect_err("queued failure");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "injected entropy failure");
        assert_eq!(entropy.remaining(), 0);
    }

    #[test]
    fn entropy_mismatch_does_not_consume_the_queue() {
        let mut entropy = DeterministicEntropy::new();
        entropy.queue_bytes(EntropyPurpose::IgnoreBootstrapCandidate, [1, 2]);

        let purpose_error = entropy
            .fill(EntropyPurpose::CapabilityProbeCandidate, &mut [0; 2])
            .expect_err("purpose mismatch");
        assert_eq!(purpose_error.kind(), io::ErrorKind::InvalidInput);
        let length_error = entropy
            .fill(EntropyPurpose::IgnoreBootstrapCandidate, &mut [0; 1])
            .expect_err("length mismatch");
        assert_eq!(length_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(entropy.remaining(), 1);

        let mut bytes = [0; 2];
        entropy
            .fill(EntropyPurpose::IgnoreBootstrapCandidate, &mut bytes)
            .expect("matching request");
        assert_eq!(bytes, [1, 2]);
        assert_eq!(
            entropy
                .fill(EntropyPurpose::IgnoreBootstrapCandidate, &mut bytes)
                .expect_err("exhausted queue")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn recording_observer_preserves_semantic_order() {
        let observer = RecordingTransitionObserver::new();
        let events = [
            TransitionKey::PrepareJournalPartial {
                sequence: 0,
                window: TransitionWindow::Before,
            },
            TransitionKey::PrepareJournalPartial {
                sequence: 0,
                window: TransitionWindow::After,
            },
            TransitionKey::PublishJournalRecord {
                sequence: 0,
                window: TransitionWindow::Before,
            },
            TransitionKey::PublishJournalRecord {
                sequence: 0,
                window: TransitionWindow::After,
            },
        ];
        for event in events {
            observer.observe(event);
        }
        assert_eq!(observer.wait_for_count(events.len()), events);
        assert_eq!(observer.events(), events);
    }

    #[test]
    fn blocking_observer_uses_an_explicit_condition_variable_barrier() {
        let blocked_key = TransitionKey::ReplaceTarget {
            ordinal: 3,
            window: TransitionWindow::After,
        };
        let observer = Arc::new(BlockingTransitionObserver::new(blocked_key));
        let worker_observer = Arc::clone(&observer);
        let worker = thread::spawn(move || {
            worker_observer.observe(TransitionKey::ReplaceTarget {
                ordinal: 3,
                window: TransitionWindow::Before,
            });
            worker_observer.observe(blocked_key);
            worker_observer.observe(TransitionKey::PrepareJournalPartial {
                sequence: 9,
                window: TransitionWindow::Before,
            });
        });

        observer.wait_until_blocked();
        assert_eq!(
            observer.events(),
            vec![
                TransitionKey::ReplaceTarget {
                    ordinal: 3,
                    window: TransitionWindow::Before,
                },
                blocked_key,
            ]
        );
        observer.release();
        worker.join().expect("observer worker");
        assert_eq!(observer.events().len(), 3);
    }

    #[test]
    fn blocking_observer_selects_an_exact_repeated_occurrence() {
        let blocked_key = TransitionKey::PublishJournalRecord {
            sequence: 4,
            window: TransitionWindow::Before,
        };
        let nonmatching_window = TransitionKey::PublishJournalRecord {
            sequence: 4,
            window: TransitionWindow::After,
        };
        let observer = Arc::new(BlockingTransitionObserver::on_occurrence(blocked_key, 2));
        let worker_observer = Arc::clone(&observer);
        let worker = thread::spawn(move || {
            worker_observer.observe(blocked_key);
            worker_observer.observe(nonmatching_window);
            worker_observer.observe(blocked_key);
            worker_observer.observe(nonmatching_window);
        });

        observer.wait_until_blocked();
        assert_eq!(
            observer.events(),
            vec![blocked_key, nonmatching_window, blocked_key]
        );
        observer.release();
        worker.join().expect("observer worker");
        assert_eq!(
            observer.events(),
            vec![
                blocked_key,
                nonmatching_window,
                blocked_key,
                nonmatching_window,
            ]
        );
    }

    #[test]
    fn runtime_routes_explicit_entropy_and_transition_seams() {
        let mut entropy = DeterministicEntropy::new();
        entropy.queue_bytes(EntropyPurpose::TransactionId, [9, 8, 7, 6]);
        let entropy = Arc::new(entropy);
        let observer = Arc::new(RecordingTransitionObserver::new());
        let runtime =
            TransactionRuntime::new(Arc::new(SystemFs), entropy.clone(), observer.clone());

        let mut bytes = [0; 4];
        runtime
            .fill_entropy(EntropyPurpose::TransactionId, &mut bytes)
            .expect("runtime entropy");
        let key = TransitionKey::CommitBoundary {
            sequence: 7,
            window: TransitionWindow::After,
        };
        runtime.observe(key);

        assert_eq!(bytes, [9, 8, 7, 6]);
        assert_eq!(observer.events(), vec![key]);
        assert_eq!(Arc::strong_count(&entropy), 2);
        assert_eq!(Arc::strong_count(&observer), 2);
        let _ = runtime.fs();
    }

    fn at_window(key: TransitionKey, window: TransitionWindow) -> TransitionKey {
        match key {
            TransitionKey::BootstrapWorkspace { .. } => {
                TransitionKey::BootstrapWorkspace { window }
            }
            TransitionKey::PublishWorkspaceOwnership { .. } => {
                TransitionKey::PublishWorkspaceOwnership { window }
            }
            TransitionKey::PrepareJournalPartial { sequence, .. } => {
                TransitionKey::PrepareJournalPartial { sequence, window }
            }
            TransitionKey::PublishJournalRecord { sequence, .. } => {
                TransitionKey::PublishJournalRecord { sequence, window }
            }
            TransitionKey::OwnerPrepared {
                artifact, ordinal, ..
            } => TransitionKey::OwnerPrepared {
                artifact,
                ordinal,
                window,
            },
            TransitionKey::Placement {
                artifact, ordinal, ..
            } => TransitionKey::Placement {
                artifact,
                ordinal,
                window,
            },
            TransitionKey::CancelPlacement {
                artifact, ordinal, ..
            } => TransitionKey::CancelPlacement {
                artifact,
                ordinal,
                window,
            },
            TransitionKey::ReplaceTarget { ordinal, .. } => {
                TransitionKey::ReplaceTarget { ordinal, window }
            }
            TransitionKey::RollbackTarget {
                action, ordinal, ..
            } => TransitionKey::RollbackTarget {
                action,
                ordinal,
                window,
            },
            TransitionKey::CommitBoundary { sequence, .. } => {
                TransitionKey::CommitBoundary { sequence, window }
            }
            TransitionKey::CleanupObject {
                outcome,
                kind,
                ordinal,
                ..
            } => TransitionKey::CleanupObject {
                outcome,
                kind,
                ordinal,
                window,
            },
            TransitionKey::PublishFinalizationLease {
                outcome,
                generation,
                ..
            } => TransitionKey::PublishFinalizationLease {
                outcome,
                generation,
                window,
            },
            TransitionKey::PublishFinalizationProgress {
                outcome,
                generation,
                ..
            } => TransitionKey::PublishFinalizationProgress {
                outcome,
                generation,
                window,
            },
            TransitionKey::RemoveWorkspaceOwnership { outcome, .. } => {
                TransitionKey::RemoveWorkspaceOwnership { outcome, window }
            }
            TransitionKey::RemoveJournalHistory {
                outcome,
                kind,
                sequence,
                ..
            } => TransitionKey::RemoveJournalHistory {
                outcome,
                kind,
                sequence,
                window,
            },
            TransitionKey::RemoveTransactionWorkspace { outcome, .. } => {
                TransitionKey::RemoveTransactionWorkspace { outcome, window }
            }
            TransitionKey::RemoveFinalizationLease {
                outcome,
                generation,
                ..
            } => TransitionKey::RemoveFinalizationLease {
                outcome,
                generation,
                window,
            },
            TransitionKey::ArmRetirementAuthority { outcome, .. } => {
                TransitionKey::ArmRetirementAuthority { outcome, window }
            }
            TransitionKey::MoveTransactionNamespaceToRetirement { outcome, .. } => {
                TransitionKey::MoveTransactionNamespaceToRetirement { outcome, window }
            }
            TransitionKey::RetireTransactionNamespace { outcome, .. } => {
                TransitionKey::RetireTransactionNamespace { outcome, window }
            }
            TransitionKey::RemoveRetirementAuthority { outcome, .. } => {
                TransitionKey::RemoveRetirementAuthority { outcome, window }
            }
        }
    }

    fn protocol_transition_points() -> Vec<TransitionKey> {
        let before = TransitionWindow::Before;
        let mut points = vec![
            TransitionKey::BootstrapWorkspace { window: before },
            TransitionKey::PublishWorkspaceOwnership { window: before },
            TransitionKey::ArmRetirementAuthority {
                outcome: TransactionOutcome::Commit,
                window: before,
            },
            TransitionKey::MoveTransactionNamespaceToRetirement {
                outcome: TransactionOutcome::Commit,
                window: before,
            },
            TransitionKey::RetireTransactionNamespace {
                outcome: TransactionOutcome::Commit,
                window: before,
            },
            TransitionKey::RemoveRetirementAuthority {
                outcome: TransactionOutcome::Commit,
                window: before,
            },
            TransitionKey::PrepareJournalPartial {
                sequence: 0,
                window: before,
            },
            TransitionKey::PublishJournalRecord {
                sequence: 0,
                window: before,
            },
            TransitionKey::OwnerPrepared {
                artifact: PreparationArtifactKind::Directory,
                ordinal: 1,
                window: before,
            },
            TransitionKey::Placement {
                artifact: PreparationArtifactKind::Directory,
                ordinal: 1,
                window: before,
            },
            TransitionKey::CancelPlacement {
                artifact: PreparationArtifactKind::Directory,
                ordinal: 1,
                window: before,
            },
            TransitionKey::OwnerPrepared {
                artifact: PreparationArtifactKind::Stage,
                ordinal: 2,
                window: before,
            },
            TransitionKey::Placement {
                artifact: PreparationArtifactKind::Stage,
                ordinal: 2,
                window: before,
            },
            TransitionKey::OwnerPrepared {
                artifact: PreparationArtifactKind::Backup,
                ordinal: 2,
                window: before,
            },
            TransitionKey::Placement {
                artifact: PreparationArtifactKind::Backup,
                ordinal: 2,
                window: before,
            },
            TransitionKey::ReplaceTarget {
                ordinal: 2,
                window: before,
            },
            TransitionKey::RollbackTarget {
                action: RollbackAction::RemoveCreatedTarget,
                ordinal: 3,
                window: before,
            },
            TransitionKey::RollbackTarget {
                action: RollbackAction::RestoreBackup,
                ordinal: 4,
                window: before,
            },
            TransitionKey::CommitBoundary {
                sequence: 5,
                window: before,
            },
        ];

        for outcome in [TransactionOutcome::Commit, TransactionOutcome::Rollback] {
            for (kind, ordinal) in [
                (CleanupObjectKind::OwnedStage, 6),
                (CleanupObjectKind::PlacedStage, 7),
                (CleanupObjectKind::OwnedBackup, 8),
                (CleanupObjectKind::PlacedBackup, 9),
                (CleanupObjectKind::CreatedDirectory, 10),
                (CleanupObjectKind::OwnedDirectory, 11),
            ] {
                points.push(TransitionKey::CleanupObject {
                    outcome,
                    kind,
                    ordinal,
                    window: before,
                });
            }
            points.push(TransitionKey::PublishFinalizationLease {
                outcome,
                generation: 0,
                window: before,
            });
            points.push(TransitionKey::RemoveWorkspaceOwnership {
                outcome,
                window: before,
            });
            for (kind, sequence) in [
                (JournalRecordKind::Published, 10),
                (JournalRecordKind::Partial, 11),
            ] {
                points.push(TransitionKey::RemoveJournalHistory {
                    outcome,
                    kind,
                    sequence,
                    window: before,
                });
            }
            points.push(TransitionKey::RemoveTransactionWorkspace {
                outcome,
                window: before,
            });
            points.push(TransitionKey::PublishFinalizationProgress {
                outcome,
                generation: 1,
                window: before,
            });
            points.push(TransitionKey::RemoveFinalizationLease {
                outcome,
                generation: 1,
                window: before,
            });
        }

        points
    }

    #[test]
    fn transition_key_surface_covers_every_protocol_mutation_before_and_after() {
        let points = protocol_transition_points();
        assert_eq!(points.len(), 45);

        let expected = points
            .into_iter()
            .flat_map(|key| {
                [
                    at_window(key, TransitionWindow::Before),
                    at_window(key, TransitionWindow::After),
                ]
            })
            .collect::<Vec<_>>();
        let observer = RecordingTransitionObserver::new();
        for key in expected.iter().copied() {
            observer.observe(key);
        }

        assert_eq!(observer.wait_for_count(expected.len()), expected);
        assert_eq!(observer.events(), expected);
        assert_eq!(expected.len(), 90);
        for pair in expected.chunks_exact(2) {
            assert_eq!(pair[0], at_window(pair[0], TransitionWindow::Before));
            assert_eq!(pair[1], at_window(pair[0], TransitionWindow::After));
        }
    }
}
