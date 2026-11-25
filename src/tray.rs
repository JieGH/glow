//! Menu-bar icon. Left click shows or hides the controls as a bubble under the icon; right click
//! (or Control-click) opens a small menu with the boost switch and Quit. When the brightness changes,
//! the same bubble pops up on its own for a moment (without taking focus), then fades out.

use crate::Core;
use block2::RcBlock;
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSImage, NSMenu, NSMenuItem,
    NSPopUpMenuWindowLevel, NSScreen, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWindow,
    NSWindowCollectionBehavior,
};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer, NSUserDefaults};
use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

const POPUP_VISIBLE: Duration = Duration::from_millis(1500);
const POPUP_FADE: Duration = Duration::from_millis(300);
// Brightness/display state does not need a frame-rate polling loop. About 14.4 checks per second
// is responsive enough for key presses while still letting an idle app sleep.
const BACKGROUND_INTERVAL: f64 = 1.0 / 14.375;
const HIDDEN_KEY: &str = "hideIndicator";
const BOOST_ON_KEY: &str = "enableAutoBoost";
const SYNC_OFF_KEY: &str = "disableSyncDimming";

/// Whether the person turned the pop-up off (remembered between launches; on by default).
pub fn hidden_saved() -> bool {
    NSUserDefaults::standardUserDefaults().boolForKey(&NSString::from_str(HIDDEN_KEY))
}

pub fn save_hidden(hidden: bool) {
    NSUserDefaults::standardUserDefaults().setBool_forKey(hidden, &NSString::from_str(HIDDEN_KEY));
}

/// Whether the person turned auto boost on (remembered between launches; off by default).
pub fn boost_on_saved() -> bool {
    NSUserDefaults::standardUserDefaults().boolForKey(&NSString::from_str(BOOST_ON_KEY))
}

pub fn save_boost_on(on: bool) {
    NSUserDefaults::standardUserDefaults().setBool_forKey(on, &NSString::from_str(BOOST_ON_KEY));
}

/// Whether the person turned synced display dimming off (remembered between launches; on by default).
pub fn sync_disabled_saved() -> bool {
    NSUserDefaults::standardUserDefaults().boolForKey(&NSString::from_str(SYNC_OFF_KEY))
}

pub fn save_sync_disabled(disabled: bool) {
    NSUserDefaults::standardUserDefaults().setBool_forKey(disabled, &NSString::from_str(SYNC_OFF_KEY));
}

/// Decides when a brightness change pops the bubble up.
#[derive(Default)]
pub struct Popup {
    last: Option<f32>,
    shown_at: Option<Instant>, // set while the bubble is up because of a brightness change
}

impl Popup {
    /// `force`: Glow handled a brightness key. Otherwise only boost-range changes pop it up, because
    /// below 100% macOS shows its own indicator; once up, it follows every change.
    pub fn wants_show(&mut self, total: f32, force: bool) -> bool {
        let moved = self.last.is_some_and(|l| (total - l).abs() > 0.001);
        let boost_range = self.last.is_some_and(|l| total > 1.0 || l > 1.0);
        self.last = Some(total);
        force || (moved && (boost_range || self.shown_at.is_some()))
    }
}

pub struct Ivars {
    window: Retained<NSWindow>,
    item: Retained<NSStatusItem>,
    core: Rc<RefCell<Core>>,
    ctx: egui::Context,
    hidden_at: Rc<Cell<Option<Instant>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and Target doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlowTrayTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(iconClicked:))]
        fn icon_clicked(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let right = NSApplication::sharedApplication(mtm).currentEvent().is_some_and(|e| {
                matches!(e.r#type(), NSEventType::RightMouseUp | NSEventType::RightMouseDown)
                    || e.modifierFlags().contains(NSEventModifierFlags::Control)
            });
            if right {
                self.show_menu();
            } else {
                self.toggle_bubble();
            }
        }

        #[unsafe(method(showBubble:))]
        fn show_bubble_action(&self, _sender: Option<&AnyObject>) {
            show(&self.ivars().window, &self.ivars().item, self.mtm());
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(toggleBoost:))]
        fn toggle_boost(&self, _sender: Option<&AnyObject>) {
            if let Ok(mut c) = self.ivars().core.try_borrow_mut() {
                c.on = !c.on;
                save_boost_on(c.on);
            }
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(toggleSync:))]
        fn toggle_sync(&self, _sender: Option<&AnyObject>) {
            if let Ok(mut c) = self.ivars().core.try_borrow_mut() {
                c.sync = !c.sync;
                save_sync_disabled(!c.sync);
            }
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(checkUpdate:))]
        fn check_update(&self, _sender: Option<&AnyObject>) {
            if let Ok(c) = self.ivars().core.try_borrow() {
                crate::update::check_now(&c.update, &self.ivars().ctx);
            }
        }

        #[unsafe(method(openUpdate:))]
        fn open_update(&self, _sender: Option<&AnyObject>) {
            crate::update::open_page();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            if let Ok(mut c) = self.ivars().core.try_borrow_mut() {
                c.quit();
            }
        }
    }
);

impl Target {
    fn toggle_bubble(&self) {
        let iv = self.ivars();
        // A click on the icon first takes focus from the open bubble, which hides it; don't reopen it then.
        let just_hidden = iv.hidden_at.get().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        if iv.window.isVisible() {
            iv.window.orderOut(None);
        } else if !just_hidden {
            show(&iv.window, &iv.item, self.mtm());
            iv.ctx.request_repaint();
        }
    }

    fn show_menu(&self) {
        let mtm = self.mtm();
        let (on, sync) = self.ivars().core.try_borrow().map(|c| (c.on, c.sync)).unwrap_or((false, false));
        let menu = NSMenu::new(mtm);
        let add = |title: &str, action, key: &str| {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            unsafe { item.setTarget(Some(self)) };
            menu.addItem(&item);
        };
        add("Show Controls", sel!(showBubble:), "");
        add(if on { "Turn Boost Off" } else { "Turn Boost On" }, sel!(toggleBoost:), "");
        add(if sync { "Turn Display Sync Off" } else { "Turn Display Sync On" }, sel!(toggleSync:), "");
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        use crate::update::Status;
        let status = self.ivars().core.try_borrow().map(|c| c.update.lock().unwrap().clone()).unwrap_or(Status::Idle);
        match status {
            Status::Available(v) => add(&format!("Update Available: {v}…"), sel!(openUpdate:), ""),
            Status::Checking => add("Checking for Updates…", sel!(checkUpdate:), ""),
            Status::UpToDate => add("Up to Date. Check Again", sel!(checkUpdate:), ""),
            _ => add("Check for Updates…", sel!(checkUpdate:), ""),
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Quit Glow", sel!(quit:), "q");
        #[allow(deprecated)]
        self.ivars().item.popUpStatusItemMenu(&menu);
    }
}

pub struct Tray {
    window: Retained<NSWindow>,
    hidden_at: Rc<Cell<Option<Instant>>>,
    _target: Retained<Target>,
    _item: Retained<NSStatusItem>,
    _timer: Retained<NSTimer>,
}

impl Tray {
    pub fn new(ctx: egui::Context, core: Rc<RefCell<Core>>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        // At start-up the app's only window is the egui one.
        let window = NSApplication::sharedApplication(mtm).windows().firstObject()?;
        window.setLevel(NSPopUpMenuWindowLevel);
        window.setHasShadow(true);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Transient,
        );
        window.setAlphaValue(0.0); // invisible until the icon is first clicked

        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let hidden_at = Rc::new(Cell::new(None));
        let target: Retained<Target> = {
            let this = mtm.alloc::<Target>().set_ivars(Ivars {
                window: window.clone(),
                item: item.clone(),
                core: core.clone(),
                ctx,
                hidden_at: hidden_at.clone(),
            });
            unsafe { msg_send![super(this), init] }
        };
        if let Some(button) = item.button(mtm) {
            match NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("sun.max.fill"),
                Some(&NSString::from_str("Glow")),
            ) {
                Some(img) => {
                    img.setTemplate(true);
                    button.setImage(Some(&img));
                }
                None => button.setTitle(&NSString::from_str("☀")),
            }
            unsafe {
                button.setTarget(Some(&target));
                button.setAction(Some(sel!(iconClicked:)));
            }
            button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        }

        // Runs the brightness logic while the bubble is closed (egui stops drawing then), and pops the
        // bubble up when the brightness changes.
        let (w, it, popup) = (window.clone(), item.clone(), RefCell::new(Popup::default()));
        let wake_core = core.clone();
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                BACKGROUND_INTERVAL,
                true,
                &RcBlock::new(move |t: NonNull<NSTimer>| {
                    // Start quietly: the bubble stays closed until it's first shown (which makes it opaque).
                    if w.alphaValue() == 0.0 && w.isVisible() {
                        w.orderOut(None);
                    }
                    let Ok(mut c) = core.try_borrow_mut() else { return };
                    // Fade a frame at a time while something moves; otherwise keep the idle cadence.
                    let frame = NSScreen::mainScreen(mtm).map_or(60, |s| s.maximumFramesPerSecond()).max(1) as f64;
                    c.step(1.0 / frame as f32);
                    let next = if c.fast() { 1.0 / frame } else { BACKGROUND_INTERVAL };
                    t.as_ref().setFireDate(&NSDate::dateWithTimeIntervalSinceNow(next));
                    let total = c.total(c.current_sys());
                    let force = std::mem::take(&mut c.key_moved);
                    let mut p = popup.borrow_mut();
                    let wants = p.wants_show(total, force) && c.show_hud;
                    let opened_by_click = w.isVisible() && p.shown_at.is_none();
                    if wants && !opened_by_click {
                        place(&w, &it, mtm);
                        w.setAlphaValue(1.0);
                        w.orderFrontRegardless(); // in front, but the app you're typing in keeps focus
                        p.shown_at = Some(Instant::now());
                    }
                    let Some(t) = p.shown_at else { return };
                    let m = NSEvent::mouseLocation();
                    let f = w.frame();
                    let hovered = m.x >= f.origin.x && m.x <= f.origin.x + f.size.width && m.y >= f.origin.y && m.y <= f.origin.y + f.size.height;
                    if w.isKeyWindow() || !w.isVisible() {
                        p.shown_at = None; // clicked into (now an open bubble) or closed
                    } else if hovered {
                        p.shown_at = Some(Instant::now()); // stay while the pointer is on it
                    } else if t.elapsed() >= POPUP_VISIBLE + POPUP_FADE {
                        w.orderOut(None);
                        w.setAlphaValue(1.0);
                        p.shown_at = None;
                    } else if t.elapsed() > POPUP_VISIBLE {
                        w.setAlphaValue(1.0 - (t.elapsed() - POPUP_VISIBLE).as_secs_f64() / POPUP_FADE.as_secs_f64());
                    }
                }),
            )
        };

        // Menus and drags run the loop in other modes; keep ticking there too, and wake on a key press.
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        let t = timer.clone();
        wake_core.borrow().booster.on_key(Box::new(move || t.setFireDate(&NSDate::now())));

        Some(Tray { window, hidden_at, _target: target, _item: item, _timer: timer })
    }

    pub fn hide(&self) {
        if self.window.isVisible() {
            self.window.orderOut(None);
            self.hidden_at.set(Some(Instant::now()));
        }
    }
}

/// The icon's place on screen, once the menu bar has laid it out.
fn icon_frame(item: &NSStatusItem, mtm: MainThreadMarker) -> Option<NSRect> {
    let w = item.button(mtm)?.window()?;
    w.screen()?;
    let f = w.frame();
    (f.size.width > 0.0 && f.size.height > 0.0).then_some(f)
}

/// Put the bubble right under the menu-bar icon, its pointer centred on the icon, fully on that
/// screen, and bring it in front.
fn show(window: &NSWindow, item: &NSStatusItem, mtm: MainThreadMarker) {
    place(window, item, mtm);
    window.setAlphaValue(1.0);
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
}

/// Put the bubble right under the menu-bar icon, its pointer centred on the icon, fully on that screen.
fn place(window: &NSWindow, item: &NSStatusItem, mtm: MainThreadMarker) {
    if let Some(icon) = icon_frame(item, mtm) {
        let f = window.frame();
        let mut x = icon.origin.x + icon.size.width / 2.0 - f.size.width / 2.0;
        if let Some(vis) = item.button(mtm).and_then(|b| b.window()).and_then(|w| w.screen()).map(|s| s.visibleFrame()) {
            x = x.clamp(vis.origin.x + 4.0, vis.origin.x + vis.size.width - f.size.width - 4.0);
        }
        window.setFrameTopLeftPoint(NSPoint::new(x, icon.origin.y - 2.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_boost_is_off_by_default_and_remembered() {
        let defaults = NSUserDefaults::standardUserDefaults();
        defaults.removeObjectForKey(&NSString::from_str(BOOST_ON_KEY));
        assert!(!boost_on_saved()); // nothing saved: off
        save_boost_on(true);
        assert!(boost_on_saved());
        save_boost_on(false);
        assert!(!boost_on_saved());
        defaults.removeObjectForKey(&NSString::from_str(BOOST_ON_KEY)); // leave the test run's settings clean
    }

    #[test]
    fn pops_on_keys_and_boost_changes_only() {
        let mut p = Popup::default();
        assert!(!p.wants_show(0.5, false)); // first reading
        assert!(!p.wants_show(0.6, false)); // system range: macOS shows its own
        assert!(p.wants_show(0.7, true)); // a brightness key Glow handled
        assert!(p.wants_show(1.2, false)); // boost range
        assert!(!p.wants_show(1.2, false)); // no change
    }
}
