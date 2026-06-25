use std::{
    fs::OpenOptions,
    mem::size_of,
    path::PathBuf,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use memmap2::{Mmap, MmapMut};

const CONTROL_VERSION: u32 = 3;

/// How much of a hand's input is withheld from the game.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMode {
    /// The game sees everything.
    #[default]
    None,
    /// Only trigger-bound actions are withheld. Used while pointing at the
    /// watch, where the trigger is the only thing wayvr consumes and taking
    /// away the rest (thumbstick, grip, buttons) would be more disruptive than
    /// the stray click it prevents.
    TriggerOnly,
    /// Every blockable action of that hand is withheld.
    All,
}

impl BlockMode {
    const fn to_bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::TriggerOnly => 1,
            Self::All => 2,
        }
    }

    pub const fn blocks_anything(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The weaker of two modes, ordered `None` < `TriggerOnly` < `All`.
    #[must_use]
    pub const fn min_mode(self, other: Self) -> Self {
        if self.to_bits() <= other.to_bits() {
            self
        } else {
            other
        }
    }

    const fn from_bits(bits: u32) -> Self {
        match bits {
            1 => Self::TriggerOnly,
            2 => Self::All,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

impl Hand {
    /// Index of this hand in a left-then-right pair.
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    /// Bit offset of this hand's two-bit [`BlockMode`] field in `flags`.
    const fn shift(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Right => 2,
        }
    }
}

const MODE_MASK: u32 = 0b11;

/// The reader treats the control state as live only if the writer's heartbeat
/// is newer than this. Picked well above a VR frame interval so a momentarily
/// busy wayvr never looks dead, but short enough that a crashed wayvr unblocks
/// input within a fraction of a second.
const HEARTBEAT_STALE_NS: u64 = 1_000_000_000;

#[repr(C)]
struct SharedState {
    version: AtomicU32,
    _reserved: AtomicU32,
    /// Wall-clock nanoseconds of the writer's last heartbeat. Wall clock is used
    /// deliberately: it is shared across PID/mount namespaces, so a reader inside
    /// a sandbox (e.g. Steam's pressure-vessel) can still tell whether the
    /// host-side writer is alive — unlike a PID, which is invisible across a PID
    /// namespace.
    heartbeat_ns: AtomicU64,
    flags: AtomicU32,
}

/// View the head of a mapping as the shared control state.
fn state(map: &[u8]) -> &SharedState {
    // SAFETY: the mapping is page-aligned (so satisfies SharedState's alignment)
    // and at least size_of::<SharedState>() bytes; every field is an integer
    // atomic, valid for any bit pattern and safe to share without `&mut`.
    unsafe { &*(map.as_ptr() as *const SharedState) }
}

fn realtime_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub fn control_path() -> PathBuf {
    if let Some(path) = std::env::var_os("WAYVR_OPENXR_LAYER_CONTROL_PATH") {
        return path.into();
    }

    // Must live somewhere visible from inside a sandboxed game process (Steam
    // pressure-vessel forwards only specific XDG_RUNTIME_DIR sockets, but it
    // bind-mounts the real home directory). Derived from $HOME directly rather
    // than $XDG_DATA_HOME so the host writer and the in-container reader compute
    // the identical path even if XDG vars differ between them.
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(".local/share/wayvr/openxr-layer-control")
}

pub struct ControlWriter {
    map: MmapMut,
}

impl ControlWriter {
    pub fn new() -> std::io::Result<Self> {
        let path = control_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.set_len(size_of::<SharedState>() as u64)?;

        // SAFETY: a file-backed shared mapping. We only ever mutate it through
        // atomics, so concurrent access from a reader-side process stays sound.
        let map = unsafe { MmapMut::map_mut(&file)? };

        let s = state(&map);
        s.version.store(CONTROL_VERSION, Ordering::Release);
        s.heartbeat_ns.store(realtime_ns(), Ordering::Release);
        s.flags.store(0, Ordering::Release);

        Ok(Self { map })
    }

    /// Publish the per-hand block modes. The two hands are independent: pointing
    /// one hand at an overlay must not take input away from the other.
    pub fn set(&self, left: BlockMode, right: BlockMode) {
        let flags =
            (left.to_bits() << Hand::Left.shift()) | (right.to_bits() << Hand::Right.shift());
        let s = state(&self.map);
        s.heartbeat_ns.store(realtime_ns(), Ordering::Release);
        s.flags.store(flags, Ordering::Release);
    }

    pub fn clear(&self) {
        self.set(BlockMode::None, BlockMode::None);
    }

    /// Refresh the liveness heartbeat. wayvr must call this regularly (every
    /// frame is fine) so a reader can distinguish a running writer from a
    /// crashed one. Cheap: a single atomic store.
    pub fn heartbeat(&self) {
        state(&self.map)
            .heartbeat_ns
            .store(realtime_ns(), Ordering::Release);
    }
}

impl Drop for ControlWriter {
    fn drop(&mut self) {
        self.clear();
    }
}

pub struct ControlReader {
    map: Option<Mmap>,
}

impl ControlReader {
    pub fn new() -> Self {
        let map = OpenOptions::new()
            .read(true)
            .open(control_path())
            .ok()
            // SAFETY: a file-backed shared mapping read through atomics only.
            .and_then(|file| unsafe { Mmap::map(&file) }.ok());
        Self { map }
    }

    /// What the writer currently wants withheld from `hand`. Falls back to
    /// [`BlockMode::None`] whenever the control state cannot be trusted.
    pub fn block_mode(&self, hand: Hand) -> BlockMode {
        BlockMode::from_bits((self.flags() >> hand.shift()) & MODE_MASK)
    }

    fn flags(&self) -> u32 {
        let Some(map) = &self.map else {
            return 0;
        };
        let s = state(map);
        if s.version.load(Ordering::Acquire) != CONTROL_VERSION {
            return 0;
        }
        let heartbeat = s.heartbeat_ns.load(Ordering::Acquire);
        if realtime_ns().saturating_sub(heartbeat) > HEARTBEAT_STALE_NS {
            // Writer is gone or stalled; fail open so input is never stuck blocked.
            return 0;
        }

        s.flags.load(Ordering::Acquire)
    }

    /// Diagnostic snapshot of the control mapping for logging: whether the
    /// control file was mapped, the writer's last heartbeat age (in ms), whether
    /// that heartbeat is fresh enough to be considered live, the recorded
    /// version, and the raw flags regardless of the liveness gate.
    pub fn debug_snapshot(&self) -> ControlSnapshot {
        let Some(map) = &self.map else {
            return ControlSnapshot::default();
        };
        let s = state(map);
        let heartbeat = s.heartbeat_ns.load(Ordering::Acquire);
        let age_ns = realtime_ns().saturating_sub(heartbeat);
        ControlSnapshot {
            mapped: true,
            heartbeat_age_ms: age_ns / 1_000_000,
            live: age_ns <= HEARTBEAT_STALE_NS,
            version: s.version.load(Ordering::Acquire),
            raw_flags: s.flags.load(Ordering::Acquire),
        }
    }
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ControlSnapshot {
    pub mapped: bool,
    pub heartbeat_age_ms: u64,
    pub live: bool,
    pub version: u32,
    pub raw_flags: u32,
}
