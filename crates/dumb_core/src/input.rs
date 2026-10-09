use glam::Vec2;

/// Keys the engine knows about. Kept as a plain `repr(u16)` enum so it is ABI-stable for scripts.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,
    Space, Enter, Escape, Tab, Backspace, Delete,
    Left, Right, Up, Down,
    LShift, RShift, LCtrl, RCtrl, LAlt, RAlt,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
}

pub const KEY_COUNT: usize = Key::F12 as usize + 1;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Snapshot of input state for the current frame.
#[repr(C)]
#[derive(Clone, Debug)]
pub struct Input {
    down: [bool; KEY_COUNT],
    pressed: [bool; KEY_COUNT],
    released: [bool; KEY_COUNT],
    mouse_down: [bool; 3],
    mouse_pressed: [bool; 3],
    pub mouse_pos: Vec2,
    pub mouse_delta: Vec2,
    pub scroll: f32,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            down: [false; KEY_COUNT],
            pressed: [false; KEY_COUNT],
            released: [false; KEY_COUNT],
            mouse_down: [false; 3],
            mouse_pressed: [false; 3],
            mouse_pos: Vec2::ZERO,
            mouse_delta: Vec2::ZERO,
            scroll: 0.0,
        }
    }
}

impl Input {
    pub fn key_down(&self, k: Key) -> bool {
        self.down[k as usize]
    }

    pub fn key_pressed(&self, k: Key) -> bool {
        self.pressed[k as usize]
    }

    pub fn key_released(&self, k: Key) -> bool {
        self.released[k as usize]
    }

    pub fn mouse_down(&self, b: MouseButton) -> bool {
        self.mouse_down[b as usize]
    }

    pub fn mouse_pressed(&self, b: MouseButton) -> bool {
        self.mouse_pressed[b as usize]
    }

    /// -1..1 axis from two keys.
    pub fn axis(&self, neg: Key, pos: Key) -> f32 {
        (self.key_down(pos) as i32 - self.key_down(neg) as i32) as f32
    }

    pub fn set_key(&mut self, k: Key, down: bool) {
        let i = k as usize;
        if down && !self.down[i] {
            self.pressed[i] = true;
        }
        if !down && self.down[i] {
            self.released[i] = true;
        }
        self.down[i] = down;
    }

    pub fn set_mouse(&mut self, b: MouseButton, down: bool) {
        let i = b as usize;
        if down && !self.mouse_down[i] {
            self.mouse_pressed[i] = true;
        }
        self.mouse_down[i] = down;
    }

    /// Clear per-frame edges. Call after the frame has been simulated.
    pub fn end_frame(&mut self) {
        self.pressed = [false; KEY_COUNT];
        self.released = [false; KEY_COUNT];
        self.mouse_pressed = [false; 3];
        self.mouse_delta = Vec2::ZERO;
        self.scroll = 0.0;
    }

    pub fn clear(&mut self) {
        *self = Input::default();
    }
}
