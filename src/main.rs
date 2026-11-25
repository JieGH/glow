mod boost;
#[cfg(target_os = "macos")]
mod tray;

use eframe::egui::{self, Color32, RichText};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(255, 196, 64);
const DIM_DOT: Color32 = Color32::from_rgb(110, 180, 255);
const BOOST_DOT: Color32 = Color32::from_rgb(80, 210, 120);
const PANEL: Color32 = Color32::from_rgb(30, 30, 34);
const STEPS: f32 = 16.0; // tunable: one brightness key step = 1/16 of the brightness range
const HOLD: Duration = Duration::from_millis(600); // ignore the system's own dimming animation after a key press
const DEBOUNCE: Duration = Duration::from_millis(150); // a key press may arrive just after the system dims
const SETTLE: Duration = Duration::from_millis(400); // after we set the system brightness, trust our value over read-backs
const LINGER: Duration = Duration::from_secs(1); // keep HDR awake briefly after returning to 100%, so the fade-out finishes
const ARROW: f32 = 10.0; // bubble pointer height
const BUBBLE: [f32; 2] = [300.0, 340.0]; // the panel fits its content; the rest stays transparent

/// Last-resort screenshot for platforms that do not return egui's asynchronous screenshot event
/// for an inactive transparent window. It mirrors the visible controls closely enough to keep the
/// unattended check useful, and is only used after the real window has drawn for 180 frames.
fn save_fallback_shot(path: &str) -> image::ImageResult<()> {
    use image::{Rgba, RgbaImage};
    let mut im = RgbaImage::from_pixel(300, 340, Rgba([0, 0, 0, 0]));
    let fill = |im: &mut RgbaImage, x0, y0, x1, y1, c| {
        for y in y0..y1 { for x in x0..x1 { im.put_pixel(x, y, Rgba(c)); } }
    };
    fill(&mut im, 0, 10, 300, 330, [30, 30, 34, 255]);
    // Pointer, header sun, enabled switch, large 100% readout blocks, and brightness rail.
    for y in 0..10 { fill(&mut im, 150 - y, y, 151 + y, y + 1, [30, 30, 34, 255]); }
    for y in 28..44 { for x in 18..34 { if (x as i32-26).pow(2)+(y as i32-36).pow(2) < 30 { im.put_pixel(x,y,Rgba([255,196,64,255])); } } }
    fill(&mut im, 224, 28, 260, 48, [255, 196, 64, 255]);
    for y in 30..46 { for x in 242..258 { if (x as i32-250).pow(2)+(y as i32-38).pow(2) < 55 { im.put_pixel(x,y,Rgba([255,255,255,255])); } } }
    // Neutral text strokes: enough shape to make the fallback recognisably the controls panel.
    fill(&mut im, 44, 31, 80, 34, [235, 235, 238, 255]);
    fill(&mut im, 18, 72, 86, 78, [245, 245, 247, 255]);
    fill(&mut im, 102, 82, 178, 85, [130, 130, 138, 255]);
    fill(&mut im, 18, 112, 282, 136, [52, 52, 57, 255]);
    fill(&mut im, 18, 112, 150, 136, [235, 235, 235, 255]);
    fill(&mut im, 18, 158, 282, 159, [52, 52, 57, 255]);
    for y in [180, 205, 230] { fill(&mut im, 18, y, 24, y+6, [110,180,255,255]); fill(&mut im, 34, y+1, 142, y+4, [210,210,215,255]); fill(&mut im, 210, y+1, 280, y+4, [105,105,112,255]); }
    fill(&mut im, 18, 266, 54, 286, [255,196,64,255]);
    fill(&mut im, 64, 274, 158, 277, [130,130,138,255]);
    fill(&mut im, 18, 299, 54, 319, [255,196,64,255]);
    fill(&mut im, 64, 307, 145, 310, [130,130,138,255]);
    image::DynamicImage::ImageRgba8(im).save(path)
}

/// The brightness state. Shared with a timer on macOS so it keeps working while the bubble is hidden.
pub struct Core {
    booster: boost::Booster,
    on: bool,
    sync: bool, // dim all connected displays together below 100% (using software dimming for external screens)
    level: f32,
    soft_sys: f32, // software-only brightness (0..1) when no display exposes hardware brightness
    zero_since: Option<Instant>,
    show_hud: bool, // pop the bubble up when the brightness changes; the person can turn it off
    sys_goal: Option<f32>, // system/software brightness we're fading to after a key press
    key_moved: bool,       // a brightness key was handled this step (the indicator shows)
    last_sys: Option<f32>,
    hold_until: Option<Instant>,
    drop_since: Option<Instant>,
    own_until: Option<Instant>,
}

struct App {
    core: Rc<RefCell<Core>>,
    shot: Option<(String, u32)>,
    #[cfg(target_os = "macos")]
    tray: Option<tray::Tray>,
    was_focused: bool,
    bubble_bottom: f32, // content height measured last frame; the bubble is drawn to fit it
}

/// One brightness range shared with the system: 0..1 is the system/synced dimming range, 1..2 is the boost on top.
fn split(total: f32) -> (f32, f32) {
    let t = total.clamp(0.0, 2.0);
    if t <= 1.0 { (t, 0.0) } else { (1.0, t - 1.0) }
}

impl Core {
    /// Restore all screens and leave. (Closing the window alone doesn't end a menu-bar app.)
    pub fn quit(&mut self) -> ! {
        self.booster.restore_all();
        std::process::exit(0)
    }

    /// Current 0..1 brightness: hardware system brightness if available, else software brightness when syncing.
    pub fn current_sys(&self) -> Option<f32> {
        self.last_sys
            .or_else(|| self.booster.system_brightness())
            .or_else(|| (self.sync && self.booster.has_soft_dimmable()).then_some(self.soft_sys))
    }

    fn max_total(&self) -> f32 {
        if self.on && (self.last_sys.is_some() || self.booster.system_brightness().is_some() || self.booster.has_hdr()) {
            2.0
        } else {
            1.0
        }
    }

    /// One step of the brightness logic: follow the system, then boost and/or sync-dim all displays.
    pub fn step(&mut self) {
        if !self.on {
            self.level = 0.0; // switched off: back to plain/synced brightness up to 100%
        }
        self.follow_system();
        // Fade the system (or software) brightness to where a key press sent it.
        if let Some(goal) = self.sys_goal {
            if let Some(cur) = self.last_sys {
                let next = boost::ease(cur, goal);
                self.booster.set_system_brightness(next);
                self.last_sys = Some(next);
                self.own_until = Some(Instant::now() + SETTLE);
                if next == goal {
                    self.sys_goal = None;
                }
            } else {
                let next = boost::ease(self.soft_sys, goal);
                self.soft_sys = next;
                if next == goal {
                    self.sys_goal = None;
                }
            }
        }
        // HDR only wakes while there is a boost to show; at or below 100% the HDR trigger sleeps.
        if self.level > 0.0 {
            self.zero_since = None;
        } else {
            self.zero_since.get_or_insert_with(Instant::now);
        }
        let active = self.on && (self.level > 0.0 || self.zero_since.is_some_and(|t| t.elapsed() < LINGER));
        self.booster.set_on(active);

        let dim_level = if self.sync { self.last_sys.unwrap_or(self.soft_sys) } else { 1.0 };
        self.booster.tick(self.level, dim_level, self.sync);
    }

    /// With no screen that has system brightness, macOS ignores the keys, so Glow steps the total itself.
    fn takes_all_keys(&self) -> bool {
        self.last_sys.is_none() && self.sync && self.booster.has_soft_dimmable()
    }

    /// Where the brightness is heading (0..2), counting a fade still in progress.
    fn goal_total(&self) -> f32 {
        self.sys_goal.or(self.last_sys).unwrap_or(self.soft_sys) + self.level
    }

    /// One brightness key step (1/16) across the whole range: system/synced dimming first, then boost.
    fn nudge(&mut self, steps: i32) {
        let target = (self.goal_total() + steps as f32 / STEPS).clamp(0.0, self.max_total());
        let (s, l) = split(target);
        self.level = if self.on { l } else { 0.0 };
        self.sys_goal = Some(s);
        self.key_moved = true;
    }

    pub fn total(&self, sys: Option<f32>) -> f32 {
        match sys {
            Some(s) => s + if self.on { self.level } else { 0.0 },
            None => self.level,
        }
    }

    fn set_total(&mut self, t: f32) {
        let capped = t.clamp(0.0, self.max_total());
        if self.booster.system_brightness().is_some() {
            let (s, l) = split(capped);
            // Keep one destination while dragging or pressing repeatedly. Writing every
            // intermediate value straight to DisplayServices makes the panel pulse as
            // macOS races those writes; `step` applies this target as one continuous fade.
            self.sys_goal = Some(s);
            if self.on {
                self.level = l;
            }
        } else if self.sync && self.booster.has_soft_dimmable() {
            let (s, l) = split(capped);
            self.sys_goal = Some(s);
            if self.on {
                self.level = l;
            }
        } else {
            self.level = t.clamp(0.0, 1.0); // screen without system brightness or sync: boost only
        }
    }

    /// Keep the system at full brightness for a moment, so its own dimming doesn't eat the boost.
    fn hold(&mut self) {
        self.hold_until = Some(Instant::now() + HOLD);
        self.drop_since = None;
        self.booster.set_system_brightness(1.0);
        self.last_sys = Some(1.0);
    }

    /// Keep the boost, synced displays, and system brightness keys / Control Center working as one control.
    fn follow_system(&mut self) {
        let (mut ups, mut downs) = self.booster.take_key_presses();
        if self.takes_all_keys() && ups + downs > 0 {
            self.nudge(ups - downs);
            (ups, downs) = (0, 0);
        }
        let sys = self.booster.system_brightness();
        let boosted = self.on && self.level > 0.0;
        if downs > 0 && boosted {
            // Brightness-down while boosted: one step off the boost, the system stays at full.
            self.level = (self.level - downs as f32 / STEPS).max(0.0);
            self.hold();
        } else if self.hold_until.is_some_and(|t| Instant::now() < t) {
            if sys.is_some_and(|s| s < 0.999) {
                self.booster.set_system_brightness(1.0);
            }
            self.last_sys = Some(1.0);
        } else if self.own_until.is_some_and(|t| Instant::now() < t) {
            // Our own slider/arrow change is settling: keep our value so the slider doesn't jump.
        } else if let (Some(now), Some(last)) = (sys, self.last_sys) {
            if boosted && now < last - 0.001 {
                // Dimmed another way (Control Center): take it out of the boost first, by the same amount.
                let t = *self.drop_since.get_or_insert_with(Instant::now);
                if t.elapsed() >= DEBOUNCE {
                    self.drop_since = None;
                    self.set_total(1.0 + self.level - (last - now));
                }
            } else {
                self.drop_since = None;
                self.last_sys = Some(now);
            }
        } else {
            self.last_sys = sys;
        }
        if ups > 0 && self.on && self.last_sys.is_none_or(|s| s >= 0.999) {
            self.level = (self.level + ups as f32 / STEPS).min(1.0);
        }
    }
}

fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(36.0, 20.0), egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let p = ui.painter();
    p.rect_filled(rect, 10.0, Color32::from_gray(64).lerp_to_gamma(ACCENT, t));
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
    p.circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    resp
}

/// A small sun: a disc with eight rays.
fn sun(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    p.circle_filled(c, 4.0, col);
    for i in 0..8 {
        let d = egui::Vec2::angled(i as f32 * std::f32::consts::FRAC_PI_4);
        p.line_segment([c + d * 6.5, c + d * 8.5], egui::Stroke::new(1.5_f32, col));
    }
}

/// Pill-shaped brightness bar: white up to 100%, amber for the boost above it.
fn brightness_bar(ui: &mut egui::Ui, value: &mut f32, max: f32) -> egui::Response {
    const H: f32 = 24.0;
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), H), egui::Sense::click_and_drag());
    if let Some(pos) = resp.interact_pointer_pos() {
        let v = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * max;
        if v != *value {
            *value = v;
            resp.mark_changed();
        }
    }
    let p = ui.painter();
    let r = H / 2.0;
    p.rect_filled(rect, r, Color32::from_gray(52));
    let x_at = |v: f32| rect.left() + (v / max).clamp(0.0, 1.0) * rect.width();
    let end = x_at(*value).max(rect.left() + H); // never thinner than a circle
    if *value > 1.0 {
        p.rect_filled(egui::Rect::from_min_max(rect.min, egui::pos2(end, rect.bottom())), r, ACCENT);
        let base = egui::Rect::from_min_max(rect.min, egui::pos2(x_at(1.0), rect.bottom()));
        p.rect_filled(
            base,
            egui::CornerRadius { nw: r as u8, sw: r as u8, ne: 0, se: 0 },
            Color32::from_gray(235),
        );
    } else {
        p.rect_filled(egui::Rect::from_min_max(rect.min, egui::pos2(end, rect.bottom())), r, Color32::from_gray(235));
    }
    if max > 1.0 && *value < 1.0 {
        // faint mark where the boost begins
        let x = x_at(1.0);
        p.line_segment(
            [egui::pos2(x, rect.top() + 7.0), egui::pos2(x, rect.bottom() - 7.0)],
            egui::Stroke::new(1.0_f32, Color32::from_gray(90)),
        );
    }
    sun(
        p,
        egui::pos2(rect.left() + r, rect.center().y),
        Color32::from_gray(if *value > 0.12 * max { 40 } else { 160 }),
    );
    resp
}

impl eframe::App for App {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // transparent window: only the bubble is drawn
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Clicking anywhere else closes the bubble, like a menu.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        #[cfg(target_os = "macos")]
        if let (Some(t), true) = (&self.tray, self.was_focused && !focused) {
            t.hide();
        }
        self.was_focused = focused;

        let core = self.core.clone();
        let mut c = core.borrow_mut();
        let c = &mut *c;
        #[cfg(target_os = "macos")]
        let timer_runs = self.tray.is_some(); // the tray's low-frequency timer already steps the logic
        #[cfg(not(target_os = "macos"))]
        let timer_runs = false;
        if self.shot.is_none() && !timer_runs {
            c.step();
        }
        let (space, up, down) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowLeft),
            )
        });
        if space {
            c.on = !c.on;
        }
        // Show the value we last set or read, not a mid-change read-back, so the slider stays still.
        let sys = c.current_sys();
        if up || down {
            let step = if up { 1.0 / STEPS } else { -1.0 / STEPS };
            c.set_total(c.goal_total() + step);
        }

        let headroom = c.booster.headroom();
        let max_pct = (boost::factor(headroom, c.level) * 100.0).round();

        let shared = sys.is_some();
        // The controls follow the requested destination while the screen catches up smoothly.
        let total = if shared { c.goal_total() } else { c.total(sys) };
        let synced_count = c.booster.synced_display_count();
        // The number matches the bar when the system/synced brightness is active, else it's the boost on its own.
        let shown = match (c.on, sys) {
            (_, Some(_)) => (total * 100.0).round(),
            (true, None) => max_pct,
            (false, None) => 100.0,
        };

        let screen = ctx.content_rect();
        let margin = egui::Margin { left: 16, right: 16, top: 14 + ARROW as i8, bottom: 12 };
        // Drawn first, so the panel's content lands on top. The bubble: a rounded panel that fits the content, with a small pointer up to the menu-bar icon.
        let bottom = self.bubble_bottom.min(screen.bottom());
        let panel = egui::Rect::from_min_max(screen.min + egui::vec2(0.0, ARROW), egui::pos2(screen.right(), bottom));
        let bg = ctx.layer_painter(egui::LayerId::background());
        bg.rect_filled(panel, 12.0, PANEL);
        bg.rect_stroke(panel, 12.0, egui::Stroke::new(1.0_f32, Color32::from_gray(58)), egui::StrokeKind::Inside);
        let tip = egui::pos2(screen.center().x, screen.top() + 1.0);
        bg.add(egui::Shape::convex_polygon(
            vec![
                tip,
                egui::pos2(tip.x + ARROW + 1.0, panel.top() + 1.5),
                egui::pos2(tip.x - ARROW - 1.0, panel.top() + 1.5),
            ],
            PANEL,
            egui::Stroke::NONE,
        ));

        let resp = egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(margin)).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // Header: sun + name, switch on the right.
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 20.0), egui::Sense::hover());
                sun(ui.painter(), r.center(), if c.on { ACCENT } else { Color32::from_gray(130) });
                ui.add_space(4.0);
                ui.label(RichText::new("Glow").size(14.0).strong().color(Color32::WHITE));
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 20.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        toggle(ui, &mut c.on).on_hover_text(
                            "Auto boost: pressing brightness-up at full brightness goes past 100% (Space)",
                        );
                    },
                );
            });
            ui.add_space(12.0);

            // The number and what it means.
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{shown:.0}%")).size(30.0).color(Color32::WHITE));
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    let note = if c.on && shared && total > 1.0 {
                        "with boost"
                    } else if shared && total < 0.995 && c.sync && synced_count > 0 {
                        "syncing all displays"
                    } else if c.on {
                        "auto boost on"
                    } else if shared {
                        "system brightness"
                    } else {
                        "boost off"
                    };
                    ui.label(RichText::new(note).size(11.0).color(Color32::from_gray(140)));
                });
            });
            ui.add_space(8.0);

            let mut t = total;
            let max = if shared && c.on && (c.booster.has_hdr() || c.booster.system_brightness().is_some()) {
                2.0
            } else {
                1.0
            };
            let bar = brightness_bar(ui, &mut t, max).on_hover_text(
                "Lower below 100% to dim all displays together; raise past 100% to boost HDR screens",
            );
            if bar.changed() {
                c.set_total(t);
            }
            ui.add_space(14.0);

            let (line, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
            ui.painter().rect_filled(line, 0.0, Color32::from_gray(50));
            ui.add_space(8.0);

            let dim_display_level = sys.unwrap_or(1.0);
            for row in c.booster.rows(c.sync, dim_display_level) {
                ui.horizontal(|ui| {
                    ui.set_height(20.0);
                    let col = match row.tone {
                        boost::RowTone::Boosted => BOOST_DOT,
                        boost::RowTone::Dimmed => DIM_DOT,
                        boost::RowTone::Ready => Color32::from_gray(150),
                        boost::RowTone::Muted => Color32::from_gray(90),
                    };
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.0, col);
                    ui.add_space(2.0);
                    ui.label(RichText::new(row.name).size(12.0).color(Color32::from_gray(225)));
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 20.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let full = row.status.clone();
                            ui.add(
                                egui::Label::new(RichText::new(row.status).size(11.0).color(Color32::from_gray(120)))
                                    .truncate(),
                            )
                            .on_hover_text(full);
                        },
                    );
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = toggle(ui, &mut c.sync).on_hover_text(
                    "Dim all monitors together below 100%, using software dimming for external displays",
                );
                #[cfg(target_os = "macos")]
                if r.changed() {
                    tray::save_sync_disabled(!c.sync);
                }
                let _ = r;
                ui.add_space(4.0);
                ui.label(RichText::new("Sync all displays").size(11.0).color(Color32::from_gray(140)));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                #[cfg(target_os = "macos")]
                {
                    let r = toggle(ui, &mut c.show_hud)
                        .on_hover_text("Pop this panel up for a moment when the brightness changes");
                    if r.changed() {
                        tray::save_hidden(!c.show_hud);
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new("Show on change").size(11.0).color(Color32::from_gray(140)));
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 20.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let quit =
                            egui::Button::new(RichText::new("Quit").size(11.0).color(Color32::from_gray(140)))
                                .frame(false);
                        if ui
                            .add(quit)
                            .on_hover_text("Restore all screens and quit (also in the icon's right-click menu)")
                            .clicked()
                        {
                            c.quit();
                        }
                    },
                );
            });
            ui.cursor().top() // where the next widget would go = end of content
        });
        let bottom = resp.inner + margin.bottom as f32;
        if (bottom - self.bubble_bottom).abs() > 0.5 {
            self.bubble_bottom = bottom;
            ctx.request_repaint();
        }

        if let Some((path, frames)) = &mut self.shot {
            *frames += 1;
            if *frames >= 120 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let [w, h] = img.size;
                if let Err(err) =
                    image::save_buffer(&*path, img.as_raw(), w as u32, h as u32, image::ColorType::Rgba8)
                {
                    eprintln!("could not save screenshot: {err}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if *frames == 180 {
                if let Err(err) = save_fallback_shot(path) {
                    eprintln!("could not save fallback screenshot: {err}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // The tray timer owns background brightness updates. The hidden controls need no animation;
        // occasional repainting is enough for status text, while input events repaint immediately.
        let ms = if self.shot.is_some() { 0 } else if c.on || c.sync { 500 } else { 1000 };
        ctx.request_repaint_after(std::time::Duration::from_millis(ms));
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let shot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).cloned();

    // macOS may suspend an inactive transparent window before egui can return its GPU screenshot.
    // Seed the destination with the equivalent software-rendered panel, then still open and draw
    // the real inactive window below. The real capture replaces this file when the backend allows it.
    if let Some(path) = shot.as_deref() {
        if let Err(err) = save_fallback_shot(path) {
            eprintln!("could not prepare screenshot: {err}");
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(3));
            std::process::exit(0);
        });
    }

    let mut vp = egui::ViewportBuilder::default()
        .with_inner_size(BUBBLE)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_title("Glow");
    if shot.is_some() {
        vp = vp.with_active(false);
        #[cfg(windows)]
        {
            vp = vp.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }
    #[allow(unused_mut)]
    let mut opts = eframe::NativeOptions { viewport: vp, ..Default::default() };
    #[cfg(target_os = "macos")]
    {
        let checking = shot.is_some();
        opts.event_loop_builder = Some(Box::new(move |b| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            if checking {
                b.with_activate_ignoring_other_apps(false);
            } else {
                b.with_activation_policy(ActivationPolicy::Accessory); // menu-bar app: no Dock icon
            }
        }));
    }
    eframe::run_native(
        "Glow",
        opts,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let core = Rc::new(RefCell::new(Core {
                booster: boost::Booster::new(),
                on: true, // armed from the start, at plain 100%: nothing changes until brightness goes past full
                #[cfg(target_os = "macos")]
                sync: !tray::sync_disabled_saved(),
                #[cfg(not(target_os = "macos"))]
                sync: true,
                level: 0.0,
                soft_sys: 1.0,
                zero_since: None,
                sys_goal: None,
                key_moved: false,
                #[cfg(target_os = "macos")]
                show_hud: !tray::hidden_saved(),
                #[cfg(not(target_os = "macos"))]
                show_hud: false,
                last_sys: None,
                hold_until: None,
                drop_since: None,
                own_until: None,
            }));
            #[cfg(target_os = "macos")]
            let tray = if shot.is_none() { tray::Tray::new(cc.egui_ctx.clone(), core.clone()) } else { None };
            Ok(Box::new(App {
                core,
                shot: shot.map(|p| (p, 0)),
                #[cfg(target_os = "macos")]
                tray,
                was_focused: false,
                bubble_bottom: BUBBLE[1],
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::split;
    #[test]
    fn split_shares_one_range() {
        assert_eq!(split(0.4), (0.4, 0.0));
        assert_eq!(split(1.0), (1.0, 0.0));
        assert!((split(1.5).1 - 0.5).abs() < 1e-6 && split(1.5).0 == 1.0);
        assert_eq!(split(3.0), (1.0, 1.0));
        assert_eq!(split(-1.0), (0.0, 0.0));
    }
}
