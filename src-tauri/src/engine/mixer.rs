// 切换台核心状态机：控制 PGM(节目) / PVW(预监) 的选择、转场类型与进度、FTB 淡黑
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use super::compose::TransitionKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitcherState {
    pub pvw_id: usize,
    pub pgm_id: usize,
    pub kind: TransitionKind,
    /// 转场进度 0.0 ~ 1.0
    pub transition_progress: f32,
    pub is_in_transition: bool,
    /// 自动转场时长（毫秒）
    pub duration_ms: u64,
    /// FTB：节目输出淡黑
    pub ftb: bool,
}

pub struct Switcher {
    state: parking_lot::RwLock<SwitcherState>,
    is_animating: Arc<AtomicBool>,
}

impl Switcher {
    pub fn new(default_pvw: usize, default_pgm: usize) -> Self {
        Self {
            state: parking_lot::RwLock::new(SwitcherState {
                pvw_id: default_pvw,
                pgm_id: default_pgm,
                kind: TransitionKind::Mix,
                transition_progress: 0.0,
                is_in_transition: false,
                duration_ms: 1000,
                ftb: false,
            }),
            is_animating: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn get_state(&self) -> SwitcherState {
        self.state.read().clone()
    }

    /// 设置预监机位 (PVW)
    pub fn set_pvw(&self, id: usize) {
        self.state.write().pvw_id = id;
    }

    /// 直接设置播出机位（直切到 PGM，不翻转 PVW）
    pub fn set_pgm(&self, id: usize) {
        self.state.write().pgm_id = id;
    }

    pub fn set_kind(&self, kind: TransitionKind) {
        self.state.write().kind = kind;
    }

    pub fn set_duration(&self, ms: u64) {
        self.state.write().duration_ms = ms.clamp(100, 5000);
    }

    pub fn toggle_ftb(&self) -> bool {
        let mut s = self.state.write();
        s.ftb = !s.ftb;
        s.ftb
    }

    fn flip(s: &mut SwitcherState) {
        std::mem::swap(&mut s.pgm_id, &mut s.pvw_id);
        s.transition_progress = 0.0;
        s.is_in_transition = false;
    }

    /// 硬切 (CUT)：瞬间翻转 PGM 与 PVW
    pub fn cut(&self) {
        // 自动转场进行中时忽略，避免两个翻转叠加
        if self.is_animating.load(Ordering::SeqCst) {
            return;
        }
        Self::flip(&mut self.state.write());
    }

    /// 手动推拉杆 (T-Bar)
    pub fn set_tbar(&self, progress: f32) {
        if self.is_animating.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.state.write();
        let p = progress.clamp(0.0, 1.0);
        if p >= 0.999 {
            Self::flip(&mut s);
        } else if p <= 0.001 {
            s.transition_progress = 0.0;
            s.is_in_transition = false;
        } else {
            s.transition_progress = p;
            s.is_in_transition = true;
        }
    }

    /// 自动转场 (AUTO)：用独立线程按时间推进进度，结束后翻转 PGM/PVW
    pub fn auto(self: &Arc<Self>) {
        if self.is_animating.swap(true, Ordering::SeqCst) {
            return; // 已经在转场动画中
        }
        let duration_ms = self.state.read().duration_ms;
        let switcher = Arc::clone(self);
        std::thread::spawn(move || {
            let start = Instant::now();
            let total = Duration::from_millis(duration_ms.max(100)).as_secs_f32();
            loop {
                std::thread::sleep(Duration::from_millis(16));
                let progress = (start.elapsed().as_secs_f32() / total).min(1.0);
                {
                    let mut s = switcher.state.write();
                    if progress >= 1.0 {
                        Self::flip(&mut s);
                    } else {
                        s.transition_progress = progress;
                        s.is_in_transition = true;
                    }
                }
                if progress >= 1.0 {
                    break;
                }
            }
            switcher.is_animating.store(false, Ordering::SeqCst);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_swaps_pgm_and_pvw() {
        let s = Switcher::new(1, 2);
        s.cut();
        let st = s.get_state();
        assert_eq!((st.pvw_id, st.pgm_id), (2, 1));
    }

    #[test]
    fn tbar_end_flips_and_resets() {
        let s = Switcher::new(1, 2);
        s.set_tbar(0.5);
        let st = s.get_state();
        assert!(st.is_in_transition);
        assert!((st.transition_progress - 0.5).abs() < 1e-6);
        s.set_tbar(1.0);
        let st = s.get_state();
        assert_eq!((st.pvw_id, st.pgm_id), (2, 1));
        assert!(!st.is_in_transition);
        assert_eq!(st.transition_progress, 0.0);
    }

    #[test]
    fn auto_finishes_and_flips_once() {
        let s = Arc::new(Switcher::new(1, 2));
        s.set_duration(100);
        s.auto();
        s.auto(); // 重复触发应被忽略
        std::thread::sleep(Duration::from_millis(400));
        let st = s.get_state();
        assert_eq!((st.pvw_id, st.pgm_id), (2, 1));
        assert!(!st.is_in_transition);
    }

    #[test]
    fn ftb_toggles() {
        let s = Switcher::new(1, 2);
        assert!(s.toggle_ftb());
        assert!(!s.toggle_ftb());
    }
}
