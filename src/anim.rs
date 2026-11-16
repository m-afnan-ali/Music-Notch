//! Tiny animation helpers.

/// A slightly under-damped spring. Gives motion that feels physical: quick to
/// start, soft to land, with a hint of overshoot.
#[derive(Clone, Copy)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    vel: f32,
    k: f32,
    d: f32,
}

impl Spring {
    pub fn new(value: f32, stiffness: f32) -> Self {
        Self { value, target: value, vel: 0.0, k: stiffness, d: 2.0 * stiffness.sqrt() * 0.82 }
    }

    /// Advance the simulation. Returns `true` while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.settled() {
            return false;
        }
        // Sub-step for stability when frames are long.
        let n = ((dt / 0.008).ceil() as i32).clamp(1, 8);
        let h = dt / n as f32;
        for _ in 0..n {
            let a = self.k * (self.target - self.value) - self.d * self.vel;
            self.vel += a * h;
            self.value += self.vel * h;
        }
        if (self.target - self.value).abs() < 1e-3 && self.vel.abs() < 1e-3 {
            self.snap();
            return false;
        }
        true
    }

    pub fn settled(&self) -> bool {
        self.value == self.target && self.vel == 0.0
    }

    pub fn snap(&mut self) {
        self.value = self.target;
        self.vel = 0.0;
    }

    pub fn reset(&mut self, value: f32, target: f32) {
        self.value = value;
        self.target = target;
        self.vel = 0.0;
    }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

pub fn ease(t: f32) -> f32 {
    let t = clamp01(t);
    t * t * (3.0 - 2.0 * t)
}
