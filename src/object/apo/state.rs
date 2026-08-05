//! object/apo/state.rs — APO 状态机（O2/v6.6）
//!
//! 纯状态逻辑：Created → Initialized → Locked，以及 LockForProcess 失败时的 RAII 回退守卫。
//! 不依赖 pipeline / config，只依赖 COM HRESULT 常量。

use std::sync::atomic::{AtomicU8, Ordering};

use crate::sys::com::apo_types::APOERR_ALREADY_INITIALIZED;
use crate::sys::com::prelude::HRESULT;

/// APO 对象状态。
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApoState {
    Created = 0,
    Initialized = 1,
    Locked = 2,
}

/// 状态转换错误（O2/v6.6）：携带期望/尝试/实际三态，替代纯字符串描述。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransitionError {
    /// 期望的起始状态。
    pub expected: ApoState,
    /// 尝试转换到的目标状态。
    pub attempted: ApoState,
    /// 实际所处的状态。
    pub actual: ApoState,
}

impl TransitionError {
    pub(crate) fn new(expected: ApoState, attempted: ApoState, actual: ApoState) -> Self {
        Self { expected, attempted, actual }
    }
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "状态转换失败：期望 {:?} → {:?}，但当前为 {:?}",
            self.expected, self.attempted, self.actual
        )
    }
}

/// TransitionError → HRESULT（O2）：统一映射为 APOERR_ALREADY_INITIALIZED。
impl From<TransitionError> for HRESULT {
    fn from(_: TransitionError) -> Self {
        APOERR_ALREADY_INITIALIZED
    }
}

/// 原子状态单元。
pub struct StateCell {
    state: AtomicU8,
}

impl StateCell {
    pub fn new() -> Self {
        Self { state: AtomicU8::new(ApoState::Created as u8) }
    }

    /// CAS 转换：成功返回 Ok，失败返回 `TransitionError{expected, attempted, actual}`。
    pub fn transition(
        &self,
        from: ApoState,
        to: ApoState,
    ) -> std::result::Result<(), TransitionError> {
        let actual_raw = self.state.load(Ordering::Acquire);
        if actual_raw != from as u8 {
            return Err(TransitionError::new(from, to, state_from_u8(actual_raw)));
        }
        self.state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|actual| TransitionError::new(from, to, state_from_u8(actual)))
    }

    /// 当前状态。
    pub fn current(&self) -> ApoState {
        state_from_u8(self.state.load(Ordering::Acquire))
    }

    /// release（O2）：任意状态 → Created，返回旧状态。DLL 卸载终态复位用。
    pub fn release(&self) -> ApoState {
        let old = self.state.swap(ApoState::Created as u8, Ordering::AcqRel);
        state_from_u8(old)
    }

    // ── 语义化便捷转换（失败即 TransitionError） ──
    pub fn initialize(&self) -> std::result::Result<(), TransitionError> {
        self.transition(ApoState::Created, ApoState::Initialized)
    }
    pub fn lock(&self) -> std::result::Result<(), TransitionError> {
        self.transition(ApoState::Initialized, ApoState::Locked)
    }
    pub fn unlock(&self) -> std::result::Result<(), TransitionError> {
        self.transition(ApoState::Locked, ApoState::Initialized)
    }
}

fn state_from_u8(v: u8) -> ApoState {
    match v {
        1 => ApoState::Initialized,
        2 => ApoState::Locked,
        _ => ApoState::Created,
    }
}

/// RAII 守卫：LockForProcess 失败时自动回退状态。
pub struct LockGuard<'a> {
    state_cell: &'a StateCell,
    armed: bool,
}

impl<'a> LockGuard<'a> {
    pub fn new(state_cell: &'a StateCell) -> Self {
        Self { state_cell, armed: true }
    }
    pub fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _: std::result::Result<(), TransitionError> =
                self.state_cell.transition(ApoState::Locked, ApoState::Initialized);
        }
    }
}
