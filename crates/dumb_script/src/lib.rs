//! Rust scripting through dynamically loaded libraries.
//!
//! A game crate is a `cdylib` that depends on `dumb_script` and exports a registration
//! function with [`export_plugin!`]. The editor loads it, registers its components (they show
//! up in the inspector immediately), and runs its systems while playing. When the library is
//! rebuilt the host:
//!
//! 1. converts every plugin component in every world to a reflected value tree,
//! 2. unloads the old library,
//! 3. loads a shadow copy of the new one (so cargo can keep writing the original),
//! 4. re-registers the components and restores the values (fields matched by name).
//!
//! ```ignore
//! use dumb_script::prelude::*;
//!
//! #[derive(Component, Editor, Default)]
//! pub struct Spinner { #[editor(range = 0.0..=720.0)] pub degrees_per_second: f32 }
//!
//! fn spin(ctx: &mut ScriptContext) {
//!     let dt = ctx.time.delta;
//!     for (t, s) in ctx.world.query::<(&mut Transform, &Spinner)>() {
//!         t.rotation *= Quat::from_rotation_y(s.degrees_per_second.to_radians() * dt);
//!     }
//! }
//!
//! fn register(reg: &mut Registry) {
//!     reg.component::<Spinner>();
//!     reg.system("spin", spin);
//! }
//! dumb_script::export_plugin!(register);
//! ```

mod host;
pub mod project;

pub use host::{ScriptHost, ScriptStatus};

use dumb_core::{AssetId, Input, Time};
use dumb_ecs::{Component, ComponentDescriptor, World};

pub mod prelude {
    pub use crate::{export_plugin, Registry, ScriptContext};
    pub use dumb_core::physics::{CollisionEvent, PhysicsQuery, RayHit};
    /// Logging: `info!("hp = {}", hp)` shows in the editor console with its level.
    pub use log::{debug, error, info, trace, warn};
    pub use dumb_core::{builtin, AssetId, Color, Entity, EulerRot, Input, Key, Mat4, MouseButton, Quat, Time, Vec2, Vec3, Vec4};
    pub use dumb_ecs::{
        AiAgent, Anchor, Animator, BodyType, Camera, CharacterController, Commands, Hud, HudPos, StreamingCell, UiBar, UiButton, UiImage,
        UiPanel, UiText, UiWeb, UiWorldLabel, Collider, ColliderShape, Component, Editor, Light, LightKind, MeshRenderer, Name,
        Parent, RigidBody, Transform, With, Without, World,
    };
}

/// Version string that host and plugin must agree on.
pub fn abi_string() -> String {
    format!("{}|{}", dumb_core::ENGINE_ABI, env!("DUMB_RUSTC_VERSION"))
}

/// What a system gets each frame.
pub struct ScriptContext<'a> {
    pub world: &'a mut World,
    pub time: &'a Time,
    pub input: &'a Input,
    /// Resolve a project asset path (`"Characters/Player/knight.glb"`) to its id.
    pub assets: &'a dyn Fn(&str) -> Option<AssetId>,
    /// Ray casts, overlaps and collision events (empty when physics is not running).
    pub physics: &'a dyn dumb_core::physics::PhysicsQuery,
    /// Screen UI drawn this frame: text, bars, images, buttons, web pages (see `Hud`).
    pub hud: &'a mut dumb_ecs::Hud,
}

pub type SystemFn = fn(&mut ScriptContext);

/// Runs a system inside the plugin binary, catching panics on the plugin side of the boundary.
pub type SystemRunner = fn(SystemFn, &mut ScriptContext) -> Result<(), String>;

#[derive(Clone)]
pub struct SystemDesc {
    pub name: String,
    pub func: SystemFn,
    pub runner: SystemRunner,
}

/// (level, target, message, source file, line) — file/line let the editor show logs on their source line.
pub type HostLogFn = fn(log::Level, &str, &str, &str, u32);

/// Filled by the plugin's registration function.
pub struct Registry {
    pub components: Vec<ComponentDescriptor>,
    pub systems: Vec<SystemDesc>,
    pub plugin_name: String,
    pub host_log: HostLogFn,
}

fn run_guarded(f: SystemFn, ctx: &mut ScriptContext) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(ctx))).map_err(|e| {
        e.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into())
    })
}

impl Registry {
    pub fn new(plugin_name: &str, host_log: HostLogFn) -> Self {
        Registry { components: Vec::new(), systems: Vec::new(), plugin_name: plugin_name.into(), host_log }
    }

    /// Register a component type so it can be stored, edited and serialized.
    pub fn component<T: Component>(&mut self) {
        let mut d = ComponentDescriptor::of::<T>();
        d.source = Some(self.plugin_name.clone());
        self.components.push(d);
    }

    /// Register a system that runs every frame while the game is playing, in registration order.
    pub fn system(&mut self, name: &str, func: SystemFn) {
        self.systems.push(SystemDesc { name: name.into(), func, runner: run_guarded });
    }
}

struct ForwardLogger(HostLogFn);

impl log::Log for ForwardLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        (self.0)(r.level(), r.target(), &r.args().to_string(), r.file().unwrap_or(""), r.line().unwrap_or(0));
    }
    fn flush(&self) {}
}

/// Route the plugin's `log` macros to the host. Called by [`export_plugin!`].
#[doc(hidden)]
pub fn install_forward_logger(f: HostLogFn) {
    let logger: &'static ForwardLogger = Box::leak(Box::new(ForwardLogger(f)));
    if log::set_logger(logger).is_ok() {
        // Everything is forwarded; the editor console filters by level.
        log::set_max_level(log::LevelFilter::Trace);
    }
}

/// Export the plugin entry points. Pass a `fn(&mut Registry)`.
#[macro_export]
macro_rules! export_plugin {
    ($register:path) => {
        /// # Safety
        /// `out_len` must be valid for a write.
        #[no_mangle]
        pub unsafe extern "C" fn dumb_plugin_abi(out_len: *mut usize) -> *const u8 {
            static ABI: ::std::sync::OnceLock<String> = ::std::sync::OnceLock::new();
            let s = ABI.get_or_init($crate::abi_string);
            unsafe { *out_len = s.len() };
            s.as_ptr()
        }

        #[no_mangle]
        pub fn dumb_plugin_register(reg: &mut $crate::Registry) {
            $crate::install_forward_logger(reg.host_log);
            $register(reg);
        }
    };
}
