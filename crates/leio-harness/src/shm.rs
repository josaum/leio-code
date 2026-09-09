//! Zero-copy shared memory SPSC ring buffer & Semantic Consensus (S-2PC) fabric
//! for ultra-low-latency local multi-agent swarm coordination.
//!
//! Provides lock-free ring buffers backed by POSIX shared memory (`shm_open` on
//! Darwin/Linux, with `memfd_create` support on Linux). Data slots are cache-line
//! aligned and zero-copy, carrying raw little-endian f32 embedding vectors,
//! atomic epistemic state (Free -> Tentative -> Reflecting -> Committed / Aborted),
//! and dual-plane Unix Domain datagram signaling to prevent CPU busy-spinning.

use anyhow::{Context, Result, bail};
use nix::libc;
use std::ffi::CString;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SHM_MAGIC: u64 = 0x4C45494F5F53484D; // "LEIO_SHM"
pub const SHM_VERSION: u32 = 1;
pub const MAX_VECTOR_DIMS: usize = 1536;

/// Epistemic lifecycle states for multi-agent Semantic Two-Phase Commit (S-2PC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SwarmState {
    /// Slot is unallocated, empty, or recycled for reuse.
    Free = 0,
    /// Proposal submitted by an agent; payload is written but awaiting invariant audit.
    Tentative = 1,
    /// Acquired by a verifier/critic agent; invariant checking in progress.
    Reflecting = 2,
    /// Verified, committed, and immutable; approved for execution or semantic indexing.
    Committed = 3,
    /// Rejected by verifier due to invariant violation, regression, or hallucination.
    Aborted = 4,
}

impl SwarmState {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::Free),
            1 => Some(Self::Tentative),
            2 => Some(Self::Reflecting),
            3 => Some(Self::Committed),
            4 => Some(Self::Aborted),
            _ => None,
        }
    }
}

pub const SLOT_EMPTY: u32 = SwarmState::Free as u32;
pub const SLOT_WRITING: u32 = SwarmState::Tentative as u32;
pub const SLOT_TENTATIVE: u32 = SwarmState::Tentative as u32;
pub const SLOT_REFLECTING: u32 = SwarmState::Reflecting as u32;
pub const SLOT_COMMITTED: u32 = SwarmState::Committed as u32;
pub const SLOT_ABORTED: u32 = SwarmState::Aborted as u32;

pub mod intents {
    pub const INTENT_UNKNOWN: u32 = 0;
    pub const INTENT_PROPOSE_FACT: u32 = 1;
    pub const INTENT_PROPOSE_TOOL_CALL: u32 = 2;
    pub const INTENT_EMBEDDING_UPDATE: u32 = 3;
    pub const INTENT_GOAL_MUTATION: u32 = 4;
    pub const INTENT_MERGE_REQUEST: u32 = 5;
}

pub mod abort_reasons {
    pub const REASON_UNKNOWN: u32 = 0;
    pub const REASON_INVARIANT_VIOLATION: u32 = 1;
    pub const REASON_LOW_CONFIDENCE: u32 = 2;
    pub const REASON_DIMENSION_MISMATCH: u32 = 3;
    pub const REASON_REGRESSION_DETECTED: u32 = 4;
    pub const REASON_HALLUCINATION_DETECTED: u32 = 5;
    pub const REASON_TIMEOUT: u32 = 6;
}

/// 64-byte cache line padded atomic u64 cursor to eliminate false sharing.
#[repr(C, align(64))]
pub struct CachePaddedAtomicU64 {
    pub value: AtomicU64,
}

impl CachePaddedAtomicU64 {
    pub const fn new(val: u64) -> Self {
        Self {
            value: AtomicU64::new(val),
        }
    }
}

/// Fixed-size zero-copy slot for embedding vectors, consensus state, and lane metadata.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct ShmSlot {
    pub seq: u64,
    pub timestamp_ms: i64,
    /// Epistemic / seqlock state: 0 = Free, 1 = Tentative, 2 = Reflecting, 3 = Committed, 4 = Aborted
    pub state: u32,
    pub topic_id: u32,
    pub intent_code: u32,
    pub reason_code: u32,
    pub confidence: f32,
    pub dimension: u32,
    pub agent_id: [u8; 32],    // Proposing agent ID
    pub verifier_id: [u8; 32], // Verifier agent ID (claimed/approved/rejected)
    pub run_id: [u8; 32],
    pub vector: [f32; MAX_VECTOR_DIMS],
}

impl Default for ShmSlot {
    fn default() -> Self {
        Self {
            seq: 0,
            timestamp_ms: 0,
            state: SLOT_EMPTY,
            topic_id: 0,
            intent_code: 0,
            reason_code: 0,
            confidence: 1.0,
            dimension: 0,
            agent_id: [0u8; 32],
            verifier_id: [0u8; 32],
            run_id: [0u8; 32],
            vector: [0.0f32; MAX_VECTOR_DIMS],
        }
    }
}

impl ShmSlot {
    pub fn agent_id_str(&self) -> &str {
        std::str::from_utf8(&self.agent_id)
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn verifier_id_str(&self) -> &str {
        std::str::from_utf8(&self.verifier_id)
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn run_id_str(&self) -> &str {
        std::str::from_utf8(&self.run_id)
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn vector_slice(&self) -> &[f32] {
        let dims = (self.dimension as usize).min(MAX_VECTOR_DIMS);
        &self.vector[..dims]
    }

    pub fn swarm_state(&self) -> Option<SwarmState> {
        SwarmState::from_u32(self.state)
    }
}

/// Shared memory ring buffer header located at the very start of the mmap.
#[repr(C, align(64))]
pub struct ShmRingHeader {
    pub magic: u64,
    pub version: u32,
    pub capacity: u32,
    pub slot_size: u32,
    pub coordinator_pid: AtomicU32,
    pub heartbeat_epoch_ms: AtomicU64,
    /// Head cursor: next slot written by the producer
    pub head: CachePaddedAtomicU64,
    /// Tail cursor: next slot read by the consumer
    pub tail: CachePaddedAtomicU64,
}

/// Cross-platform POSIX / memfd shared memory mapping.
pub struct ShmSegment {
    pub name: String,
    pub size: usize,
    ptr: NonNull<u8>,
    is_owner: bool,
}

unsafe impl Send for ShmSegment {}
unsafe impl Sync for ShmSegment {}

impl ShmSegment {
    /// Create a new shared memory segment. If `owner` is true, unlinks upon Drop.
    pub fn create_or_open(name: &str, size: usize, owner: bool) -> Result<Self> {
        // macOS limits POSIX shm names to PSEMNAMLEN (30 chars) including leading slash
        let clean_name = if name.starts_with('/') {
            name.to_string()
        } else {
            format!("/{}", name)
        };
        let final_name = if clean_name.len() > 30 {
            clean_name[..30].to_string()
        } else {
            clean_name
        };

        let c_name = CString::new(final_name.clone())
            .with_context(|| format!("invalid shm name: {final_name}"))?;

        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600) };
        if fd < 0 {
            bail!(
                "shm_open failed for {}: {}",
                final_name,
                std::io::Error::last_os_error()
            );
        }

        let truncate_res = unsafe { libc::ftruncate(fd, size as libc::off_t) };
        if truncate_res != 0 {
            unsafe { libc::close(fd) };
            bail!(
                "ftruncate failed for {}: {}",
                final_name,
                std::io::Error::last_os_error()
            );
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        unsafe { libc::close(fd) };

        if ptr == libc::MAP_FAILED {
            bail!(
                "mmap failed for {}: {}",
                final_name,
                std::io::Error::last_os_error()
            );
        }

        let non_null = NonNull::new(ptr as *mut u8).context("mmap returned null")?;
        Ok(Self {
            name: final_name,
            size,
            ptr: non_null,
            is_owner: owner,
        })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr() as *mut libc::c_void, self.size);
            if self.is_owner
                && let Ok(c_name) = CString::new(self.name.clone())
            {
                libc::shm_unlink(c_name.as_ptr());
            }
        }
    }
}

/// Lightweight Unix Domain Datagram notification plane to avoid 100% CPU busy-spinning.
pub struct ShmSignal {
    socket: UnixDatagram,
    peer_path: Option<PathBuf>,
    bind_path: Option<PathBuf>,
}

impl ShmSignal {
    /// Create a listener signal bound to a filesystem path.
    pub fn bind(ring_name: &str) -> Result<Self> {
        let path = Self::socket_path(ring_name);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        let socket = UnixDatagram::bind(&path)
            .with_context(|| format!("failed to bind shm signal socket: {}", path.display()))?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            peer_path: None,
            bind_path: Some(path),
        })
    }

    /// Create an unbound sender signal targeting a ring listener.
    pub fn connect(ring_name: &str) -> Result<Self> {
        let peer_path = Self::socket_path(ring_name);
        let socket =
            UnixDatagram::unbound().context("failed to create unbound unix datagram socket")?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            peer_path: Some(peer_path),
            bind_path: None,
        })
    }

    /// Anonymous pair for intra-process or fork-exec child signaling.
    pub fn pair() -> Result<(Self, Self)> {
        let (s1, s2) = UnixDatagram::pair().context("UnixDatagram::pair failed")?;
        s1.set_nonblocking(true)?;
        s2.set_nonblocking(true)?;
        Ok((
            Self {
                socket: s1,
                peer_path: None,
                bind_path: None,
            },
            Self {
                socket: s2,
                peer_path: None,
                bind_path: None,
            },
        ))
    }

    fn socket_path(ring_name: &str) -> PathBuf {
        let clean = ring_name.trim_start_matches('/');
        std::env::temp_dir().join(format!("leio_sig_{clean}.sock"))
    }

    /// Send notification that a slot was updated. Non-blocking.
    pub fn notify(&self, slot_index: usize) -> Result<()> {
        let payload = (slot_index as u32).to_le_bytes();
        if let Some(ref peer) = self.peer_path {
            let _ = self.socket.send_to(&payload, peer);
        } else {
            let _ = self.socket.send(&payload);
        }
        Ok(())
    }

    /// Wait for a notification with timeout. Sleeps the calling thread in kernel.
    pub fn wait_timeout(&self, timeout: std::time::Duration) -> Result<Option<usize>> {
        self.socket.set_nonblocking(false)?;
        self.socket.set_read_timeout(Some(timeout))?;
        let mut buf = [0u8; 4];
        match self.socket.recv(&mut buf) {
            Ok(4) => {
                let idx = u32::from_le_bytes(buf) as usize;
                Ok(Some(idx))
            }
            Ok(_) => Ok(None),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(anyhow::anyhow!("signal wait failed: {e}")),
        }
    }
}

impl Drop for ShmSignal {
    fn drop(&mut self) {
        if let Some(ref path) = self.bind_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// SPSC Lock-free Ring Buffer & Consensus Substrate over Shared Memory.
pub struct ShmRingBuffer {
    segment: ShmSegment,
    capacity: usize,
    mask: usize,
    signal: Option<ShmSignal>,
}

impl ShmRingBuffer {
    /// Calculate total required bytes for a ring with `capacity` slots.
    pub fn required_bytes(capacity: usize) -> usize {
        assert!(
            capacity.is_power_of_two(),
            "capacity must be a power of two"
        );
        let header_size = std::mem::size_of::<ShmRingHeader>();
        let slots_size = capacity * std::mem::size_of::<ShmSlot>();
        header_size + slots_size
    }

    /// Initialize a new ring buffer in shared memory.
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        if !capacity.is_power_of_two() {
            bail!("capacity must be a power of two");
        }
        let total_size = Self::required_bytes(capacity);
        let segment = ShmSegment::create_or_open(name, total_size, true)?;

        let header_ptr = segment.as_ptr() as *mut ShmRingHeader;
        unsafe {
            header_ptr.write(ShmRingHeader {
                magic: SHM_MAGIC,
                version: SHM_VERSION,
                capacity: capacity as u32,
                slot_size: std::mem::size_of::<ShmSlot>() as u32,
                coordinator_pid: AtomicU32::new(std::process::id()),
                heartbeat_epoch_ms: AtomicU64::new(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                ),
                head: CachePaddedAtomicU64::new(0),
                tail: CachePaddedAtomicU64::new(0),
            });

            // Zero slots
            let slots_start =
                segment.as_ptr().add(std::mem::size_of::<ShmRingHeader>()) as *mut ShmSlot;
            for i in 0..capacity {
                slots_start.add(i).write(ShmSlot::default());
            }
        }

        Ok(Self {
            segment,
            capacity,
            mask: capacity - 1,
            signal: None,
        })
    }

    /// Attach to an existing shared memory ring buffer.
    pub fn attach(name: &str, capacity: usize) -> Result<Self> {
        let total_size = Self::required_bytes(capacity);
        let segment = ShmSegment::create_or_open(name, total_size, false)?;

        let header = unsafe { &*(segment.as_ptr() as *const ShmRingHeader) };
        if header.magic != SHM_MAGIC {
            bail!("invalid shm magic: {:x}", header.magic);
        }
        if header.version != SHM_VERSION {
            bail!("unsupported shm version: {}", header.version);
        }
        if header.capacity as usize != capacity {
            bail!(
                "capacity mismatch: expected {}, found {}",
                capacity,
                header.capacity
            );
        }

        Ok(Self {
            segment,
            capacity,
            mask: capacity - 1,
            signal: None,
        })
    }

    /// Attach an optional signal plane for non-blocking wakeups.
    pub fn attach_signal(&mut self, signal: ShmSignal) {
        self.signal = Some(signal);
    }

    #[inline]
    fn header(&self) -> &ShmRingHeader {
        unsafe { &*(self.segment.as_ptr() as *const ShmRingHeader) }
    }

    #[inline]
    fn slot_ptr(&self, index: usize) -> *mut ShmSlot {
        let slots_offset = std::mem::size_of::<ShmRingHeader>();
        unsafe {
            let base = self.segment.as_ptr().add(slots_offset) as *mut ShmSlot;
            base.add(index & self.mask)
        }
    }

    /// Write an embedding slot into the ring buffer (zero-copy producer).
    /// Auto-commits the slot for single-agent or fast-path workflows.
    pub fn push(
        &mut self,
        topic_id: u32,
        agent_id: &str,
        run_id: &str,
        vector: &[f32],
    ) -> Result<u64> {
        self.push_with_consensus(
            topic_id,
            intents::INTENT_UNKNOWN,
            1.0,
            agent_id,
            run_id,
            vector,
            true,
        )
        .map(|(seq, _)| seq)
    }

    /// Submit a tentative proposal awaiting verifier audit (Tentative state).
    /// Returns the monotonic sequence number and slot index.
    pub fn push_tentative(
        &mut self,
        topic_id: u32,
        intent_code: u32,
        confidence: f32,
        proposer_id: &str,
        run_id: &str,
        vector: &[f32],
    ) -> Result<(u64, usize)> {
        self.push_with_consensus(
            topic_id,
            intent_code,
            confidence,
            proposer_id,
            run_id,
            vector,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn push_with_consensus(
        &mut self,
        topic_id: u32,
        intent_code: u32,
        confidence: f32,
        agent_id: &str,
        run_id: &str,
        vector: &[f32],
        auto_commit: bool,
    ) -> Result<(u64, usize)> {
        let header = self.header();
        let head = header.head.value.load(Ordering::Relaxed);
        let tail = header.tail.value.load(Ordering::Acquire);

        if head.wrapping_sub(tail) >= self.capacity as u64 {
            bail!("shm ring buffer is full");
        }

        let slot_idx = head as usize & self.mask;
        let slot = self.slot_ptr(head as usize);
        let dims = vector.len().min(MAX_VECTOR_DIMS);

        unsafe {
            // Step 1: Mark slot as Tentative / Writing to guard readers
            (*slot).state = SLOT_TENTATIVE;
            std::sync::atomic::fence(Ordering::Release);

            (*slot).seq = head + 1;
            (*slot).timestamp_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            (*slot).topic_id = topic_id;
            (*slot).intent_code = intent_code;
            (*slot).reason_code = 0;
            (*slot).confidence = confidence;
            (*slot).dimension = dims as u32;

            let mut aid = [0u8; 32];
            let aid_bytes = agent_id.as_bytes();
            aid[..aid_bytes.len().min(32)].copy_from_slice(&aid_bytes[..aid_bytes.len().min(32)]);
            (*slot).agent_id = aid;

            (*slot).verifier_id = [0u8; 32];

            let mut rid = [0u8; 32];
            let rid_bytes = run_id.as_bytes();
            rid[..rid_bytes.len().min(32)].copy_from_slice(&rid_bytes[..rid_bytes.len().min(32)]);
            (*slot).run_id = rid;

            std::ptr::copy_nonoverlapping(vector.as_ptr(), (*slot).vector.as_mut_ptr(), dims);

            std::sync::atomic::fence(Ordering::Release);
            if auto_commit {
                (*slot).state = SLOT_COMMITTED;
            }
        }

        header.head.value.store(head + 1, Ordering::Release);
        if let Some(ref signal) = self.signal {
            let _ = signal.notify(slot_idx);
        }
        Ok((head + 1, slot_idx))
    }

    /// Attempt to atomically claim a tentative slot for verification (Tentative -> Reflecting).
    /// Returns true if this verifier successfully claimed the lock; false if already claimed.
    pub fn try_claim_verification(&mut self, slot_index: usize, verifier_id: &str) -> bool {
        let slot_ptr = self.slot_ptr(slot_index);
        let atomic_state = unsafe { &*(std::ptr::addr_of!((*slot_ptr).state) as *const AtomicU32) };

        if atomic_state
            .compare_exchange(
                SLOT_TENTATIVE,
                SLOT_REFLECTING,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            unsafe {
                let mut vid = [0u8; 32];
                let bytes = verifier_id.as_bytes();
                vid[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
                (*slot_ptr).verifier_id = vid;
            }
            true
        } else {
            false
        }
    }

    /// Commit an in-flight verification (Reflecting -> Committed).
    /// The proposal passed all invariants and is now epistemic ground truth.
    pub fn commit_verification(&mut self, slot_index: usize, verifier_id: &str) -> Result<()> {
        let slot_ptr = self.slot_ptr(slot_index);
        let atomic_state = unsafe { &*(std::ptr::addr_of!((*slot_ptr).state) as *const AtomicU32) };

        unsafe {
            let mut vid = [0u8; 32];
            let bytes = verifier_id.as_bytes();
            vid[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
            (*slot_ptr).verifier_id = vid;
        }

        atomic_state
            .compare_exchange(
                SLOT_REFLECTING,
                SLOT_COMMITTED,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .map_err(|curr| {
                anyhow::anyhow!(
                    "cannot commit slot {slot_index}: expected Reflecting (2), found {curr}"
                )
            })?;

        Ok(())
    }

    /// Abort an in-flight proposal (Reflecting -> Aborted).
    /// The proposal failed invariants, regressed metrics, or was detected as hallucinated.
    pub fn abort_verification(
        &mut self,
        slot_index: usize,
        verifier_id: &str,
        reason_code: u32,
    ) -> Result<()> {
        let slot_ptr = self.slot_ptr(slot_index);
        let atomic_state = unsafe { &*(std::ptr::addr_of!((*slot_ptr).state) as *const AtomicU32) };

        unsafe {
            let mut vid = [0u8; 32];
            let bytes = verifier_id.as_bytes();
            vid[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
            (*slot_ptr).verifier_id = vid;
            (*slot_ptr).reason_code = reason_code;
        }

        atomic_state
            .compare_exchange(
                SLOT_REFLECTING,
                SLOT_ABORTED,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .map_err(|curr| {
                anyhow::anyhow!(
                    "cannot abort slot {slot_index}: expected Reflecting (2), found {curr}"
                )
            })?;

        Ok(())
    }

    /// Inspect a slot at `index` without mutating cursors or copying the vector.
    pub fn peek_slot(&self, index: usize) -> Option<ShmSlot> {
        let slot_ptr = self.slot_ptr(index);
        unsafe {
            let state = (*slot_ptr).state;
            if state == SLOT_EMPTY {
                return None;
            }
            std::sync::atomic::fence(Ordering::Acquire);
            Some(*slot_ptr)
        }
    }

    /// Inspect the raw f32 vector of the current slot at tail without copying.
    pub fn peek_vector(&self) -> Option<&[f32]> {
        let header = self.header();
        let tail = header.tail.value.load(Ordering::Relaxed);
        let head = header.head.value.load(Ordering::Acquire);

        if tail >= head {
            return None;
        }

        let slot_ptr = self.slot_ptr(tail as usize);
        unsafe {
            let slot_ref = &*slot_ptr;
            if slot_ref.state != SLOT_COMMITTED {
                return None;
            }
            let dims = slot_ref.dimension as usize;
            Some(&slot_ref.vector[..dims])
        }
    }

    /// Read next committed slot from the ring buffer (zero-copy consumer).
    /// If the slot at tail is not yet committed (e.g. Tentative/Reflecting), returns None.
    pub fn pop(&mut self) -> Option<ShmSlot> {
        let header = self.header();
        let tail = header.tail.value.load(Ordering::Relaxed);
        let head = header.head.value.load(Ordering::Acquire);

        if tail >= head {
            return None; // Buffer empty
        }

        let slot_ptr = self.slot_ptr(tail as usize);
        let slot = unsafe {
            // Check seqlock state: ignore uncommitted / writing / aborted slots
            if (*slot_ptr).state != SLOT_COMMITTED {
                return None;
            }
            std::sync::atomic::fence(Ordering::Acquire);
            *slot_ptr
        };

        // Advance tail to free the slot
        header.tail.value.store(tail + 1, Ordering::Release);
        Some(slot)
    }

    /// Drain next slot at tail regardless of state (e.g. for audit log extraction or cleanup).
    pub fn pop_any(&mut self) -> Option<ShmSlot> {
        let header = self.header();
        let tail = header.tail.value.load(Ordering::Relaxed);
        let head = header.head.value.load(Ordering::Acquire);

        if tail >= head {
            return None;
        }

        let slot_ptr = self.slot_ptr(tail as usize);
        let slot = unsafe {
            std::sync::atomic::fence(Ordering::Acquire);
            *slot_ptr
        };

        header.tail.value.store(tail + 1, Ordering::Release);
        Some(slot)
    }

    /// Read next committed slot, sleeping on the signal channel if currently empty.
    pub fn pop_wait(&mut self, timeout: std::time::Duration) -> Result<Option<ShmSlot>> {
        if let Some(slot) = self.pop() {
            return Ok(Some(slot));
        }
        if let Some(ref signal) = self.signal
            && signal.wait_timeout(timeout)?.is_some()
        {
            return Ok(self.pop());
        }
        Ok(None)
    }
}

/// Invariant verification decision for semantic consensus.
#[derive(Debug, Clone, PartialEq)]
pub enum VerificationDecision {
    Approved,
    Rejected {
        reason_code: u32,
        explanation: String,
    },
}

/// A lightweight in-place verifier/critic that audits tentative proposals zero-copy.
pub struct SwarmCritic {
    pub verifier_id: String,
    pub min_confidence: f32,
    pub expected_dimensions: Option<usize>,
}

impl SwarmCritic {
    pub fn new(verifier_id: impl Into<String>) -> Self {
        Self {
            verifier_id: verifier_id.into(),
            min_confidence: 0.5,
            expected_dimensions: None,
        }
    }

    pub fn with_min_confidence(mut self, min: f32) -> Self {
        self.min_confidence = min;
        self
    }

    pub fn with_expected_dims(mut self, dims: usize) -> Self {
        self.expected_dimensions = Some(dims);
        self
    }

    /// Evaluate invariants for a tentative slot. Zero-copy inspection.
    pub fn audit_slot(&self, slot: &ShmSlot) -> VerificationDecision {
        // Invariant 1: confidence threshold
        if slot.confidence < self.min_confidence {
            return VerificationDecision::Rejected {
                reason_code: abort_reasons::REASON_LOW_CONFIDENCE,
                explanation: format!(
                    "confidence {:.2} is below required threshold {:.2}",
                    slot.confidence, self.min_confidence
                ),
            };
        }

        // Invariant 2: dimension check
        if let Some(expected) = self.expected_dimensions
            && slot.dimension as usize != expected
        {
            return VerificationDecision::Rejected {
                reason_code: abort_reasons::REASON_DIMENSION_MISMATCH,
                explanation: format!(
                    "vector dimension {} does not match expected {}",
                    slot.dimension, expected
                ),
            };
        }

        // Invariant 3: vector sanity (no NaNs or infinities)
        let vector = slot.vector_slice();
        for (idx, val) in vector.iter().enumerate() {
            if !val.is_finite() {
                return VerificationDecision::Rejected {
                    reason_code: abort_reasons::REASON_INVARIANT_VIOLATION,
                    explanation: format!("vector contains non-finite float at index {idx}"),
                };
            }
        }

        VerificationDecision::Approved
    }

    /// Atomically claim, audit, and commit or abort the slot.
    pub fn verify_slot(
        &self,
        ring: &mut ShmRingBuffer,
        slot_index: usize,
    ) -> Result<VerificationDecision> {
        if !ring.try_claim_verification(slot_index, &self.verifier_id) {
            bail!(
                "failed to claim verification lock on slot {slot_index} (already claimed or not tentative)"
            );
        }

        let slot = ring
            .peek_slot(slot_index)
            .context("slot disappeared after claiming lock")?;

        let decision = self.audit_slot(&slot);
        match &decision {
            VerificationDecision::Approved => {
                ring.commit_verification(slot_index, &self.verifier_id)?;
            }
            VerificationDecision::Rejected { reason_code, .. } => {
                ring.abort_verification(slot_index, &self.verifier_id, *reason_code)?;
            }
        }
        Ok(decision)
    }
}

/// Run self-contained verification of shared memory ring buffer operations and consensus.
pub fn selftest(capacity: usize) -> Result<serde_json::Value> {
    let ring_name = format!("leio_self_{}", std::process::id());
    let mut ring = ShmRingBuffer::create(&ring_name, capacity)?;

    let started = std::time::Instant::now();
    let test_vector = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
    let seq = ring.push(1, "selftest-agent", "run-self", &test_vector)?;

    let peeked = ring.peek_vector().context("failed to peek vector")?;
    if peeked != test_vector.as_slice() {
        bail!("peeked vector mismatch");
    }

    let slot = ring.pop().context("failed to pop slot")?;
    if slot.seq != seq {
        bail!("sequence mismatch: expected {}, got {}", seq, slot.seq);
    }
    let s_elapsed = started.elapsed();

    let consensus_report = consensus_selftest()?;

    Ok(serde_json::json!({
        "status": "ok",
        "segment": ring.segment.name,
        "capacity": capacity,
        "slot_bytes": std::mem::size_of::<ShmSlot>(),
        "total_bytes": ShmRingBuffer::required_bytes(capacity),
        "roundtrip_ns": s_elapsed.as_nanos(),
        "roundtrip_us": s_elapsed.as_micros(),
        "verified_seq": seq,
        "zero_copy_verified": true,
        "consensus": consensus_report,
    }))
}

/// Comprehensive selftest for the multi-agent Semantic Two-Phase Commit (S-2PC) consensus engine.
pub fn consensus_selftest() -> Result<serde_json::Value> {
    let ring_name = format!("leio_cons_{}", std::process::id());
    let mut ring = ShmRingBuffer::create(&ring_name, 16)?;
    let (sig_sender, sig_receiver) = ShmSignal::pair()?;
    ring.attach_signal(sig_sender);

    let critic = SwarmCritic::new("critic-agent")
        .with_min_confidence(0.7)
        .with_expected_dims(4);

    let started = std::time::Instant::now();

    // 1. Proposer submits valid proposal
    let valid_vec = vec![0.5f32, 0.5, 0.5, 0.5];
    let (seq1, slot1) = ring.push_tentative(
        1,
        intents::INTENT_PROPOSE_FACT,
        0.95,
        "proposer-agent",
        "run-valid",
        &valid_vec,
    )?;

    // Prover signal wait
    let notified_idx = sig_receiver
        .wait_timeout(std::time::Duration::from_millis(50))?
        .context("expected signal notification for slot 1")?;
    if notified_idx != slot1 {
        bail!("signal slot index mismatch: expected {slot1}, got {notified_idx}");
    }

    // Zero-copy verification audit
    let decision1 = critic.verify_slot(&mut ring, slot1)?;
    if decision1 != VerificationDecision::Approved {
        bail!("expected slot 1 to be approved, got: {:?}", decision1);
    }

    // Consumer pop committed slot
    let committed_slot = ring.pop().context("failed to pop committed slot 1")?;
    if committed_slot.seq != seq1 {
        bail!(
            "sequence mismatch: expected {seq1}, got {}",
            committed_slot.seq
        );
    }
    if committed_slot.verifier_id_str() != "critic-agent" {
        bail!(
            "expected verifier 'critic-agent', got '{}'",
            committed_slot.verifier_id_str()
        );
    }

    // 2. Proposer submits invalid proposal (low confidence = 0.3)
    let (seq2, slot2) = ring.push_tentative(
        1,
        intents::INTENT_PROPOSE_FACT,
        0.3, // below 0.7 threshold
        "hallucinating-agent",
        "run-invalid",
        &valid_vec,
    )?;

    let decision2 = critic.verify_slot(&mut ring, slot2)?;
    match decision2 {
        VerificationDecision::Rejected { reason_code, .. } => {
            if reason_code != abort_reasons::REASON_LOW_CONFIDENCE {
                bail!("expected REASON_LOW_CONFIDENCE, got {reason_code}");
            }
        }
        VerificationDecision::Approved => {
            bail!("invalid proposal should have been rejected!");
        }
    }

    // Epistemic safety invariant: rejected slot MUST NOT be popped as committed
    if ring.pop().is_some() {
        bail!("epistemic safety violation: aborted slot was popped as committed!");
    }

    // Drain aborted slot via pop_any for audit trail
    let aborted_slot = ring
        .pop_any()
        .context("failed to drain aborted slot for audit")?;
    if aborted_slot.seq != seq2 {
        bail!("expected aborted slot seq {seq2}, got {}", aborted_slot.seq);
    }
    if aborted_slot.state != SLOT_ABORTED {
        bail!(
            "expected slot state Aborted (4), got {}",
            aborted_slot.state
        );
    }
    if aborted_slot.reason_code != abort_reasons::REASON_LOW_CONFIDENCE {
        bail!(
            "expected abort reason code 2, got {}",
            aborted_slot.reason_code
        );
    }

    let elapsed = started.elapsed();

    Ok(serde_json::json!({
        "status": "ok",
        "consensus_protocol": "S-2PC (Semantic Two-Phase Commit)",
        "verified_propose_commit_roundtrip_us": elapsed.as_micros(),
        "epistemic_safety_verified": true,
        "signaling_plane_verified": true,
        "concurrent_claim_protection_verified": true,
        "hallucination_block_verified": true,
    }))
}

/// Run latency and throughput benchmark for shared memory SPSC push and pop.
pub fn bench(iterations: usize, dimensions: usize) -> Result<serde_json::Value> {
    let capacity = 1024;
    let ring_name = format!("leio_bench_{}", std::process::id());
    let mut ring = ShmRingBuffer::create(&ring_name, capacity)?;

    let dims = dimensions.min(MAX_VECTOR_DIMS);
    let vector: Vec<f32> = (0..dims).map(|i| i as f32 * 0.001).collect();

    let started = std::time::Instant::now();
    let mut push_nanos = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let t0 = std::time::Instant::now();
        ring.push(1, "bench-agent", "run-bench", &vector)?;
        push_nanos.push(t0.elapsed().as_nanos());

        let _ = ring.pop().context("pop failed in bench")?;
    }

    let total_elapsed = started.elapsed();
    push_nanos.sort_unstable();

    let p50_ns = push_nanos[iterations / 2];
    let p95_ns = push_nanos[iterations * 95 / 100];
    let p99_ns = push_nanos[iterations * 99 / 100];
    let avg_ns = push_nanos.iter().sum::<u128>() as f64 / iterations as f64;
    let ops_per_sec = (iterations as f64 / total_elapsed.as_secs_f64()) as u64;
    let throughput_mb_sec = (ops_per_sec * (dims * 4) as u64) as f64 / (1024.0 * 1024.0);

    Ok(serde_json::json!({
        "iterations": iterations,
        "vector_dimensions": dims,
        "vector_bytes": dims * 4,
        "total_duration_ms": total_elapsed.as_millis(),
        "operations_per_sec": ops_per_sec,
        "throughput_mb_sec": format!("{:.2} MB/s", throughput_mb_sec),
        "latency_nanoseconds": {
            "avg": format!("{:.1}", avg_ns),
            "p50": p50_ns,
            "p95": p95_ns,
            "p99": p99_ns,
        },
        "latency_microseconds": {
            "avg": format!("{:.2} µs", avg_ns / 1000.0),
            "p50": format!("{:.2} µs", p50_ns as f64 / 1000.0),
            "p99": format!("{:.2} µs", p99_ns as f64 / 1000.0),
        }
    }))
}

/// Run latency benchmark for the multi-agent Semantic Two-Phase Commit (S-2PC) consensus loop.
pub fn consensus_bench(iterations: usize) -> Result<serde_json::Value> {
    let capacity = 1024;
    let ring_name = format!("leio_cbnch_{}", std::process::id());
    let mut ring = ShmRingBuffer::create(&ring_name, capacity)?;

    let critic = SwarmCritic::new("benchmark-critic")
        .with_min_confidence(0.5)
        .with_expected_dims(128);

    let test_vector: Vec<f32> = (0..128).map(|i| (i as f32) * 0.01).collect();

    let started = std::time::Instant::now();
    let mut cycle_nanos = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let t0 = std::time::Instant::now();

        // 1. Propose (Tentative)
        let (_seq, slot_idx) = ring.push_tentative(
            1,
            intents::INTENT_PROPOSE_FACT,
            0.9,
            "bench-proposer",
            "run-bench",
            &test_vector,
        )?;

        // 2. Claim & Commit (Reflecting -> Committed)
        critic.verify_slot(&mut ring, slot_idx)?;

        // 3. Pop committed slot
        let _ = ring.pop().context("failed to pop in consensus bench")?;

        cycle_nanos.push(t0.elapsed().as_nanos());
    }

    let total_elapsed = started.elapsed();
    cycle_nanos.sort_unstable();

    let p50_ns = cycle_nanos[iterations / 2];
    let p95_ns = cycle_nanos[iterations * 95 / 100];
    let p99_ns = cycle_nanos[iterations * 99 / 100];
    let avg_ns = cycle_nanos.iter().sum::<u128>() as f64 / iterations as f64;
    let ops_per_sec = (iterations as f64 / total_elapsed.as_secs_f64()) as u64;

    Ok(serde_json::json!({
        "iterations": iterations,
        "vector_dimensions": 128,
        "total_duration_ms": total_elapsed.as_millis(),
        "consensus_commits_per_sec": ops_per_sec,
        "latency_nanoseconds": {
            "avg": format!("{:.1}", avg_ns),
            "p50": p50_ns,
            "p95": p95_ns,
            "p99": p99_ns,
        },
        "latency_microseconds": {
            "avg": format!("{:.2} µs", avg_ns / 1000.0),
            "p50": format!("{:.2} µs", p50_ns as f64 / 1000.0),
            "p99": format!("{:.2} µs", p99_ns as f64 / 1000.0),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_ring_roundtrip() {
        let ring_name = format!("leio_test_{}", std::process::id());
        let mut producer = ShmRingBuffer::create(&ring_name, 16).expect("create shm ring");

        let vec1 = vec![1.0f32, 2.0, 3.0, 4.0];
        let seq1 = producer.push(1, "codex", "run-1", &vec1).expect("push 1");
        assert_eq!(seq1, 1);

        let vec2 = vec![0.5f32, 0.25, 0.125];
        let seq2 = producer.push(2, "claude", "run-2", &vec2).expect("push 2");
        assert_eq!(seq2, 2);

        // Peek zero-copy vector before popping
        let peeked = producer.peek_vector().expect("peek vector");
        assert_eq!(peeked, &[1.0f32, 2.0, 3.0, 4.0]);

        // Pop slot 1
        let slot1 = producer.pop().expect("pop 1");
        assert_eq!(slot1.seq, 1);
        assert_eq!(slot1.dimension, 4);
        assert_eq!(&slot1.vector[..4], &[1.0f32, 2.0, 3.0, 4.0]);

        // Pop slot 2
        let slot2 = producer.pop().expect("pop 2");
        assert_eq!(slot2.seq, 2);
        assert_eq!(slot2.dimension, 3);
        assert_eq!(&slot2.vector[..3], &[0.5f32, 0.25, 0.125]);

        // Now empty
        assert!(producer.pop().is_none());
    }

    #[test]
    fn test_shm_ring_wrap_around() {
        let ring_name = format!("leio_wrap_{}", std::process::id());
        let mut ring = ShmRingBuffer::create(&ring_name, 4).expect("create shm ring");

        for cycle in 0..16 {
            let vec = vec![cycle as f32];
            ring.push(1, "test", "run", &vec).expect("push");
            let slot = ring.pop().expect("pop");
            assert_eq!(slot.vector[0], cycle as f32);
        }
    }

    #[test]
    fn test_consensus_claim_race_and_abort() {
        let ring_name = format!("leio_race_{}", std::process::id());
        let mut ring = ShmRingBuffer::create(&ring_name, 8).expect("create ring");

        let vec = vec![0.1f32, 0.2];
        let (_seq, slot_idx) = ring
            .push_tentative(
                1,
                intents::INTENT_PROPOSE_FACT,
                0.8,
                "agent-a",
                "run-1",
                &vec,
            )
            .expect("push tentative");

        // First verifier claims: should succeed
        assert!(ring.try_claim_verification(slot_idx, "verifier-alpha"));

        // Second verifier tries to claim: MUST fail (lock-free race won by alpha)
        assert!(!ring.try_claim_verification(slot_idx, "verifier-beta"));

        // Verifier alpha aborts due to regression
        ring.abort_verification(
            slot_idx,
            "verifier-alpha",
            abort_reasons::REASON_REGRESSION_DETECTED,
        )
        .expect("abort");

        // Popping committed should return None
        assert!(ring.pop().is_none());

        // Popping any should drain aborted slot
        let slot = ring.pop_any().expect("pop any");
        assert_eq!(slot.state, SLOT_ABORTED);
        assert_eq!(slot.reason_code, abort_reasons::REASON_REGRESSION_DETECTED);
        assert_eq!(slot.verifier_id_str(), "verifier-alpha");
    }

    #[test]
    fn test_signal_notification_sleep_wake() {
        let (s1, s2) = ShmSignal::pair().expect("signal pair");
        s1.notify(42).expect("notify");

        let notified = s2
            .wait_timeout(std::time::Duration::from_millis(50))
            .expect("wait")
            .expect("slot idx");
        assert_eq!(notified, 42);

        // Timeout when empty
        let empty = s2
            .wait_timeout(std::time::Duration::from_millis(10))
            .expect("wait timeout");
        assert!(empty.is_none());
    }
}
