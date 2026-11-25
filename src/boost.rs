//! Brightness boost and synced software dimming (see docs/how-it-works.md):
//! 1. Above 100% (HDR boost): put a tiny window on each HDR screen that shows a colour brighter
//!    than white (1.6), which makes macOS switch the screen into its HDR (EDR) mode; as soon as
//!    the screen's HDR headroom is above 1.05, multiply the screen's normal colour table by the
//!    boost factor, following the headroom as macOS ramps it up.
//! 2. Below 100% (synced dimming): keep all connected displays dimming together.
//!    Displays with native hardware brightness follow via DisplayServices; external displays that
//!    do not expose hardware brightness are dimmed smoothly in software by scaling their colour
//!    table below 1.0 (with a borderless dark overlay fallback if the colour table is unavailable).
//! Turning off or quitting restores all colour tables and closes all helper windows.

use std::time::{Duration, Instant};

pub const EDR_MIN_HEADROOM: f64 = 1.05;
pub const GATE_TIMEOUT: Duration = Duration::from_secs(10); // tunable: give up waking HDR after this long
pub const GAMMA_DRIFT: f32 = 0.03;
pub const TRIGGER_VALUE: f64 = 1.6;
pub const TRIGGER_SIZE: f64 = 5.0;
pub const SMOOTH: f32 = 0.62; // share per ~14 Hz background step (~0.3 s fade)
pub const DIM_FLOOR: f32 = 0.05; // minimum software gamma scale at 0% (dark, yet never strands the screen)

/// Move the applied value a share of the way to the target, snapping when close.
pub fn ease(applied: f32, target: f32) -> f32 {
    if (target - applied).abs() < 0.003 { target } else { applied + (target - applied) * SMOOTH }
}

/// Boost multiplier: 1.0 at level 0, the screen's full HDR headroom at level 1 (linear; tunable).
pub fn factor(headroom: f64, level: f32) -> f32 {
    (1.0 + (headroom - 1.0).max(0.0) * level.clamp(0.0, 1.0) as f64) as f32
}

/// Software dimming multiplier: 1.0 at full brightness, linearly down to DIM_FLOOR at 0%.
pub fn dim_factor(level: f32) -> f32 {
    let l = level.clamp(0.0, 1.0);
    if l >= 0.999 { 1.0 } else { DIM_FLOOR + (1.0 - DIM_FLOOR) * l }
}

/// Reading the live NSScreen list is relatively expensive. It is only needed while HDR is active
/// or while a software shade must follow a screen's frame.
fn needs_screen_snapshot(boosting: bool, has_shade: bool, needs_soft_dim: bool) -> bool {
    boosting || has_shade || needs_soft_dim
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowTone {
    Boosted,
    Dimmed,
    Ready,
    Muted,
}

pub struct Row {
    pub name: String,
    pub status: String,
    pub tone: RowTone,
}

#[cfg(target_os = "macos")]
pub use mac::Booster;

#[cfg(not(target_os = "macos"))]
pub struct Booster;
#[cfg(not(target_os = "macos"))]
impl Booster {
    pub fn new() -> Self {
        Booster
    }
    pub fn set_on(&mut self, _on: bool) {}
    pub fn restore_all(&mut self) {}
    pub fn tick(&mut self, _boost_level: f32, _dim_level: f32, _sync: bool) {}
    pub fn headroom(&self) -> f64 {
        1.0
    }
    pub fn system_brightness(&self) -> Option<f32> {
        None
    }
    pub fn set_system_brightness(&self, _v: f32) {}
    pub fn has_soft_dimmable(&self) -> bool {
        false
    }
    pub fn has_hdr(&self) -> bool {
        false
    }
    pub fn synced_display_count(&self) -> usize {
        0
    }
    pub fn take_key_presses(&self) -> (i32, i32) {
        (0, 0)
    }
    pub fn set_intercept(&self, _up: bool, _down: bool) {}
    pub fn watch_keys(&mut self) {}
    pub fn keys_taken(&self) -> bool {
        false
    }
    pub fn rows(&self, _sync: bool, _dim_level: f32) -> Vec<Row> {
        vec![Row {
            name: "This computer".into(),
            status: "Needs a Mac with an HDR screen".into(),
            tone: RowTone::Muted,
        }]
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSBackingStoreType, NSColor, NSEvent, NSEventMask, NSEventType, NSScreen, NSWindow,
        NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop, CGSize};
    use objc2_core_graphics::{
        kCGColorSpaceExtendedLinearDisplayP3, CGColorSpace, CGDisplayGammaTableCapacity,
        CGDisplayIsBuiltin, CGDisplayRestoreColorSyncSettings, CGError, CGEvent, CGEventTapLocation,
        CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
        CGGetDisplayTransferByTable, CGPreflightPostEventAccess, CGRequestPostEventAccess,
        CGSetDisplayTransferByTable,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use objc2_metal::{
        MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
        MTLCreateSystemDefaultDevice, MTLDevice, MTLLoadAction, MTLPixelFormat,
        MTLRenderPassDescriptor, MTLStoreAction,
    };
    use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
    use std::cell::RefCell;
    use std::ffi::{c_char, c_void};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::{Arc, OnceLock};

    const SCREEN_SYNC_INTERVAL: Duration = Duration::from_secs(1);
    const GAMMA_CHECK_INTERVAL: Duration = Duration::from_secs(1);

    type Gamma = [Vec<f32>; 3];

    enum BoostPhase {
        Off,
        Waiting { since: Instant },
        Ready { headroom: f64 },
        Failed(String),
    }

    struct Display {
        id: u32,
        name: String,
        potential: f64,
        has_hw_brightness: bool,
        baseline: Option<Gamma>,
        applied_gamma: f32,
        applied_hw: Option<f32>,
        last_gamma_check: Instant,
        boost: BoostPhase,
        window: Option<Retained<NSWindow>>,
        layer: Option<Retained<CAMetalLayer>>,
        shade: Option<Retained<NSWindow>>,
    }

    pub struct Booster {
        mtm: MainThreadMarker,
        displays: Vec<Display>,
        on: bool,
        gamma_dirty: bool,
        last_screen_sync: Instant,
        brightness_id: Option<u32>,
        keys: Arc<Keys>,
        tap: Option<CFRetained<CFMachPort>>,
        _monitors: Vec<Retained<AnyObject>>,
        device: Option<Retained<ProtocolObject<dyn MTLDevice>>>,
        queue: Option<Retained<ProtocolObject<dyn MTLCommandQueue>>>,
    }

    fn read_gamma(id: u32) -> Option<Gamma> {
        let cap = CGDisplayGammaTableCapacity(id);
        if cap < 2 {
            return None;
        }
        let (mut r, mut g, mut b) = (vec![0f32; cap as usize], vec![0f32; cap as usize], vec![0f32; cap as usize]);
        let mut n = 0u32;
        let err = unsafe {
            CGGetDisplayTransferByTable(id, cap, r.as_mut_ptr(), g.as_mut_ptr(), b.as_mut_ptr(), &mut n)
        };
        if err != CGError::Success || n < 2 {
            return None;
        }
        for t in [&mut r, &mut g, &mut b] {
            t.truncate(n as usize);
        }
        Some([r, g, b])
    }

    fn write_gamma(id: u32, base: &Gamma, factor: f32) {
        let s: Vec<Vec<f32>> = base.iter().map(|t| t.iter().map(|v| v * factor).collect()).collect();
        unsafe {
            CGSetDisplayTransferByTable(id, s[0].len() as u32, s[0].as_ptr(), s[1].as_ptr(), s[2].as_ptr());
        }
    }

    /// True when the live table still matches the baseline at the quarter sample (within 0.03).
    fn gamma_unchanged(id: u32, base: &Gamma) -> bool {
        let Some(now) = read_gamma(id) else { return false };
        let i = base[0].len() / 4;
        (0..3).all(|c| now[c].get(i).zip(base[c].get(i)).is_some_and(|(a, b)| (a - b).abs() < GAMMA_DRIFT))
    }

    /// True when the live table still matches `base * expected` (detects macOS resetting gamma after sleep/wake).
    fn gamma_matches(id: u32, base: &Gamma, expected: f32) -> bool {
        let Some(now) = read_gamma(id) else { return false };
        let i = (base[0].len() * 3) / 4;
        (0..3).all(|c| {
            now[c]
                .get(i)
                .zip(base[c].get(i))
                .is_some_and(|(a, b)| (a - b * expected).abs() < GAMMA_DRIFT)
        })
    }

    fn screens(mtm: MainThreadMarker) -> Vec<Retained<NSScreen>> {
        NSScreen::screens(mtm).iter().collect()
    }

    impl Booster {
        pub fn new() -> Self {
            let mtm = MainThreadMarker::new().expect("main thread");
            let keys = Arc::new(Keys::default());
            let _monitors = watch_brightness_keys(&keys);
            let mut b = Booster {
                mtm,
                displays: vec![],
                on: false,
                gamma_dirty: false,
                last_screen_sync: Instant::now(),
                brightness_id: None,
                keys,
                tap: None,
                _monitors,
                device: None,
                queue: None,
            };
            b.sync_screens();
            b
        }

        /// The system brightness (0..1) of the primary screen macOS lets us read it from.
        pub fn system_brightness(&self) -> Option<f32> {
            let (get, _) = display_services()?;
            let mut v = 0f32;
            (unsafe { get(self.brightness_id?, &mut v) } == 0).then_some(v)
        }

        pub fn set_system_brightness(&self, v: f32) {
            if let (Some((_, set)), Some(id)) = (display_services(), self.brightness_id) {
                unsafe { set(id, v.clamp(0.0, 1.0)) };
            }
        }

        /// True if at least one connected screen lacks hardware brightness control and can be dimmed in software.
        pub fn has_soft_dimmable(&self) -> bool {
            self.displays.iter().any(|d| !d.has_hw_brightness)
        }

        /// True if at least one connected screen supports HDR boosting.
        pub fn has_hdr(&self) -> bool {
            self.displays.iter().any(|d| d.potential > EDR_MIN_HEADROOM)
        }

        /// Number of displays that follow synced dimming (software-dimmed or secondary hardware displays).
        pub fn synced_display_count(&self) -> usize {
            self.displays
                .iter()
                .filter(|d| !d.has_hw_brightness || Some(d.id) != self.brightness_id)
                .count()
        }

        /// Brightness (up, down) key presses seen since the last call.
        pub fn take_key_presses(&self) -> (i32, i32) {
            let c = &self.keys.count;
            (c[0].swap(0, Ordering::Relaxed), c[1].swap(0, Ordering::Relaxed))
        }

        /// Take over the brightness keys (once permission is given).
        pub fn watch_keys(&mut self) {
            if self.tap.is_none() {
                self.tap = intercept_brightness_keys(&self.keys);
            }
        }

        /// Whether Glow can take brightness keys before macOS sees them (permission given).
        pub fn keys_taken(&self) -> bool {
            self.tap.is_some()
        }

        /// Which brightness keys (up, down) Glow takes over before macOS sees them.
        pub fn set_intercept(&self, up: bool, down: bool) {
            self.keys.intercept[0].store(up, Ordering::Relaxed);
            self.keys.intercept[1].store(down, Ordering::Relaxed);
        }

        /// Keep one entry per connected screen.
        fn sync_screens(&mut self) {
            self.last_screen_sync = Instant::now();
            let list = screens(self.mtm);
            let mut i = 0;
            while i < self.displays.len() {
                let id = self.displays[i].id;
                if list.iter().any(|s| s.CGDirectDisplayID() == id) {
                    i += 1;
                } else {
                    let mut removed = self.displays.remove(i);
                    if let Some(w) = removed.window.take() {
                        w.close();
                    }
                    if let Some(s) = removed.shade.take() {
                        s.close();
                    }
                }
            }
            let ds = display_services();
            let now = Instant::now();
            for s in &list {
                let id = s.CGDirectDisplayID();
                if !self.displays.iter().any(|d| d.id == id) {
                    let has_hw_brightness = ds.is_some_and(|(get, _)| unsafe { get(id, &mut 0f32) } == 0);
                    let mut d = Display {
                        id,
                        name: s.localizedName().to_string(),
                        potential: s.maximumPotentialExtendedDynamicRangeColorComponentValue(),
                        has_hw_brightness,
                        baseline: None,
                        applied_gamma: 1.0,
                        applied_hw: None,
                        last_gamma_check: now,
                        boost: BoostPhase::Off,
                        window: None,
                        layer: None,
                        shade: None,
                    };
                    if self.on {
                        self.init_boost_display(&mut d, s);
                    }
                    self.displays.push(d);
                }
            }
            // Prefer the built-in display as the primary brightness reference when multiple hardware displays exist.
            self.brightness_id = self
                .displays
                .iter()
                .find(|d| d.has_hw_brightness && CGDisplayIsBuiltin(d.id))
                .or_else(|| self.displays.iter().find(|d| d.has_hw_brightness))
                .map(|d| d.id);
        }

        fn init_boost_display(&self, d: &mut Display, screen: &NSScreen) {
            if d.potential <= EDR_MIN_HEADROOM {
                d.boost = BoostPhase::Failed("No HDR brightness on this screen".into());
                return;
            }
            if d.baseline.is_none() {
                d.baseline = read_gamma(d.id);
            }
            if d.baseline.is_none() {
                d.boost = BoostPhase::Failed("Colour table not ready, left unchanged".into());
                return;
            }
            if let Some(dev) = &self.device {
                let (w, l) = make_trigger(self.mtm, screen, dev);
                d.window = Some(w);
                d.layer = Some(l);
            }
            d.boost = BoostPhase::Waiting { since: Instant::now() };
        }

        /// Restore global ColorSync settings only once every display is back to neutral (1.0) and unboosted.
        fn maybe_restore_colorsync(&mut self) {
            if !self.gamma_dirty || self.on {
                return;
            }
            let any_active = self.displays.iter().any(|d| {
                (d.applied_gamma - 1.0).abs() >= 0.001 || d.baseline.is_some() || d.window.is_some()
            });
            if !any_active {
                CGDisplayRestoreColorSyncSettings();
                self.gamma_dirty = false;
            }
        }

        pub fn set_on(&mut self, on: bool) {
            if on == self.on {
                return;
            }
            self.on = on;
            if on {
                self.watch_keys();
                self.sync_screens();
                if self.device.is_none() {
                    self.device = MTLCreateSystemDefaultDevice();
                    self.queue = self.device.as_ref().and_then(|d| d.newCommandQueue());
                }
                let list = screens(self.mtm);
                let mtm = self.mtm;
                let device = self.device.clone();
                for d in &mut self.displays {
                    if d.potential <= EDR_MIN_HEADROOM {
                        d.boost = BoostPhase::Failed("No HDR brightness on this screen".into());
                        continue;
                    }
                    if d.baseline.is_none() {
                        d.baseline = read_gamma(d.id);
                    }
                    if d.baseline.is_none() {
                        d.boost = BoostPhase::Failed("Colour table not ready, left unchanged".into());
                        continue;
                    }
                    if let (Some(s), Some(dev)) = (list.iter().find(|s| s.CGDirectDisplayID() == d.id), &device) {
                        let (w, l) = make_trigger(mtm, s, dev);
                        d.window = Some(w);
                        d.layer = Some(l);
                    }
                    d.boost = BoostPhase::Waiting { since: Instant::now() };
                }
                self.render_triggers();
            } else {
                for d in &mut self.displays {
                    if let Some(w) = d.window.take() {
                        w.close();
                    }
                    d.layer = None;
                    d.boost = BoostPhase::Off;
                    // Restore only displays that were boosted (> 1.0); leave software-dimmed displays untouched.
                    if d.applied_gamma > 1.001 {
                        if let Some(base) = &d.baseline {
                            write_gamma(d.id, base, 1.0);
                        }
                        d.applied_gamma = 1.0;
                        d.baseline = None;
                    } else if d.applied_gamma >= 0.999 {
                        d.baseline = None;
                    }
                }
                self.maybe_restore_colorsync();
            }
        }

        /// Close every helper window and restore all display colour tables (used on quit/drop).
        pub fn restore_all(&mut self) {
            self.on = false;
            for d in &mut self.displays {
                if let Some(w) = d.window.take() {
                    w.close();
                }
                if let Some(s) = d.shade.take() {
                    s.close();
                }
                d.layer = None;
                d.baseline = None;
                d.applied_gamma = 1.0;
                d.applied_hw = None;
                d.boost = BoostPhase::Off;
            }
            if self.gamma_dirty {
                CGDisplayRestoreColorSyncSettings();
                self.gamma_dirty = false;
            }
        }

        fn render_triggers(&self) {
            let Some(q) = &self.queue else { return };
            for l in self.displays.iter().filter_map(|d| d.layer.as_ref()) {
                render_trigger(l, q);
            }
        }

        /// Called every frame: updates HDR boost above 100% and synced hardware/software dimming below 100%.
        pub fn tick(&mut self, boost_level: f32, dim_level: f32, sync: bool) {
            if self.last_screen_sync.elapsed() >= SCREEN_SYNC_INTERVAL {
                self.sync_screens();
            }
            // The Metal layer retains its last drawable. Re-rendering the same five-pixel trigger
            // every poll only submits duplicate GPU command buffers; set_on renders it once.
            let ds = display_services();
            let primary_id = self.brightness_id;
            let dim_target = if sync { dim_factor(dim_level) } else { 1.0 };
            let hw_target = dim_level.clamp(0.0, 1.0);
            let mtm = self.mtm;
            let on = self.on;
            let has_shade = self.displays.iter().any(|d| d.shade.is_some());
            let needs_soft_dim = dim_target < 0.999 && self.displays.iter().any(|d| !d.has_hw_brightness);
            let list = needs_screen_snapshot(on, has_shade, needs_soft_dim).then(|| screens(self.mtm));
            let mut gamma_written = false;

            for d in &mut self.displays {
                let screen = list
                    .as_ref()
                    .and_then(|screens| screens.iter().find(|s| s.CGDirectDisplayID() == d.id));

                // 1. Update HDR boost gate while boost is active.
                if let (true, Some(s)) = (on, screen) {
                    let h = s.maximumExtendedDynamicRangeColorComponentValue();
                    match &mut d.boost {
                        BoostPhase::Waiting { since } => {
                            if h > EDR_MIN_HEADROOM {
                                let owned_by_dim = d.applied_gamma < 0.999;
                                let ok = d
                                    .baseline
                                    .as_ref()
                                    .is_some_and(|b| owned_by_dim || gamma_unchanged(d.id, b));
                                d.boost = if ok {
                                    BoostPhase::Ready { headroom: h }
                                } else {
                                    BoostPhase::Failed(
                                        "Colour table was already changed by another app, left unchanged".into(),
                                    )
                                };
                            } else if since.elapsed() > GATE_TIMEOUT {
                                d.boost = BoostPhase::Failed(
                                    "HDR brightness did not become available, left unchanged".into(),
                                );
                            }
                        }
                        BoostPhase::Ready { headroom } => {
                            *headroom = h.max(1.0);
                        }
                        _ => {}
                    }
                }

                // 2. Sync secondary displays that DO support hardware brightness (e.g. Apple Studio Display).
                if d.has_hw_brightness && Some(d.id) != primary_id {
                    if sync {
                        if let Some((get, set)) = ds {
                            let cur = d.applied_hw.unwrap_or_else(|| {
                                let mut v = hw_target;
                                unsafe {
                                    get(d.id, &mut v);
                                }
                                v
                            });
                            let next = ease(cur, hw_target);
                            if (next - cur).abs() > 0.0005 || d.applied_hw.is_none() {
                                unsafe {
                                    set(d.id, next.clamp(0.0, 1.0));
                                }
                            }
                            d.applied_hw = Some(next);
                        }
                    } else {
                        d.applied_hw = None;
                    }
                }

                // 3. Unified gamma / software-dim target for this display.
                let boost_target = match d.boost {
                    BoostPhase::Ready { headroom } if on => factor(headroom, boost_level),
                    _ => 1.0,
                };
                let target_gamma = if boost_target > 1.001 {
                    boost_target
                } else if !d.has_hw_brightness {
                    dim_target
                } else {
                    1.0
                };

                let needs_gamma_work = (target_gamma - 1.0).abs() >= 0.001 || (d.applied_gamma - 1.0).abs() >= 0.001;
                if needs_gamma_work {
                    // Capture baseline lazily on first software dim; fall back to a dark overlay if gamma fails.
                    if d.baseline.is_none() && d.shade.is_none() {
                        d.baseline = read_gamma(d.id);
                        if d.baseline.is_none() && target_gamma < 0.999 {
                            let Some(s) = screen else { continue };
                            d.shade = Some(make_shade(mtm, s));
                        }
                    }

                    let next = ease(d.applied_gamma, target_gamma);
                    let moving = (next - d.applied_gamma).abs() > 0.0;
                    let periodic = target_gamma < 0.999 && d.last_gamma_check.elapsed() >= GAMMA_CHECK_INTERVAL;

                    if let Some(base) = &d.baseline {
                        if moving {
                            d.applied_gamma = next;
                            write_gamma(d.id, base, d.applied_gamma);
                            gamma_written = true;
                            d.last_gamma_check = Instant::now();
                        } else if periodic {
                            if !gamma_matches(d.id, base, d.applied_gamma) {
                                write_gamma(d.id, base, d.applied_gamma);
                                gamma_written = true;
                            }
                            d.last_gamma_check = Instant::now();
                        }
                    } else if let Some(shade) = &d.shade {
                        if moving || periodic {
                            d.applied_gamma = next;
                            if let Some(s) = screen {
                                shade.setFrame_display(s.frame(), false);
                            }
                            shade.setAlphaValue((1.0 - d.applied_gamma).clamp(0.0, 1.0 - DIM_FLOOR) as f64);
                            d.last_gamma_check = Instant::now();
                        }
                    }

                    // Once faded cleanly back to 1.0, release our colour table / overlay.
                    if (d.applied_gamma - 1.0).abs() < 0.001 && (target_gamma - 1.0).abs() < 0.001 {
                        d.applied_gamma = 1.0;
                        if let Some(shade) = d.shade.take() {
                            shade.close();
                        }
                        if !matches!(d.boost, BoostPhase::Waiting { .. } | BoostPhase::Ready { .. }) {
                            if let Some(base) = &d.baseline {
                                write_gamma(d.id, base, 1.0);
                            }
                            d.baseline = None;
                        }
                    }
                }
            }

            if gamma_written {
                self.gamma_dirty = true;
            }
            self.maybe_restore_colorsync();
        }

        /// Best HDR headroom among the screens (what 100% boost reaches).
        pub fn headroom(&self) -> f64 {
            self.displays
                .iter()
                .map(|d| match d.boost {
                    BoostPhase::Ready { headroom } => headroom,
                    _ => d.potential.min(TRIGGER_VALUE),
                })
                .fold(1.0, f64::max)
        }

        pub fn rows(&self, sync: bool, dim_level: f32) -> Vec<Row> {
            let pct = (dim_level.clamp(0.0, 1.0) * 100.0).round();
            let primary_id = self.brightness_id;
            self.displays
                .iter()
                .map(|d| {
                    let (status, tone) = if d.applied_gamma > 1.005 {
                        (format!("Boosted ×{:.2}", d.applied_gamma), RowTone::Boosted)
                    } else if d.applied_gamma < 0.995 {
                        (format!("Dimmed · {pct:.0}%"), RowTone::Dimmed)
                    } else if sync && d.has_hw_brightness && Some(d.id) != primary_id && dim_level < 0.995 {
                        (format!("Synced · {pct:.0}%"), RowTone::Dimmed)
                    } else {
                        match &d.boost {
                            BoostPhase::Waiting { .. } => ("Waking up HDR…".to_string(), RowTone::Ready),
                            BoostPhase::Ready { .. } => ("Boosted ×1.00".to_string(), RowTone::Boosted),
                            BoostPhase::Failed(_) if sync && !d.has_hw_brightness => {
                                ("At 100% (no HDR)".to_string(), RowTone::Muted)
                            }
                            BoostPhase::Failed(m) => (m.clone(), RowTone::Muted),
                            BoostPhase::Off => {
                                if d.potential > EDR_MIN_HEADROOM {
                                    if sync && !d.has_hw_brightness {
                                        ("HDR + software dim".to_string(), RowTone::Ready)
                                    } else {
                                        ("Ready to boost".to_string(), RowTone::Ready)
                                    }
                                } else if sync && !d.has_hw_brightness {
                                    ("Software dim ready".to_string(), RowTone::Ready)
                                } else {
                                    ("No HDR brightness".to_string(), RowTone::Muted)
                                }
                            }
                        }
                    };
                    Row { name: d.name.clone(), status, tone }
                })
                .collect()
        }
    }

    impl Drop for Booster {
        fn drop(&mut self) {
            self.restore_all();
        }
    }

    type GetBrightness = unsafe extern "C" fn(u32, *mut f32) -> i32;
    type SetBrightness = unsafe extern "C" fn(u32, f32) -> i32;

    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    /// macOS's own brightness calls (DisplayServices). Loaded at run time because the framework is
    /// private and has no file to link against.
    fn display_services() -> Option<(GetBrightness, SetBrightness)> {
        static DS: OnceLock<Option<(GetBrightness, SetBrightness)>> = OnceLock::new();
        *DS.get_or_init(|| unsafe {
            let h = dlopen(c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices".as_ptr(), 1);
            if h.is_null() {
                return None;
            }
            let g = dlsym(h, c"DisplayServicesGetBrightness".as_ptr());
            let s = dlsym(h, c"DisplayServicesSetBrightness".as_ptr());
            (!g.is_null() && !s.is_null()).then(|| {
                (
                    std::mem::transmute::<*mut c_void, GetBrightness>(g),
                    std::mem::transmute::<*mut c_void, SetBrightness>(s),
                )
            })
        })
    }

    /// Count brightness up/down key presses, both while another app or this one is in front.
    fn watch_brightness_keys(keys: &Arc<Keys>) -> Vec<Retained<AnyObject>> {
        let k = keys.clone();
        let seen = move |e: NonNull<NSEvent>| {
            if let Some((i, true)) = brightness_key(unsafe { e.as_ref() }) {
                k.count[i].fetch_add(1, Ordering::Relaxed);
            }
        };
        let local = seen.clone();
        let mut out = vec![];
        out.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::SystemDefined,
            &block2::RcBlock::new(seen),
        ));
        out.extend(unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::SystemDefined,
                &block2::RcBlock::new(move |e: NonNull<NSEvent>| {
                    local(e);
                    e.as_ptr()
                }),
            )
        });
        out
    }

    #[derive(Default)]
    pub struct Keys {
        count: [AtomicI32; 2],      // presses seen: [up, down]
        intercept: [AtomicBool; 2], // keys to take over: [up, down]
        tap: RefCell<Option<CFRetained<CFMachPort>>>,
    }

    /// A brightness key event: (0 = up / 1 = down, pressed down?).
    fn brightness_key(e: &NSEvent) -> Option<(usize, bool)> {
        // Media keys: subtype 8, key code in the high 16 bits of data1, 0xA = pressed, 0xB = released.
        if e.r#type() != NSEventType::SystemDefined || e.subtype().0 != 8 {
            return None;
        }
        let d = e.data1();
        let key = (d >> 16) & 0xFFFF;
        (key == 2 || key == 3).then(|| ((key - 2) as usize, (d >> 8) & 0xFF == 0xA))
    }

    unsafe extern "C-unwind" fn on_key(
        _: CGEventTapProxy,
        ty: CGEventType,
        ev: NonNull<CGEvent>,
        info: *mut c_void,
    ) -> *mut CGEvent {
        let keys = unsafe { &*(info as *const Keys) };
        if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
            if let Some(t) = keys.tap.borrow().as_deref() {
                CGEvent::tap_enable(t, true);
            }
        } else if let Some((i, pressed)) =
            NSEvent::eventWithCGEvent(unsafe { ev.as_ref() }).as_deref().and_then(brightness_key)
        {
            if keys.intercept[i].load(Ordering::Relaxed) {
                if pressed {
                    keys.count[i].fetch_add(1, Ordering::Relaxed);
                }
                return std::ptr::null_mut(); // macOS never sees it, so the screen doesn't dim first
            }
        }
        ev.as_ptr()
    }

    /// Take over the brightness keys while boosting or syncing. Needs Accessibility permission (macOS asks once);
    /// without it the key watchers above still work.
    fn intercept_brightness_keys(keys: &Arc<Keys>) -> Option<CFRetained<CFMachPort>> {
        if !CGPreflightPostEventAccess() && !CGRequestPostEventAccess() {
            return None;
        }
        let tap = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                1 << 14, // system-defined events (media keys)
                Some(on_key),
                Arc::as_ptr(keys) as *mut c_void,
            )
        }?;
        let src = CFMachPort::new_run_loop_source(None, Some(&tap), 0)?;
        CFRunLoop::main()?.add_source(Some(&src), unsafe { kCFRunLoopCommonModes });
        *keys.tap.borrow_mut() = Some(tap.clone());
        Some(tap)
    }

    /// Fallback click-through black overlay for displays whose gamma table cannot be read.
    fn make_shade(mtm: MainThreadMarker, screen: &NSScreen) -> Retained<NSWindow> {
        let rect = screen.frame();
        unsafe {
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            w.setReleasedWhenClosed(false);
            w.setOpaque(false);
            w.setHasShadow(false);
            w.setBackgroundColor(Some(&NSColor::blackColor()));
            w.setIgnoresMouseEvents(true);
            w.setAlphaValue(0.0);
            w.setLevel(100); // above normal windows and menu bar, just below Glow's pop-up bubble
            w.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            w.orderFrontRegardless();
            w
        }
    }

    fn make_trigger(
        mtm: MainThreadMarker,
        screen: &NSScreen,
        device: &ProtocolObject<dyn MTLDevice>,
    ) -> (Retained<NSWindow>, Retained<CAMetalLayer>) {
        let f = screen.frame();
        let rect = NSRect::new(NSPoint::new(f.origin.x, f.origin.y), NSSize::new(TRIGGER_SIZE, TRIGGER_SIZE));
        unsafe {
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            w.setReleasedWhenClosed(false);
            w.setOpaque(false);
            w.setHasShadow(false);
            w.setBackgroundColor(Some(&NSColor::clearColor()));
            w.setIgnoresMouseEvents(true);
            w.setLevel(25); // status-bar level, stays above normal windows
            w.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            let layer = CAMetalLayer::new();
            layer.setDevice(Some(device));
            layer.setPixelFormat(MTLPixelFormat::RGBA16Float);
            layer.setWantsExtendedDynamicRangeContent(true);
            let cs = CGColorSpace::with_name(Some(kCGColorSpaceExtendedLinearDisplayP3));
            layer.setColorspace(cs.as_deref());
            let scale = screen.backingScaleFactor();
            layer.setDrawableSize(CGSize::new(TRIGGER_SIZE * scale, TRIGGER_SIZE * scale));
            if let Some(v) = w.contentView() {
                v.setLayer(Some(&layer));
                v.setWantsLayer(true);
            }
            w.orderFrontRegardless();
            (w, layer)
        }
    }

    /// Fill the trigger with a value brighter than white so macOS turns on HDR for the screen.
    fn render_trigger(layer: &CAMetalLayer, queue: &ProtocolObject<dyn MTLCommandQueue>) {
        unsafe {
            let Some(drawable) = layer.nextDrawable() else { return };
            let pass = MTLRenderPassDescriptor::new();
            let att = pass.colorAttachments().objectAtIndexedSubscript(0);
            att.setTexture(Some(&drawable.texture()));
            att.setLoadAction(MTLLoadAction::Clear);
            att.setStoreAction(MTLStoreAction::Store);
            att.setClearColor(MTLClearColor {
                red: TRIGGER_VALUE,
                green: TRIGGER_VALUE,
                blue: TRIGGER_VALUE,
                alpha: 1.0,
            });
            let Some(cb) = queue.commandBuffer() else { return };
            if let Some(enc) = cb.renderCommandEncoderWithDescriptor(&pass) {
                enc.endEncoding();
            }
            cb.presentDrawable(ProtocolObject::from_ref(&*drawable));
            cb.commit();
        }
    }
}

#[cfg(test)]
mod cpu_tests {
    use super::needs_screen_snapshot;

    #[test]
    fn idle_tick_does_not_enumerate_screens() {
        assert!(!needs_screen_snapshot(false, false, false));
        assert!(needs_screen_snapshot(true, false, false));
        assert!(needs_screen_snapshot(false, true, false));
        assert!(needs_screen_snapshot(false, false, true));
    }
}

#[cfg(test)]
mod tests {
    use super::{dim_factor, ease, factor, DIM_FLOOR};
    #[test]
    fn ease_moves_smoothly_then_lands() {
        let mut a = 1.0;
        let first = ease(a, 1.5);
        assert!(first > 1.0 && first < 1.5);
        for _ in 0..10 {
            a = ease(a, 1.5);
        }
        assert_eq!(a, 1.5);
    }
    #[test]
    fn factor_spans_one_to_headroom() {
        assert_eq!(factor(1.6, 0.0), 1.0);
        assert!((factor(1.6, 1.0) - 1.6).abs() < 1e-6);
        assert!((factor(1.6, 0.5) - 1.3).abs() < 1e-6);
        assert_eq!(factor(0.9, 1.0), 1.0);
    }
    #[test]
    fn dim_factor_spans_floor_to_one() {
        assert_eq!(dim_factor(1.0), 1.0);
        assert_eq!(dim_factor(1.5), 1.0);
        assert!((dim_factor(0.0) - DIM_FLOOR).abs() < 1e-6);
        assert!((dim_factor(-0.5) - DIM_FLOOR).abs() < 1e-6);
        let mid = dim_factor(0.5);
        assert!(mid > 0.45 && mid < 0.60);
        assert!(dim_factor(0.25) < mid && mid < dim_factor(0.75));
    }
}
