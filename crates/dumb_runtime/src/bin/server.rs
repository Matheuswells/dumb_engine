//! Headless game server: `dumb-server [project_dir] [scene] [--hz N] [--ticks N] [--listen ADDR]
//! [--save FILE] [--save-every TICKS]`.
//!
//! Runs a project's simulation (scripts, physics, AI, world streaming) with no window or GPU.
//! Assets load as data only: models for colliders and animation, scenes for streaming.
//! The simulation advances in fixed ticks (`--hz`, default 30), whatever the machine's speed:
//! a tick that runs long delays the next one rather than making it bigger.
//!
//! Streaming and AI LOD follow the `StreamFocus` resource (one point per player, set by the
//! game), or a camera in the scene when there is none. Ctrl+C or SIGTERM stops it cleanly.
//!
//! With `--listen` (e.g. `0.0.0.0:7777`) the server accepts clients and replicates every entity
//! with a `Replicated` component (see `dumb_runtime::net`). `--save` writes the replicated world
//! periodically and on shutdown, and restores it at startup, so the world survives restarts.

use dumb_asset::AssetDatabase;
use dumb_core::{Input, Time};
use dumb_ecs::World;
use dumb_runtime::{request_sim_assets, simulate, FrameLimiter, Project, SimStats};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the server logs its status.
const REPORT_EVERY: Duration = Duration::from_secs(10);

struct Args {
    project: std::path::PathBuf,
    scene: Option<String>,
    hz: u32,
    /// Stop after this many ticks (0 = run until stopped). For smoke tests and benchmarks.
    ticks: u64,
    /// UDP address to accept clients on (none = no networking).
    listen: Option<String>,
    save: Option<std::path::PathBuf>,
    /// World save interval in ticks.
    save_every: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { project: "project".into(), scene: None, hz: 30, ticks: 0, listen: None, save: None, save_every: 1800 };
    // An exported game has project.ron next to the executable.
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).filter(|d| d.join("project.ron").exists()) {
        args.project = d;
    }
    let mut positional = 0;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut number = |name: &str| it.next().and_then(|v| v.parse::<u64>().ok()).ok_or(format!("{name} needs a number"));
        match a.as_str() {
            // At least 4: `Time::advance` caps a step at 0.25 s, so slower ticks would run game
            // time slower than real time.
            "--hz" => args.hz = number("--hz")?.clamp(4, 1000) as u32,
            "--ticks" => args.ticks = number("--ticks")?,
            "--save-every" => args.save_every = number("--save-every")?,
            "--listen" => args.listen = Some(it.next().ok_or("--listen needs an address")?),
            "--save" => args.save = Some(it.next().ok_or("--save needs a file")?.into()),
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => {
                match positional {
                    0 => args.project = a.into(),
                    1 => args.scene = Some(a),
                    _ => return Err(format!("unexpected argument {a}")),
                }
                positional += 1;
            }
        }
    }
    Ok(args)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!("usage: dumb-server [project_dir] [scene] [--hz N] [--ticks N] [--listen ADDR] [--save FILE] [--save-every TICKS]");
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    };

    // Nothing reads the profiler here and nothing calls `profiler::end_frame`, so recorded
    // scopes would pile up in the current frame forever.
    dumb_core::profiler::set_enabled(false);

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)) {
            log::warn!("no Ctrl+C handler: {e}");
        }
    }

    let project = Project::open(&args.project);
    let mut db = match AssetDatabase::open(&project.root) {
        Ok(db) => db,
        Err(e) => {
            log::error!("cannot open the project at {}: {e}", project.root.display());
            std::process::exit(1);
        }
    };
    let mut world = World::new();
    let mut scripts = project.script_host();
    if let Err(e) = scripts.load(&mut [&mut world]) {
        log::warn!("scripts: {e}");
    }
    let scene = args.scene.clone().unwrap_or(project.settings.startup_scene.clone());
    if !dumb_runtime::load_scene(&db, &mut world, &scene) {
        scripts.unload(&mut [&mut world]);
        std::process::exit(1);
    }
    db.update();
    let mut physics = dumb_physics::Physics::new(dumb_core::Vec3::from(project.settings.gravity));
    project.configure_physics(&mut physics);
    let mut streamer = dumb_runtime::streaming::Streamer::default();
    let (mut hud, mut meshes) = (dumb_ecs::Hud::default(), dumb_ecs::MeshUploads::default());
    let (mut time, input) = (Time::new(), Input::default());
    let mut limiter = FrameLimiter::default();
    let tick_dt = 1.0 / args.hz as f32;
    log::info!("serving `{}` ({scene}) at {} Hz, {} entities", project.settings.name, args.hz, world.entity_count());

    // Networking: the server sees `Replicated` entities and raises connection events.
    let mut net = args.listen.as_deref().map(|addr| {
        let socket = dumb_net::bind_udp(addr).unwrap_or_else(|e| {
            log::error!("cannot listen on {addr}: {e}");
            std::process::exit(1);
        });
        let cfg = dumb_runtime::net::ServerConfig { hz: args.hz, save_path: args.save.clone(), save_every: args.save_every, ..Default::default() };
        let mut n = dumb_runtime::net::ServerNet::new(socket, cfg, &mut world);
        n.restore(&mut world);
        log::info!("listening on {addr}");
        n
    });
    let started = Instant::now();

    let mut report = Report::default();
    let mut tick: u64 = 0;
    while !stop.load(Ordering::SeqCst) && (args.ticks == 0 || tick < args.ticks) {
        let t0 = Instant::now();
        db.update();
        scripts.update(&mut [&mut world]);
        request_sim_assets(&world, &mut db);
        time.advance(tick_dt);
        if let Some(n) = net.as_mut() {
            n.pre_tick(&mut world, started.elapsed().as_secs_f64());
        }
        let sim = simulate(&mut world, &mut scripts, &db, &time, &input, Some(&mut physics), Some(&mut streamer), &project.settings, &mut hud, &mut meshes);
        if let Some(n) = net.as_mut() {
            n.post_tick(&mut world);
        }
        db.apply_mesh_uploads(&mut meshes);
        // No web panels to run scripts' JavaScript in: drop it so it doesn't pile up.
        hud.web_eval.clear();
        tick += 1;
        report.add(t0.elapsed(), tick_dt);
        if report.since.elapsed() >= REPORT_EVERY {
            report.log(tick, &world, &sim);
        }
        limiter.wait(args.hz);
    }

    log::info!("stopping after {tick} ticks");
    if let Some(n) = &net {
        n.save_now(&world);
    }
    scripts.unload(&mut [&mut world]);
}

/// Tick timings between status lines.
struct Report {
    since: Instant,
    ticks: u32,
    busy: Duration,
    worst: Duration,
    /// Ticks that took longer than their slot (the server is falling behind).
    late: u32,
}

impl Default for Report {
    fn default() -> Self {
        Report { since: Instant::now(), ticks: 0, busy: Duration::ZERO, worst: Duration::ZERO, late: 0 }
    }
}

impl Report {
    fn add(&mut self, took: Duration, slot: f32) {
        self.ticks += 1;
        self.busy += took;
        self.worst = self.worst.max(took);
        self.late += (took.as_secs_f32() > slot) as u32;
    }

    fn log(&mut self, tick: u64, world: &World, sim: &SimStats) {
        let secs = self.since.elapsed().as_secs_f32();
        let avg = self.busy.as_secs_f32() * 1000.0 / self.ticks.max(1) as f32;
        let mut line = format!(
            "tick {tick}: {:.1} ticks/s, {avg:.2} ms avg, {:.2} ms worst, {} entities",
            self.ticks as f32 / secs,
            self.worst.as_secs_f32() * 1000.0,
            world.entity_count()
        );
        if sim.ai.agents > 0 {
            line += &format!(", AI {} agents ({} asleep)", sim.ai.agents, sim.ai.sleeping);
        }
        if sim.stream.cells > 0 {
            line += &format!(", streaming {}/{} cells", sim.stream.loaded, sim.stream.cells);
        }
        if self.late > 0 {
            log::warn!("{line}, {} ticks over budget", self.late);
        } else {
            log::info!("{line}");
        }
        *self = Report::default();
    }
}
