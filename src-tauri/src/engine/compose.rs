// 画面合成：纯 CPU 实现的转场混合与淡黑，输入输出均为紧凑排列的 RGB24 数据
// 本模块不依赖 GStreamer，可独立单元测试

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// 混合（叠化）
    Mix,
    /// 推入：新画面把旧画面推出去
    Push,
    /// 擦除：左→右
    Wipe,
    /// 滑动：新画面从右侧滑入并覆盖旧画面
    Slide,
    /// 划像：从中线向两侧展开
    Split,
    /// 圆形：从中心圆形扩散
    Circle,
    /// 淡黑：旧画面淡出到黑再淡入新画面
    Dip,
    /// 下擦：上→下
    VWipe,
}

impl Default for TransitionKind {
    fn default() -> Self {
        TransitionKind::Mix
    }
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: u32) -> u8 {
    ((a as u32 * (256 - t) + b as u32 * t) >> 8) as u8
}

/// 把 a、b 两帧按转场类型和进度 p(0~1) 合成到 out。
/// a/b/out 长度都必须等于 w*h*3，否则直接复制 a（保证不 panic）。
pub fn compose(a: &[u8], b: &[u8], w: usize, h: usize, kind: TransitionKind, p: f32, out: &mut Vec<u8>) {
    let len = w * h * 3;
    out.clear();
    if a.len() != len || b.len() != len {
        out.extend_from_slice(if a.len() == len { a } else { b });
        out.resize(len, 0);
        return;
    }
    let p = p.clamp(0.0, 1.0);
    if p <= 0.0 {
        out.extend_from_slice(a);
        return;
    }
    if p >= 1.0 {
        out.extend_from_slice(b);
        return;
    }
    let stride = w * 3;
    match kind {
        TransitionKind::Mix => {
            let t = (p * 256.0) as u32;
            out.extend(a.iter().zip(b.iter()).map(|(&x, &y)| lerp_u8(x, y, t)));
        }
        TransitionKind::Dip => {
            // 前半段 a 淡出到黑，后半段 b 从黑淡入
            let (src, level) = if p < 0.5 { (a, 1.0 - p * 2.0) } else { (b, p * 2.0 - 1.0) };
            let k = (level * 256.0) as u32;
            out.extend(src.iter().map(|&x| ((x as u32 * k) >> 8) as u8));
        }
        TransitionKind::Wipe => {
            let x = ((p * w as f32) as usize).min(w) * 3;
            for y in 0..h {
                let row = y * stride;
                out.extend_from_slice(&b[row..row + x]);
                out.extend_from_slice(&a[row + x..row + stride]);
            }
        }
        TransitionKind::VWipe => {
            let split = ((p * h as f32) as usize).min(h);
            out.extend_from_slice(&b[..split * stride]);
            out.extend_from_slice(&a[split * stride..]);
        }
        TransitionKind::Slide => {
            let shown = ((p * w as f32) as usize).min(w) * 3;
            for y in 0..h {
                let row = y * stride;
                out.extend_from_slice(&a[row..row + stride - shown]);
                out.extend_from_slice(&b[row..row + shown]);
            }
        }
        TransitionKind::Push => {
            let shift = ((p * w as f32) as usize).min(w) * 3;
            for y in 0..h {
                let row = y * stride;
                out.extend_from_slice(&a[row + shift..row + stride]);
                out.extend_from_slice(&b[row..row + shift]);
            }
        }
        TransitionKind::Split => {
            let cx = w / 2;
            let half = ((p * w as f32 / 2.0) as usize).min(cx + 1);
            let l = cx.saturating_sub(half) * 3;
            let r = ((cx + half).min(w)) * 3;
            for y in 0..h {
                let row = y * stride;
                out.extend_from_slice(&a[row..row + l]);
                out.extend_from_slice(&b[row + l..row + r]);
                out.extend_from_slice(&a[row + r..row + stride]);
            }
        }
        TransitionKind::Circle => {
            let cx = w as f32 / 2.0;
            let cy = h as f32 / 2.0;
            let max_r = (cx * cx + cy * cy).sqrt();
            let r = p * max_r;
            for y in 0..h {
                let row = y * stride;
                let dy = y as f32 + 0.5 - cy;
                let d2 = r * r - dy * dy;
                if d2 <= 0.0 {
                    out.extend_from_slice(&a[row..row + stride]);
                    continue;
                }
                let dx = d2.sqrt();
                let l = ((cx - dx).max(0.0) as usize).min(w) * 3;
                let rr = ((cx + dx).min(w as f32) as usize).min(w) * 3;
                out.extend_from_slice(&a[row..row + l]);
                out.extend_from_slice(&b[row + l..row + rr]);
                out.extend_from_slice(&a[row + rr..row + stride]);
            }
        }
    }
}

/// 整帧乘以亮度系数（0=全黑，1=原样），用于 FTB 淡黑
pub fn apply_fade(frame: &mut [u8], level: f32) {
    let level = level.clamp(0.0, 1.0);
    if level >= 1.0 {
        return;
    }
    if level <= 0.0 {
        frame.fill(0);
        return;
    }
    let k = (level * 256.0) as u32;
    for px in frame.iter_mut() {
        *px = ((*px as u32 * k) >> 8) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 16;
    const H: usize = 9;

    fn solid(v: u8) -> Vec<u8> {
        vec![v; W * H * 3]
    }

    const ALL: [TransitionKind; 8] = [
        TransitionKind::Mix,
        TransitionKind::Push,
        TransitionKind::Wipe,
        TransitionKind::Slide,
        TransitionKind::Split,
        TransitionKind::Circle,
        TransitionKind::Dip,
        TransitionKind::VWipe,
    ];

    #[test]
    fn length_is_always_preserved() {
        let (a, b) = (solid(10), solid(200));
        let mut out = Vec::new();
        for k in ALL {
            for i in 0..=20 {
                compose(&a, &b, W, H, k, i as f32 / 20.0, &mut out);
                assert_eq!(out.len(), W * H * 3, "{:?} p={}", k, i);
            }
        }
    }

    #[test]
    fn endpoints_match_inputs() {
        let (a, b) = (solid(10), solid(200));
        let mut out = Vec::new();
        for k in ALL {
            compose(&a, &b, W, H, k, 0.0, &mut out);
            assert_eq!(out, a, "{:?} p=0", k);
            compose(&a, &b, W, H, k, 1.0, &mut out);
            assert_eq!(out, b, "{:?} p=1", k);
        }
    }

    #[test]
    fn mix_midpoint_is_average() {
        let (a, b) = (solid(0), solid(200));
        let mut out = Vec::new();
        compose(&a, &b, W, H, TransitionKind::Mix, 0.5, &mut out);
        assert!(out.iter().all(|&v| (99..=101).contains(&v)));
    }

    #[test]
    fn wipe_splits_left_right() {
        let (a, b) = (solid(0), solid(255));
        let mut out = Vec::new();
        compose(&a, &b, W, H, TransitionKind::Wipe, 0.5, &mut out);
        assert_eq!(out[0], 255); // 左侧是新画面
        assert_eq!(out[(W - 1) * 3], 0); // 右侧仍是旧画面
    }

    #[test]
    fn circle_center_is_new_corner_is_old() {
        let (a, b) = (solid(0), solid(255));
        let mut out = Vec::new();
        compose(&a, &b, W, H, TransitionKind::Circle, 0.3, &mut out);
        let center = ((H / 2) * W + W / 2) * 3;
        assert_eq!(out[center], 255);
        assert_eq!(out[0], 0);
    }

    #[test]
    fn dip_goes_through_black() {
        let (a, b) = (solid(200), solid(200));
        let mut out = Vec::new();
        compose(&a, &b, W, H, TransitionKind::Dip, 0.5, &mut out);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn mismatched_input_does_not_panic() {
        let mut out = Vec::new();
        compose(&[1, 2, 3], &solid(5), W, H, TransitionKind::Mix, 0.5, &mut out);
        assert_eq!(out.len(), W * H * 3);
    }

    #[test]
    fn fade_scales_brightness() {
        let mut f = solid(200);
        apply_fade(&mut f, 0.5);
        assert!(f.iter().all(|&v| (99..=101).contains(&v)));
        apply_fade(&mut f, 0.0);
        assert!(f.iter().all(|&v| v == 0));
    }
}

