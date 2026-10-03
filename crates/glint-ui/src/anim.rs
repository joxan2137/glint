//! Springs, tweens and `Animated<T>`.
//!
//! Animations read a per-thread frame clock that the event loop (or an offscreen render) sets before every
//! dispatch. Reading an `Animated` value that is still moving, or retargeting one, marks the current window as
//! needing another frame, so views never schedule animation frames by hand.

use std::cell::Cell;

use glint_core::{PointF, RectF, SizeF};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::color::Color;

thread_local! {
    static CLOCK: Cell<f64> = const { Cell::new(0.0) };
    static MOTION_PENDING: Cell<bool> = const { Cell::new(false) };
    static REDUCED_MOTION: Cell<bool> = const { Cell::new(false) };
}

/// Seconds since an arbitrary fixed point, from QueryPerformanceCounter.
pub fn precise_time() -> f64 {
    let mut counter = 0i64;
    let mut frequency = 1i64;
    // SAFETY: both calls only write to the provided integers and cannot fail on Windows XP or later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    counter as f64 / frequency.max(1) as f64
}

/// The current frame time used by every `Animated` on this thread.
pub fn now() -> f64 {
    CLOCK.with(Cell::get)
}

/// Sets the animation clock. The event loop calls this before dispatching; offscreen renders use it for
/// deterministic stepping.
pub fn set_clock(time: f64) {
    CLOCK.with(|c| c.set(time));
}

/// Runs `f` with the animation clock set to `time`, restoring the previous value afterwards.
pub fn with_clock<R>(time: f64, f: impl FnOnce() -> R) -> R {
    let previous = now();
    set_clock(time);
    let result = f();
    set_clock(previous);
    result
}

pub(crate) fn take_motion_pending() -> bool {
    MOTION_PENDING.with(|m| m.replace(false))
}

pub(crate) fn mark_motion_pending() {
    MOTION_PENDING.with(|m| m.set(true));
}

/// True when the user turned off "Show animations in Windows" (SPI_GETCLIENTAREAANIMATION).
/// Per UI thread; the event loop keeps it in sync with the system setting.
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.with(Cell::get)
}

pub fn set_reduced_motion(reduced: bool) {
    REDUCED_MOTION.with(|r| r.set(reduced));
}

/// Damped harmonic spring with unit mass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f32,
    pub damping_ratio: f32,
}

impl Spring {
    /// Anything that moves or resizes.
    pub const DEFAULT: Spring = Spring { stiffness: 420.0, damping_ratio: 0.86 };
    /// Hover highlights and pointer-following morphs.
    pub const SNAPPY: Spring = Spring { stiffness: 700.0, damping_ratio: 0.9 };

    fn omega(&self) -> f64 {
        (self.stiffness.max(1e-3) as f64).sqrt()
    }

    /// Unit responses at time `t`: `(f, f', g, g')` where `x(t) = target + d0 * f + v0 * g`.
    fn response(&self, t: f64) -> (f64, f64, f64, f64) {
        let w = self.omega();
        let z = self.damping_ratio.max(0.0) as f64;
        if (z - 1.0).abs() < 1e-4 {
            let e = (-w * t).exp();
            return (e * (1.0 + w * t), -w * w * t * e, t * e, e * (1.0 - w * t));
        }
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let e = (-z * w * t).exp();
            let (sin, cos) = (wd * t).sin_cos();
            let k = z * w / wd;
            return (e * (cos + k * sin), -e * (w * w / wd) * sin, e * sin / wd, e * (cos - k * sin));
        }
        let root = (z * z - 1.0).sqrt();
        let r1 = -w * (z - root);
        let r2 = -w * (z + root);
        let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
        let span = r1 - r2;
        (
            (r1 * e2 - r2 * e1) / span,
            (r1 * r2 * e2 - r2 * r1 * e1) / span,
            (e1 - e2) / span,
            (r1 * e1 - r2 * e2) / span,
        )
    }

    /// Time after which the displacement stays below `epsilon`.
    fn settle_time(&self, displacement: f64, velocity: f64, epsilon: f64) -> f64 {
        let w = self.omega();
        let z = self.damping_ratio.max(0.05) as f64;
        let (rate, amplitude) = if z < 1.0 - 1e-4 {
            let wd = w * (1.0 - z * z).sqrt();
            (z * w, displacement * w / wd + velocity / wd)
        } else if z <= 1.0 + 1e-4 {
            (0.75 * w, 2.0 * (displacement + velocity / w))
        } else {
            let slow = w * (z - (z * z - 1.0).sqrt());
            (slow, 2.0 * (displacement + velocity / slow))
        };
        if amplitude <= epsilon { 0.0 } else { (amplitude / epsilon).ln() / rate }
    }
}

/// CSS-style `cubic-bezier(x1, y1, x2, y2)` easing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    /// DESIGN §5 fades: `cubic-bezier(0.2, 0, 0, 1)`.
    pub const STANDARD: CubicBezier = CubicBezier { x1: 0.2, y1: 0.0, x2: 0.0, y2: 1.0 };
    pub const LINEAR: CubicBezier = CubicBezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 };
    pub const EASE_IN_OUT: CubicBezier = CubicBezier { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 };

    fn coordinate(a: f32, b: f32, s: f32) -> f32 {
        let c = 3.0 * a;
        let bb = 3.0 * (b - a) - c;
        let aa = 1.0 - c - bb;
        ((aa * s + bb) * s + c) * s
    }

    fn slope(a: f32, b: f32, s: f32) -> f32 {
        let c = 3.0 * a;
        let bb = 3.0 * (b - a) - c;
        let aa = 1.0 - c - bb;
        (3.0 * aa * s + 2.0 * bb) * s + c
    }

    /// Eased progress for linear progress `x` in 0..=1.
    pub fn ease(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let mut s = x;
        for _ in 0..8 {
            let error = Self::coordinate(self.x1, self.x2, s) - x;
            if error.abs() < 1e-6 {
                return Self::coordinate(self.y1, self.y2, s);
            }
            let d = Self::slope(self.x1, self.x2, s);
            if d.abs() < 1e-6 {
                break;
            }
            s -= error / d;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        s = x;
        for _ in 0..32 {
            let value = Self::coordinate(self.x1, self.x2, s);
            if (value - x).abs() < 1e-6 {
                break;
            }
            if value < x {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) * 0.5;
        }
        Self::coordinate(self.y1, self.y2, s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    pub duration: f32,
    pub curve: CubicBezier,
}

impl Tween {
    /// 140 ms `cubic-bezier(0.2, 0, 0, 1)`.
    pub const FADE: Tween = Tween { duration: 0.14, curve: CubicBezier::STANDARD };
    /// 120 ms disappear fade.
    pub const FADE_OUT: Tween = Tween { duration: 0.12, curve: CubicBezier::STANDARD };
    /// ≈100 ms hover/press transitions.
    pub const HOVER: Tween = Tween { duration: 0.10, curve: CubicBezier::STANDARD };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Spring(Spring),
    Tween(Tween),
}

impl Default for Motion {
    fn default() -> Self {
        Motion::Spring(Spring::DEFAULT)
    }
}

/// A value that can be animated component-wise (up to four f32 components).
pub trait Animatable: Copy + PartialEq {
    fn to_components(self) -> [f32; 4];
    fn from_components(c: [f32; 4]) -> Self;
}

impl Animatable for f32 {
    fn to_components(self) -> [f32; 4] {
        [self, 0.0, 0.0, 0.0]
    }
    fn from_components(c: [f32; 4]) -> Self {
        c[0]
    }
}

impl Animatable for PointF {
    fn to_components(self) -> [f32; 4] {
        [self.x, self.y, 0.0, 0.0]
    }
    fn from_components(c: [f32; 4]) -> Self {
        PointF::new(c[0], c[1])
    }
}

impl Animatable for SizeF {
    fn to_components(self) -> [f32; 4] {
        [self.w, self.h, 0.0, 0.0]
    }
    fn from_components(c: [f32; 4]) -> Self {
        SizeF::new(c[0], c[1])
    }
}

impl Animatable for RectF {
    fn to_components(self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }
    fn from_components(c: [f32; 4]) -> Self {
        RectF::new(c[0], c[1], c[2], c[3])
    }
}

/// Colors animate in premultiplied space.
impl Animatable for Color {
    fn to_components(self) -> [f32; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }
    fn from_components(c: [f32; 4]) -> Self {
        let a = c[3].clamp(0.0, 1.0);
        if a <= 1e-6 {
            return Color::TRANSPARENT;
        }
        Color::rgba((c[0] / a).clamp(0.0, 1.0), (c[1] / a).clamp(0.0, 1.0), (c[2] / a).clamp(0.0, 1.0), a)
    }
}

const SETTLE_EPSILON: f64 = 0.004;

/// A value that animates toward its target with a spring (velocity carried over on retarget) or a tween.
#[derive(Clone, Debug)]
pub struct Animated<T: Animatable> {
    from: [f32; 4],
    velocity: [f32; 4],
    to: T,
    start: f64,
    settle: f64,
    motion: Motion,
}

impl<T: Animatable> Animated<T> {
    pub fn new(value: T) -> Self {
        Self::with_motion(value, Motion::default())
    }

    pub fn snappy(value: T) -> Self {
        Self::with_motion(value, Motion::Spring(Spring::SNAPPY))
    }

    pub fn fade(value: T) -> Self {
        Self::with_motion(value, Motion::Tween(Tween::FADE))
    }

    pub fn with_motion(value: T, motion: Motion) -> Self {
        Self { from: value.to_components(), velocity: [0.0; 4], to: value, start: 0.0, settle: 0.0, motion }
    }

    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// Changes how future retargets animate; the current motion continues unchanged.
    pub fn set_motion(&mut self, motion: Motion) {
        if self.is_animating() {
            let t = now();
            let (value, velocity) = self.sample(t);
            self.from = value;
            self.velocity = velocity;
            self.start = t;
            self.motion = motion;
            self.settle = self.compute_settle();
        } else {
            self.motion = motion;
        }
    }

    pub fn target(&self) -> T {
        self.to
    }

    /// Animates toward `target` from the current value (snaps when reduced motion is on).
    pub fn set(&mut self, target: T) {
        if target == self.to {
            return;
        }
        if reduced_motion() {
            self.snap(target);
            mark_motion_pending();
            return;
        }
        let t = now();
        let (value, velocity) = self.sample(t);
        self.from = value;
        self.velocity = match self.motion {
            Motion::Spring(_) => velocity,
            Motion::Tween(_) => [0.0; 4],
        };
        self.to = target;
        self.start = t;
        self.settle = self.compute_settle();
        mark_motion_pending();
    }

    /// Animates with a one-off motion.
    pub fn set_with(&mut self, target: T, motion: Motion) {
        self.set_motion(motion);
        self.set(target);
    }

    /// Jumps to `value` with no motion.
    pub fn snap(&mut self, value: T) {
        self.from = value.to_components();
        self.velocity = [0.0; 4];
        self.to = value;
        self.settle = 0.0;
        self.start = now();
    }

    /// Current value at the frame clock; requests another frame while still moving.
    pub fn get(&self) -> T {
        let t = now();
        if t - self.start >= self.settle {
            return self.to;
        }
        mark_motion_pending();
        T::from_components(self.sample(t).0)
    }

    pub fn is_animating(&self) -> bool {
        now() - self.start < self.settle
    }

    /// Current velocity per second.
    pub fn velocity(&self) -> T {
        T::from_components(self.sample(now()).1)
    }

    fn compute_settle(&self) -> f64 {
        let to = self.to.to_components();
        match self.motion {
            Motion::Tween(tween) => {
                if self.from == to { 0.0 } else { tween.duration.max(0.0) as f64 }
            }
            Motion::Spring(spring) => (0..4)
                .map(|i| {
                    let d = (self.from[i] - to[i]).abs() as f64;
                    let v = self.velocity[i].abs() as f64;
                    spring.settle_time(d, v, SETTLE_EPSILON)
                })
                .fold(0.0, f64::max),
        }
    }

    fn sample(&self, t: f64) -> ([f32; 4], [f32; 4]) {
        let to = self.to.to_components();
        let elapsed = (t - self.start).max(0.0);
        if elapsed >= self.settle {
            return (to, [0.0; 4]);
        }
        let mut value = [0.0f32; 4];
        let mut velocity = [0.0f32; 4];
        match self.motion {
            Motion::Spring(spring) => {
                let (f, df, g, dg) = spring.response(elapsed);
                for i in 0..4 {
                    let d0 = (self.from[i] - to[i]) as f64;
                    let v0 = self.velocity[i] as f64;
                    value[i] = (to[i] as f64 + d0 * f + v0 * g) as f32;
                    velocity[i] = (d0 * df + v0 * dg) as f32;
                }
            }
            Motion::Tween(tween) => {
                let duration = tween.duration.max(1e-4);
                let x = (elapsed as f32 / duration).clamp(0.0, 1.0);
                let eased = tween.curve.ease(x);
                let slope = (tween.curve.ease((x + 0.001).min(1.0)) - tween.curve.ease((x - 0.001).max(0.0)))
                    / (0.002 * duration);
                for i in 0..4 {
                    value[i] = self.from[i] + (to[i] - self.from[i]) * eased;
                    velocity[i] = (to[i] - self.from[i]) * slope;
                }
            }
        }
        (value, velocity)
    }
}

impl<T: Animatable + Default> Default for Animated<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// Linear interpolation helper.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step_until_settled<T: Animatable>(anim: &Animated<T>, start: f64, limit: f64) -> f64 {
        let mut t = start;
        while t < start + limit {
            set_clock(t);
            if !anim.is_animating() {
                return t - start;
            }
            t += 1.0 / 240.0;
        }
        f64::INFINITY
    }

    #[test]
    fn default_spring_converges_quickly_without_big_overshoot() {
        set_reduced_motion(false);
        set_clock(10.0);
        let mut a = Animated::new(0.0f32);
        a.set(100.0);
        let mut peak = 0.0f32;
        let mut t = 10.0;
        while t < 11.0 {
            set_clock(t);
            peak = peak.max(a.get());
            t += 1.0 / 240.0;
        }
        set_clock(11.0);
        assert_eq!(a.get(), 100.0);
        assert!(peak < 101.5, "overshoot {peak}");
        set_clock(10.0);
        a.snap(0.0);
        a.set(100.0);
        let settle = step_until_settled(&a, 10.0, 2.0);
        assert!(settle < 0.75, "settle {settle}");
    }

    #[test]
    fn spring_matches_numeric_integration() {
        let spring = Spring::DEFAULT;
        let (mut x, mut v) = (1.0f64, 0.0f64);
        let dt = 1e-5;
        let mut t = 0.0;
        while t < 0.2 {
            let a = -(spring.stiffness as f64) * x - 2.0 * spring.damping_ratio as f64 * spring.omega() * v;
            v += a * dt;
            x += v * dt;
            t += dt;
        }
        let (f, ..) = spring.response(t);
        assert!((f - x).abs() < 1e-3, "{f} vs {x}");
    }

    #[test]
    fn retarget_carries_velocity() {
        set_reduced_motion(false);
        set_clock(0.0);
        let mut a = Animated::new(0.0f32);
        a.set(100.0);
        set_clock(0.05);
        let moving = a.velocity();
        assert!(moving > 0.0);
        a.set(200.0);
        assert!((a.velocity() - moving).abs() < 1e-2, "velocity must carry over");
        set_clock(3.0);
        assert_eq!(a.get(), 200.0);
    }

    #[test]
    fn critically_and_over_damped_settle() {
        for ratio in [1.0f32, 1.4] {
            let spring = Spring { stiffness: 400.0, damping_ratio: ratio };
            set_clock(0.0);
            let mut a = Animated::with_motion(0.0f32, Motion::Spring(spring));
            a.set(1.0);
            let settle = step_until_settled(&a, 0.0, 3.0);
            assert!(settle.is_finite());
            set_clock(settle);
            assert!((a.get() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn tween_follows_curve() {
        set_reduced_motion(false);
        set_clock(0.0);
        let mut a = Animated::fade(0.0f32);
        a.set(1.0);
        set_clock(0.07);
        let mid = a.get();
        assert!(mid > 0.5 && mid < 1.0, "standard easing front-loads progress: {mid}");
        set_clock(0.14);
        assert_eq!(a.get(), 1.0);
    }

    #[test]
    fn bezier_endpoints_and_linear() {
        let c = CubicBezier::STANDARD;
        assert_eq!(c.ease(0.0), 0.0);
        assert_eq!(c.ease(1.0), 1.0);
        for i in 1..10 {
            let x = i as f32 / 10.0;
            assert!((CubicBezier::LINEAR.ease(x) - x).abs() < 1e-4);
        }
        let mut last = 0.0;
        for i in 1..=100 {
            let y = c.ease(i as f32 / 100.0);
            assert!(y >= last - 1e-5);
            last = y;
        }
    }

    #[test]
    fn rect_and_color_animate_componentwise() {
        set_reduced_motion(false);
        set_clock(0.0);
        let mut r = Animated::new(RectF::new(0.0, 0.0, 10.0, 10.0));
        r.set(RectF::new(100.0, 50.0, 20.0, 40.0));
        set_clock(5.0);
        assert_eq!(r.get(), RectF::new(100.0, 50.0, 20.0, 40.0));
        set_clock(0.0);
        let mut c = Animated::fade(Color::rgba(1.0, 0.0, 0.0, 1.0));
        c.set(Color::rgba(0.0, 0.0, 1.0, 1.0));
        set_clock(1.0);
        assert_eq!(c.get(), Color::rgba(0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn reduced_motion_snaps() {
        set_clock(0.0);
        set_reduced_motion(true);
        let mut a = Animated::new(0.0f32);
        a.set(5.0);
        assert!(!a.is_animating());
        assert_eq!(a.get(), 5.0);
        set_reduced_motion(false);
    }
}
