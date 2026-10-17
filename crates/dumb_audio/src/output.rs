//! Sound device output. ALSA is loaded at runtime (`libasound.so.2`), so the engine builds and
//! runs without audio libraries installed; a missing device just means silence.

use crate::mixer::Mixer;
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_FORMAT_FLOAT_LE: c_int = 14;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;
const PERIOD_FRAMES: usize = 1024;

type Open = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
type SetParams = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
type WriteI = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
type Recover = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;

struct Pcm(*mut c_void);
// The handle is only ever used by the playback thread that owns it.
unsafe impl Send for Pcm {}

/// A running output device. Dropping it stops the playback thread.
pub struct Device {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Device {
    /// Open the default ALSA device and start pulling audio from `mixer`.
    pub fn open(mixer: Arc<Mutex<Mixer>>, rate: u32) -> Result<Device, String> {
        unsafe {
            let lib = libloading::Library::new("libasound.so.2").map_err(|e| format!("libasound: {e}"))?;
            let open: Open = *lib.get::<Open>(b"snd_pcm_open\0").map_err(|e| e.to_string())?;
            let set_params: SetParams = *lib.get::<SetParams>(b"snd_pcm_set_params\0").map_err(|e| e.to_string())?;
            let writei: WriteI = *lib.get::<WriteI>(b"snd_pcm_writei\0").map_err(|e| e.to_string())?;
            let recover: Recover = *lib.get::<Recover>(b"snd_pcm_recover\0").map_err(|e| e.to_string())?;
            let close: Close = *lib.get::<Close>(b"snd_pcm_close\0").map_err(|e| e.to_string())?;
            let mut pcm: *mut c_void = std::ptr::null_mut();
            let rc = open(&mut pcm, c"default".as_ptr(), SND_PCM_STREAM_PLAYBACK, 0);
            if rc < 0 || pcm.is_null() {
                return Err(format!("no audio device (alsa error {rc})"));
            }
            let rc = set_params(pcm, SND_PCM_FORMAT_FLOAT_LE, SND_PCM_ACCESS_RW_INTERLEAVED, 2, rate, 1, 80_000);
            if rc < 0 {
                close(pcm);
                return Err(format!("audio device rejected the format (alsa error {rc})"));
            }
            let pcm = Pcm(pcm);
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let thread = std::thread::Builder::new()
                .name("audio-out".into())
                .spawn(move || {
                    let pcm = pcm;
                    let mut buf = vec![0.0f32; PERIOD_FRAMES * 2];
                    while !flag.load(Ordering::Relaxed) {
                        match mixer.lock() {
                            Ok(mut m) => m.render(&mut buf),
                            Err(_) => break,
                        }
                        let n = writei(pcm.0, buf.as_ptr() as *const c_void, PERIOD_FRAMES as c_ulong);
                        if n < 0 && recover(pcm.0, n as c_int, 1) < 0 {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                    }
                    close(pcm.0);
                    drop(lib);
                })
                .map_err(|e| e.to_string())?;
            Ok(Device { stop, thread: Some(thread) })
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}
