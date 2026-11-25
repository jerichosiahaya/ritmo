//! The floating windows: Playlist, Channel Rack, Piano Roll, Sampler and Mixer. They edit
//! `app.project` through its editing operations and draw previews from `app.snap`.

use crate::engine::*;
use crate::project::{CHOP_BASE, CHORDS, ChannelId, Clip, ClipId, DEFAULT_VEL, Edge, MAX_BARS, MAX_SLICES, Note, NoteId, PatternId, Project};
use crate::sample;
use crate::widgets::*;
use crate::{App, Win, channel_color, chord_color, pattern_color};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

const CLIP_TEXT: Color32 = Color32::from_rgb(22, 24, 28);

/// Keys the piano roll shows at once (two octaves and the C above).
pub const ROLL_KEYS: usize = 25;
const STEPS_PER_BAR: usize = 16;

pub fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 1)
}

fn is_black(n: u8) -> bool {
    matches!(n % 12, 1 | 3 | 6 | 8 | 10)
}

pub fn show(app: &mut App, ctx: &egui::Context, area: Rect) {
    let o = area.min;
    let roll_title = match (app.project.channel(app.roll.1), app.project.pattern(app.roll.0)) {
        (Some(c), Some(p)) => format!("Piano Roll — {} · {}", c.name, p.name),
        _ => "Piano Roll".into(),
    };
    let chop_title = app.chop_ch.and_then(|c| app.project.channel(c)).map_or("Sampler".into(), |c| format!("Sampler — {}", c.name));
    let specs: [(Win, &str, Pos2, [f32; 2]); 5] = [
        (Win::Playlist, "Playlist", o + vec2(16.0, 16.0), [860.0, 236.0]),
        (Win::Rack, "Channel Rack", o + vec2(16.0, 290.0), [560.0, 250.0]),
        (Win::Roll, &roll_title, o + vec2(80.0, 40.0), [820.0, 530.0]),
        (Win::Mixer, "Mixer", o + vec2(600.0, 290.0), [620.0, 270.0]),
        (Win::Chop, &chop_title, o + vec2(120.0, 40.0), [760.0, 430.0]),
    ];
    for (w, title, pos, size) in specs {
        let mut open = app.is_open(w);
        // A fixed ID keeps each window in place when its title changes.
        egui::Window::new(title)
            .id(egui::Id::unique(("window", w as usize)))
            .open(&mut open)
            .default_pos(pos)
            .default_size(size)
            .constrain_to(area)
            .resizable(true)
            .show(ctx, |ui| match w {
                Win::Playlist => playlist(app, ui),
                Win::Rack => rack(app, ui),
                Win::Roll => piano_roll(app, ui),
                Win::Mixer => mixer(app, ui),
                Win::Chop => sampler(app, ui),
            });
        if !open {
            app.open[w as usize] = false;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Channel Rack

fn rack(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        let name = app.project.pattern(app.pattern).map_or("—".into(), |p| p.name.clone());
        ui.label("Pattern");
        egui::ComboBox::from_id_salt("rack-pattern")
            .selected_text(name)
            .show_ui(ui, |ui| {
                for p in &app.project.patterns {
                    ui.selectable_value(&mut app.pattern, p.id, &p.name);
                }
            })
            .response
            .on_hover_text("Pattern whose steps the drum rows edit");
        if ui.small_button("＋").on_hover_text("New empty pattern").clicked() {
            app.pattern = app.project.new_pattern();
        }
    });
    ui.add_space(2.0);
    let pat = app.pattern;
    let steps = app.project.pattern(pat).map_or(STEPS_PER_BAR, |p| (p.len / STEP) as usize).clamp(STEPS_PER_BAR, 4 * STEPS_PER_BAR);
    let playing_step = app.cur.and_then(|t| app.project.pattern_pos(pat, t)).map(|t| (t / STEP) as usize);
    let mut delete = None;
    egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
        for i in 0..app.project.channels.len() {
            let color = channel_color(i);
            ui.horizontal(|ui| {
                let c = &mut app.project.channels[i];
                let (id, name, inst) = (c.id, c.name.clone(), c.instrument);
                let mut on = !c.mute;
                if led(ui, &mut on, &format!("{name} on/off")).changed() {
                    c.mute = !on;
                }
                knob(ui, &mut c.pan, -1.0..=1.0, 0.0, &format!("{name} pan"), color);
                knob(ui, &mut c.vol, 0.0..=1.0, 0.8, &format!("{name} volume"), color);
                let fill = color.gamma_multiply(if c.mute { 0.4 } else { 0.9 });
                let outline = if app.live == id { Stroke::new(1.5, ACCENT_HOT) } else { Stroke::NONE };
                let btn = egui::Button::new(RichText::new(&name).color(CLIP_TEXT).strong()).fill(fill).stroke(outline).min_size(vec2(84.0, 26.0));
                let hint = match inst {
                    _ if inst.is_drum() => "Click to play it from the keyboard",
                    Instrument::Chop => "Open in the Sampler",
                    _ => "Open in the piano roll",
                };
                let resp = ui.add(btn).on_hover_text(format!("{hint} · right-click to rename or delete"));
                if resp.clicked() {
                    if inst.is_drum() {
                        app.live = id;
                    } else if inst == Instrument::Chop {
                        app.select_chop(id);
                    } else {
                        app.edit_channel(id);
                    }
                }
                edit_menu(&resp, |ui| {
                    ui.label(RichText::new("Name").small().weak());
                    ui.text_edit_singleline(&mut app.project.channels[i].name);
                    if ui.button("🗑 Delete channel and its notes").clicked() {
                        delete = Some(id);
                        ui.close();
                    }
                });
                ui.add_space(6.0);
                if inst.is_drum() {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    for s in 0..steps {
                        if s > 0 && s % 4 == 0 {
                            ui.add_space(4.0);
                        }
                        let on = app.project.step(pat, id, s as Tick);
                        if step_button(ui, on, s / 4 % 2 == 0, playing_step == Some(s), color, &format!("{name} step {}", s + 1)).clicked() {
                            app.project.toggle_step(pat, id, s as Tick);
                        }
                    }
                } else {
                    strip_preview(ui, &app.snap, i, app.cur);
                }
            });
        }
    });
    if let Some(id) = delete {
        app.project.remove_channel(id);
        app.fix_selection();
    }
}

/// Melodic rows show what the channel plays across the song instead of step buttons.
fn strip_preview(ui: &mut Ui, snap: &Snapshot, ch: usize, cur: Option<Tick>) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0 * 21.0 + 12.0, 26.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3, BG_DEEP);
    let (a, b) = (snap.loop_start, snap.loop_end.max(snap.loop_start + 1));
    let x = |t: Tick| rect.left() + (t.clamp(a, b) - a) as f32 / (b - a) as f32 * rect.width();
    for bar in (a..b).step_by(4 * PPQ as usize).skip(1) {
        p.line_segment([Pos2::new(x(bar), rect.top()), Pos2::new(x(bar), rect.bottom())], Stroke::new(1.0, Color32::from_gray(60)));
    }
    let notes = || snap.notes.iter().filter(|n| n.channel as usize == ch && n.start < b && n.end > a);
    let (lo, hi) = notes().fold((u8::MAX, 0), |(lo, hi), n| (lo.min(n.pitch), hi.max(n.pitch)));
    let range = hi.saturating_sub(lo).max(1) as f32;
    for n in notes() {
        let y = rect.bottom() - 4.0 - n.pitch.saturating_sub(lo) as f32 / range * (rect.height() - 8.0);
        let r = Rect::from_min_max(Pos2::new(x(n.start), y - 1.0), Pos2::new(x(n.end).max(x(n.start) + 2.0), y + 1.0));
        p.rect_filled(r, 0, channel_color(ch));
    }
    if let Some(c) = cur {
        p.line_segment([Pos2::new(x(c), rect.top()), Pos2::new(x(c), rect.bottom())], Stroke::new(1.5, ACCENT_HOT));
    }
}

// ---------------------------------------------------------------------------------------------
// Piano Roll

const KEY_W: f32 = 46.0;
const CELL_W: f32 = 16.0;
const ROW_H: f32 = 14.0;
const RULER_H: f32 = 18.0;
/// Height of the velocity lane under the notes, and the gap above it.
const VEL_H: f32 = 56.0;
const VEL_GAP: f32 = 6.0;
/// How near a note's right end (in pixels) a drag changes its length instead of moving it.
const NOTE_EDGE: f32 = 5.0;

/// What piano-roll edits snap to. Holding Alt places freely.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NoteSnap {
    Beat,
    Eighth,
    Step,
    ThirtySecond,
}

impl NoteSnap {
    const ALL: [NoteSnap; 4] = [NoteSnap::Beat, NoteSnap::Eighth, NoteSnap::Step, NoteSnap::ThirtySecond];

    pub fn ticks(self) -> Tick {
        match self {
            NoteSnap::Beat => PPQ,
            NoteSnap::Eighth => PPQ / 2,
            NoteSnap::Step => STEP,
            NoteSnap::ThirtySecond => STEP / 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            NoteSnap::Beat => "1/4 (beat)",
            NoteSnap::Eighth => "1/8",
            NoteSnap::Step => "1/16 (step)",
            NoteSnap::ThirtySecond => "1/32",
        }
    }
}

/// A piano-roll drag in progress. Like playlist drags, moves and resizes show their result and
/// change the project when dropped. `grab` is where it was picked up: tick and row (from the
/// top, fractional).
pub enum NoteDrag {
    /// Notes moved together (copied if Ctrl is held at the drop); `anchor` snaps to the grid.
    Move { notes: Vec<NoteId>, anchor: NoteId, grab: (f32, f32) },
    /// Notes made longer or shorter together, by dragging `anchor`'s end.
    Resize { notes: Vec<NoteId>, anchor: NoteId, grab: f32 },
    /// A selection box (Ctrl+drag on empty space).
    Select { grab: (f32, f32) },
    /// Painting velocities in the velocity lane; applied as it goes.
    Velocity,
}

/// Applies `drag` to `p` as if dropped at tick `tick`, row `row`; returns the notes to select
/// after it (None: keep the selection).
fn drop_note_drag(p: &mut Project, (pat, ch, top): (PatternId, ChannelId, u8), drag: &NoteDrag, tick: f32, row: f32, unit: Tick, copy: bool) -> Option<Vec<NoteId>> {
    let snap = |t: f32| (t / unit as f32).round().max(0.0) as Tick * unit;
    let note = |p: &Project, id| p.pattern(pat).and_then(|x| x.notes.iter().find(|n| n.id == id).copied());
    match drag {
        NoteDrag::Move { notes, anchor, grab } => {
            let a = note(p, *anchor)?;
            let dt = i64::from(snap(a.start as f32 + tick - grab.0)) - i64::from(a.start);
            let dp = -(row - grab.1).round() as i64;
            Some(p.shift_notes(pat, notes, dt, dp, copy))
        }
        NoteDrag::Resize { notes, anchor, grab } => {
            let a = note(p, *anchor)?;
            let end = snap(a.end() as f32 + tick - grab).max(a.start + unit);
            p.resize_notes(pat, notes, i64::from(end) - i64::from(a.end()), unit.min(a.len).max(1));
            Some(notes.clone())
        }
        NoteDrag::Select { grab } => {
            let (t0, t1) = (grab.0.min(tick).max(0.0) as Tick, grab.0.max(tick).ceil() as Tick);
            let pitch = |r: f32| (i64::from(top) - r.floor() as i64).clamp(0, 127) as u8;
            let (lo, hi) = (pitch(grab.1.max(row)), pitch(grab.1.min(row)));
            Some(p.notes_in(pat, ch, t0, t1.max(t0 + 1), lo, hi))
        }
        NoteDrag::Velocity => None,
    }
}

fn piano_roll(app: &mut App, ui: &mut Ui) {
    let (pat, ch) = app.roll;
    let (Some(channel), Some(_)) = (app.project.channel(ch), app.project.pattern(pat)) else {
        ui.label("Pick a channel in the Channel Rack or the browser to edit its notes.");
        return;
    };
    let (ch_name, color) = (channel.name.clone(), channel_color(app.project.channel_index(ch).unwrap_or(0)));
    let is_chop = channel.instrument == Instrument::Chop;
    let ch_index = app.project.channel_index(ch).map_or(u16::MAX, |i| i as u16);
    let bl = app.project.bar_len();
    ui.horizontal(|ui| {
        ui.label(RichText::new(&ch_name).color(color).strong());
        if ui.button("Clear").on_hover_text(format!("Remove all of {ch_name}'s notes from this pattern")).clicked()
            && let Some(p) = app.project.pattern_mut(pat)
        {
            p.notes.retain(|n| n.channel != ch);
            app.roll_sel.clear();
        }
        let top = 127 - ROLL_KEYS as u8 + 1;
        if ui.add_enabled(app.roll_low < top, egui::Button::new("▲")).on_hover_text("Show an octave higher").clicked() {
            app.roll_low = (app.roll_low + 12).min(top);
        }
        if ui.add_enabled(app.roll_low > 0, egui::Button::new("▼")).on_hover_text("Show an octave lower").clicked() {
            app.roll_low = app.roll_low.saturating_sub(12);
        }
        ui.separator();
        ui.label("Snap");
        egui::ComboBox::from_id_salt("roll-snap")
            .width(90.0)
            .selected_text(app.roll_snap.name())
            .show_ui(ui, |ui| {
                for s in NoteSnap::ALL {
                    ui.selectable_value(&mut app.roll_snap, s, s.name());
                }
            })
            .response
            .on_hover_text("What notes snap to. Hold Alt while dragging to place freely.");
        ui.label(RichText::new("Click: add · drag: move · drag the end: length · Ctrl+drag: select · right-click: delete").small().weak())
            .on_hover_text("New notes get the length of the last note you drew, resized or clicked.\nCtrl+drag a note copies it. Delete removes the selection; arrows move it (Shift+↑↓: an octave).\nCtrl+A selects all, Ctrl+C / Ctrl+X / Ctrl+V copy, cut and paste, Ctrl+D duplicates.\nThe lane under the notes sets velocity: drag over a note's stem.");
    });
    let low = app.roll_low;
    let top_pitch = low + ROLL_KEYS as u8 - 1;
    let (alt, command, pointer) = ui.input(|i| (i.modifiers.alt, i.modifiers.command, i.pointer.latest_pos()));
    let unit = if alt { 1 } else { app.roll_snap.ticks() };
    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        let top_left = ui.cursor().min;
        let gx = top_left.x + KEY_W;
        let gy = top_left.y + RULER_H;
        let tick_at = |x: f32| ((x - gx) / CELL_W * STEP as f32).max(0.0);
        let row_at = |y: f32| (y - gy) / ROW_H;
        let px = |t: Tick| gx + t as f32 / STEP as f32 * CELL_W;

        // While moving, resizing or box-selecting, draw the result of dropping here.
        let mut preview = None;
        let mut selected = app.roll_sel.clone();
        if let (Some(drag), Some(pos)) = (&app.roll_drag, pointer)
            && !matches!(drag, NoteDrag::Velocity)
        {
            let mut p = app.project.clone();
            if let Some(sel) = drop_note_drag(&mut p, (pat, ch, top_pitch), drag, tick_at(pos.x), row_at(pos.y), unit, command) {
                selected = sel;
            }
            if !matches!(drag, NoteDrag::Select { .. }) {
                preview = Some(p);
            }
        }
        // What to draw: this channel's notes, each with whether a playlist clip plays it.
        let (notes, audible, pattern_len, playhead) = {
            let project = preview.as_ref().unwrap_or(&app.project);
            let Some(pattern) = project.pattern(pat) else { return };
            let notes: Vec<Note> = pattern.notes.iter().filter(|n| n.channel == ch).copied().collect();
            let audible: Vec<bool> = notes.iter().map(|n| project.clips.iter().any(|c| c.pattern == pat && project.clip_plays(c, n.start))).collect();
            (notes, audible, pattern.len, app.cur.and_then(|t| project.pattern_pos(pat, t)))
        };
        // The pattern, plus a bar to grow into.
        let ticks = (pattern_len.div_ceil(bl) + 1) * bl;
        let steps = (ticks / STEP) as usize;
        let size = vec2(KEY_W + CELL_W * steps as f32, RULER_H + ROW_H * ROLL_KEYS as f32 + VEL_GAP + VEL_H);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let grid = Rect::from_min_max(Pos2::new(gx, gy), Pos2::new(rect.right(), gy + ROW_H * ROLL_KEYS as f32));
        // The keyboard stays at the left edge when scrolled sideways, drawn over the notes.
        let kx = rect.left().max(ui.clip_rect().left());
        let keys = Rect::from_min_max(Pos2::new(kx, gy), Pos2::new(kx + KEY_W, grid.bottom()));
        let lane = Rect::from_min_max(Pos2::new(gx, grid.bottom() + VEL_GAP), rect.max);
        let pitch_at = |y: f32| {
            let row = row_at(y).floor();
            (0.0..ROLL_KEYS as f32).contains(&row).then(|| top_pitch - row as u8)
        };
        let note_rect = |n: &Note| {
            let y = gy + (top_pitch - n.pitch) as f32 * ROW_H;
            Rect::from_min_max(Pos2::new(px(n.start), y), Pos2::new(px(n.end()), y + ROW_H)).shrink(1.0)
        };
        let visible = low..=top_pitch;
        let hit = |pos: Pos2| notes.iter().rev().filter(|n| visible.contains(&n.pitch)).find(|n| note_rect(n).expand(1.0).contains(pos)).copied();

        // Keyboard: hold a key to hear it.
        let kresp = ui.interact(keys, ui.scope_id().with("roll-keys"), Sense::drag()).on_hover_text("Hold a key to hear it");
        let key_held = kresp.is_pointer_button_down_on().then(|| kresp.interact_pointer_pos()).flatten().and_then(|p| pitch_at(p.y));

        // Notes.
        let gresp = ui.interact(grid, ui.scope_id().with("roll-grid"), Sense::click_and_drag());
        if let Some(h) = gresp.hover_pos()
            && app.roll_drag.is_none()
            && hit(h).is_some_and(|n| h.x > note_rect(&n).right() - NOTE_EDGE)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if gresp.is_pointer_button_down_on() || gresp.clicked() || gresp.secondary_clicked() {
            app.roll_focus = true;
        }
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        let mut sound = None;
        if gresp.drag_started_by(egui::PointerButton::Primary)
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
        {
            let grab = (tick_at(o.x), row_at(o.y));
            app.roll_drag = match hit(o) {
                Some(n) => {
                    if !app.roll_sel.contains(&n.id) {
                        app.roll_sel = vec![n.id];
                    }
                    let picked = app.roll_sel.clone();
                    Some(if o.x > note_rect(&n).right() - NOTE_EDGE {
                        NoteDrag::Resize { notes: picked, anchor: n.id, grab: grab.0 }
                    } else {
                        NoteDrag::Move { notes: picked, anchor: n.id, grab }
                    })
                }
                None if command => Some(NoteDrag::Select { grab }),
                // On empty space a drag draws a note and moves it.
                None => pitch_at(o.y).and_then(|pitch| {
                    let at = tick_at(o.x) as Tick / unit * unit;
                    let id = app.project.draw_note(pat, ch, pitch, at, app.note_len, DEFAULT_VEL)?;
                    app.roll_sel = vec![id];
                    Some(NoteDrag::Move { notes: vec![id], anchor: id, grab })
                }),
            };
        } else if gresp.clicked()
            && let Some(pos) = gresp.interact_pointer_pos()
        {
            match hit(pos) {
                Some(n) if command => {
                    if let Some(i) = app.roll_sel.iter().position(|&s| s == n.id) {
                        app.roll_sel.remove(i);
                    } else {
                        app.roll_sel.push(n.id);
                    }
                }
                Some(n) => {
                    app.roll_sel = vec![n.id];
                    app.note_len = n.len;
                }
                None if command => app.roll_sel.clear(),
                None => {
                    if let Some(pitch) = pitch_at(pos.y) {
                        let at = tick_at(pos.x) as Tick / unit * unit;
                        app.roll_sel = app.project.draw_note(pat, ch, pitch, at, app.note_len, DEFAULT_VEL).into_iter().collect();
                    }
                }
            }
        }
        // Hear the note being pressed or dragged.
        if gresp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()) {
            sound = match &app.roll_drag {
                Some(NoteDrag::Move { anchor, .. } | NoteDrag::Resize { anchor, .. }) => notes.iter().find(|n| n.id == *anchor).map(|n| n.pitch),
                Some(_) => None,
                None if pressed => pointer.and_then(|p| hit(p).map(|n| n.pitch).or_else(|| pitch_at(p.y))),
                None => app.preview,
            };
        }
        // Right button: delete the notes it touches.
        if gresp.is_pointer_button_down_on()
            && ui.input(|i| i.pointer.secondary_down())
            && let Some(n) = pointer.and_then(hit)
        {
            app.project.remove_notes(pat, &[n.id]);
            app.roll_sel.retain(|&s| s != n.id);
        }

        // Velocity lane: drag over notes' stems to set how hard they play.
        let vresp = ui.interact(lane, ui.scope_id().with("roll-velocity"), Sense::click_and_drag()).on_hover_text("Velocity: drag up or down over a note's stem (in a chord, the selected notes)");
        if (vresp.is_pointer_button_down_on() || vresp.clicked())
            && let Some(pos) = vresp.interact_pointer_pos()
        {
            app.roll_focus = true;
            app.roll_drag.get_or_insert(NoteDrag::Velocity);
            let vel = ((lane.bottom() - pos.y) / VEL_H * 127.0).round().clamp(1.0, 127.0) as u8;
            let near = (CELL_W * 0.5).max(4.0);
            // Notes sharing a column (a chord): the selected ones, if any are there.
            let under: Vec<NoteId> = notes.iter().filter(|n| (px(n.start) - pos.x).abs() <= near).map(|n| n.id).collect();
            let picked: Vec<NoteId> = under.iter().copied().filter(|id| app.roll_sel.contains(id)).collect();
            app.project.set_velocity(pat, if picked.is_empty() { &under } else { &picked }, vel);
        }

        // Drop a drag when the button comes up.
        if app.roll_drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
            let drag = app.roll_drag.take().expect("checked above");
            if let Some(pos) = pointer
                && let Some(sel) = drop_note_drag(&mut app.project, (pat, ch, top_pitch), &drag, tick_at(pos.x), row_at(pos.y), unit, command)
            {
                app.roll_sel = sel;
                if let NoteDrag::Resize { anchor, .. } | NoteDrag::Move { anchor, .. } = drag
                    && let Some(n) = app.project.pattern(pat).and_then(|p| p.notes.iter().find(|n| n.id == anchor))
                {
                    app.note_len = n.len;
                }
            }
        }

        // Preview sound: keyboard, or the note under the pointer.
        let want = key_held.or(sound);
        if app.preview != want {
            if let Some(old) = app.preview {
                app.send(Cmd::NoteOff { channel: ch_index, pitch: old });
            }
            if let Some(n) = want {
                app.send(Cmd::NoteOn { channel: ch_index, pitch: n });
            }
            app.preview = want;
        }

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0, BG_DEEP);
        // Ruler.
        for s in (0..steps).step_by(4) {
            let x = gx + s as f32 * CELL_W;
            if (s as Tick * STEP).is_multiple_of(bl) {
                p.text(Pos2::new(x + 3.0, rect.top() + RULER_H / 2.0), Align2::LEFT_CENTER, format!("{}", s as Tick * STEP / bl + 1), FontId::proportional(12.0), Color32::from_gray(200));
            } else {
                p.line_segment([Pos2::new(x, rect.top() + RULER_H - 5.0), Pos2::new(x, rect.top() + RULER_H)], Stroke::new(1.0, Color32::from_gray(110)));
            }
        }
        // Rows.
        for k in 0..ROLL_KEYS {
            // Chop pads aren't notes of a scale: every row is a plain row.
            let black = !is_chop && is_black(top_pitch - k as u8);
            let row = Rect::from_min_size(Pos2::new(gx, gy + k as f32 * ROW_H), vec2(grid.width(), ROW_H));
            p.rect_filled(row, 0, if black { Color32::from_rgb(36, 40, 45) } else { Color32::from_rgb(47, 52, 58) });
        }
        let draw_keys = |p: &egui::Painter| {
            p.rect_filled(keys.expand2(vec2(0.0, RULER_H)).with_max_y(lane.bottom()), 0, BG_DEEP);
            for k in 0..ROLL_KEYS {
                let pitch = top_pitch - k as u8;
                let black = !is_chop && is_black(pitch);
                let y = gy + k as f32 * ROW_H;
                // Black keys are shorter, drawn over the white key area like a real keyboard.
                let key_w = if black { (KEY_W - 2.0) * 0.62 } else { KEY_W - 2.0 };
                let key = Rect::from_min_size(Pos2::new(kx, y), vec2(key_w, ROW_H));
                if black {
                    p.rect_filled(Rect::from_min_size(Pos2::new(kx, y), vec2(KEY_W - 2.0, ROW_H)), 0, Color32::from_rgb(222, 222, 216));
                }
                let fill = match (black, app.preview == Some(pitch)) {
                    (_, true) => ACCENT,
                    (true, _) => Color32::from_rgb(28, 28, 30),
                    (false, _) => Color32::from_rgb(222, 222, 216),
                };
                p.rect_filled(key.shrink2(vec2(0.0, 0.5)), 1, fill);
                if is_chop {
                    if let Some(pad) = pitch.checked_sub(CHOP_BASE).filter(|&k| (k as usize) < MAX_SLICES) {
                        p.text(key.right_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, format!("Pad {}", pad + 1), FontId::proportional(9.0), Color32::from_gray(60));
                    }
                } else if pitch.is_multiple_of(12) {
                    p.text(key.right_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, note_name(pitch), FontId::proportional(10.0), Color32::from_gray(60));
                }
            }
        };
        // Grid lines; the pattern's end.
        for s in 0..=steps {
            let x = gx + s as f32 * CELL_W;
            let t = s as Tick * STEP;
            let (w, c) = match t {
                t if t.is_multiple_of(bl) => (1.5, Color32::from_gray(120)),
                t if t.is_multiple_of(PPQ) => (1.0, Color32::from_gray(78)),
                _ => (1.0, Color32::from_gray(56)),
            };
            p.line_segment([Pos2::new(x, grid.top()), Pos2::new(x, lane.bottom())], Stroke::new(w, c));
        }
        let end = px(pattern_len);
        p.rect_filled(Rect::from_min_max(Pos2::new(end, grid.top()), grid.max), 0, Color32::from_black_alpha(70));
        // Notes: brighter when louder, dimmed where no playlist clip plays that part of the
        // pattern, outlined when selected.
        for (n, &heard) in notes.iter().zip(&audible).filter(|(n, _)| visible.contains(&n.pitch)) {
            let r = note_rect(n);
            let loud = 0.45 + 0.55 * n.vel as f32 / 127.0;
            let col = if heard { color.gamma_multiply(loud) } else { color.gamma_multiply(0.3) };
            p.rect_filled(r, 3, col);
            let outline = if selected.contains(&n.id) { Stroke::new(2.0, Color32::WHITE) } else { Stroke::new(1.0, col.gamma_multiply(0.5)) };
            p.rect_stroke(r, 3, outline, StrokeKind::Inside);
            if r.width() > 26.0 {
                p.text(r.left_center() + vec2(3.0, 0.0), Align2::LEFT_CENTER, note_name(n.pitch), FontId::proportional(9.0), CLIP_TEXT);
            }
        }
        // Velocity stems.
        p.rect_filled(lane, 0, BG_PANEL.gamma_multiply(0.6));
        for n in &notes {
            let x = px(n.start) + 1.5;
            let y = lane.bottom() - n.vel as f32 / 127.0 * (VEL_H - 4.0);
            let col = if selected.contains(&n.id) { Color32::WHITE } else { color };
            p.line_segment([Pos2::new(x, lane.bottom()), Pos2::new(x, y)], Stroke::new(2.0, col));
            p.circle_filled(Pos2::new(x, y), 2.5, col);
        }
        if let (Some(NoteDrag::Select { grab }), Some(pos)) = (&app.roll_drag, pointer) {
            let from = Pos2::new(px(grab.0 as Tick), gy + grab.1 * ROW_H);
            let r = Rect::from_two_pos(from, pos).intersect(grid);
            p.rect_filled(r, 0, ACCENT.gamma_multiply(0.15));
            p.rect_stroke(r, 0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
        }
        if let Some(t) = playhead {
            p.line_segment([Pos2::new(px(t), rect.top()), Pos2::new(px(t), lane.bottom())], Stroke::new(2.0, ACCENT_HOT));
        }
        draw_keys(&p);
    });
}

// ---------------------------------------------------------------------------------------------
// Playlist

const LABEL_W: f32 = 96.0;
const TRACK_H: f32 = 40.0;
/// How near a clip's edge (in pixels) a drag resizes it instead of moving it.
const EDGE_W: f32 = 7.0;
/// Bar widths the zoom allows, in pixels.
pub const BAR_W: std::ops::RangeInclusive<f32> = 24.0..=480.0;

/// What playlist drags snap to. Holding Alt places freely.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Snap {
    Bar,
    Beat,
    Step,
}

impl Snap {
    const ALL: [Snap; 3] = [Snap::Bar, Snap::Beat, Snap::Step];

    fn ticks(self, bar: Tick) -> Tick {
        match self {
            Snap::Bar => bar,
            Snap::Beat => PPQ,
            Snap::Step => STEP,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Snap::Bar => "Bar",
            Snap::Beat => "Beat",
            Snap::Step => "Step",
        }
    }
}

/// A playlist drag in progress. The playlist shows what dropping it would do; the project
/// changes when it is dropped. `grab` is where it was picked up: song tick and track row.
pub enum ClipDrag {
    /// Selected clips moved together (copied if Ctrl is held at the drop); `anchor`, the one
    /// grabbed, snaps to the grid.
    Move { clips: Vec<ClipId>, anchor: ClipId, grab: (f32, f32) },
    Resize { clip: ClipId, edge: Edge, grab: (f32, f32) },
    /// A loop range being dragged out on the ruler from `from`.
    Loop { from: Tick },
}

/// Applies `drag` to `p` as if dropped at song tick `tick`, track row `row`; returns the clips
/// to select after it.
fn drop_drag(p: &mut Project, drag: &ClipDrag, tick: f32, row: f32, unit: Tick, copy: bool) -> Vec<ClipId> {
    let snap = |t: f32| (t / unit as f32).round().max(0.0) as Tick * unit;
    match drag {
        ClipDrag::Move { clips, anchor, grab } => {
            let Some(a) = p.clips.iter().find(|c| c.id == *anchor) else { return Vec::new() };
            let dt = i64::from(snap(a.start as f32 + tick - grab.0)) - i64::from(a.start);
            p.shift_clips(clips, dt, (row - grab.1).round() as i64, copy)
        }
        ClipDrag::Resize { clip, edge, grab } => {
            let Some(c) = p.clips.iter().find(|c| c.id == *clip).copied() else { return Vec::new() };
            let at = if *edge == Edge::Start { c.start } else { c.end() };
            p.resize_clip(*clip, *edge, snap(at as f32 + tick - grab.0), unit.max(STEP));
            vec![*clip]
        }
        ClipDrag::Loop { from } => {
            let to = snap(tick);
            if to != *from {
                p.set_loop(*from, to);
            }
            Vec::new()
        }
    }
}

/// "3" for the start of bar 3, "3.2" for its second beat (in ticks otherwise).
fn bar_beat(t: Tick, bar: Tick) -> String {
    match (t % bar, t % PPQ) {
        (0, _) => format!("{}", t / bar + 1),
        (b, 0) => format!("{}.{}", t / bar + 1, b / PPQ + 1),
        _ => format!("{}.{}+{}", t / bar + 1, t % bar / PPQ + 1, t % PPQ),
    }
}

fn playlist_header(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if ui.small_button("＋ Track").on_hover_text("Add an empty playlist track").clicked() {
            app.project.new_track();
        }
        ui.separator();
        ui.label("Snap");
        egui::ComboBox::from_id_salt("playlist-snap")
            .width(60.0)
            .selected_text(app.grid.name())
            .show_ui(ui, |ui| {
                for s in Snap::ALL {
                    ui.selectable_value(&mut app.grid, s, s.name());
                }
            })
            .response
            .on_hover_text("What clips and the loop snap to while dragging. Hold Alt to place freely.");
        if ui.small_button("－").on_hover_text("Zoom out (Ctrl+scroll)").clicked() {
            app.bar_w = (app.bar_w / 1.25).clamp(*BAR_W.start(), *BAR_W.end());
        }
        if ui.small_button("＋").on_hover_text("Zoom in (Ctrl+scroll)").clicked() {
            app.bar_w = (app.bar_w * 1.25).clamp(*BAR_W.start(), *BAR_W.end());
        }
        ui.separator();
        let bl = app.project.bar_len();
        match app.project.loop_range {
            Some((a, b)) => {
                let text = match (a % bl, b % bl) {
                    (0, 0) if b - a == bl => format!("⟲ Loop bar {}", a / bl + 1),
                    (0, 0) => format!("⟲ Loop bars {}–{}", a / bl + 1, b / bl),
                    _ => format!("⟲ Loop {} → {}", bar_beat(a, bl), bar_beat(b, bl)),
                };
                ui.label(RichText::new(text).color(ACCENT));
                if ui.small_button("✕").on_hover_text("Clear the loop: play the whole song").clicked() {
                    app.project.loop_range = None;
                }
            }
            None => {
                ui.label(RichText::new("Drag on the ruler to loop part of the song").small().weak());
            }
        }
    });
}

fn playlist(app: &mut App, ui: &mut Ui) {
    playlist_header(app, ui);
    let bl = app.project.bar_len();
    let (alt, command, pointer) = ui.input(|i| (i.modifiers.alt, i.modifiers.command, i.pointer.latest_pos()));
    let unit = if alt { 1 } else { app.grid.ticks(bl) };
    // Ctrl+scroll zooms.
    let zoom = ui.input(|i| i.zoom_delta());
    if zoom != 1.0 && ui.rect_contains_pointer(ui.max_rect()) {
        app.bar_w = (app.bar_w * zoom).clamp(*BAR_W.start(), *BAR_W.end());
    }
    let mut delete = None;
    egui::ScrollArea::both().auto_shrink([false, true]).show(ui, |ui| {
        let bar_w = app.bar_w;
        let top_left = ui.cursor().min;
        let gx = top_left.x + LABEL_W;
        let x = |t: Tick| gx + t as f32 / bl as f32 * bar_w;
        let tick_at = |px: f32| ((px - gx) / bar_w * bl as f32).max(0.0);
        let row_at = |py: f32| (py - top_left.y - RULER_H) / TRACK_H;

        // While dragging, draw the playlist as the drop would leave it.
        let mut view = None;
        if let (Some(drag), Some(pos)) = (&app.clip_drag, pointer) {
            let mut p = app.project.clone();
            let sel = drop_drag(&mut p, drag, tick_at(pos.x), row_at(pos.y), unit, command);
            view = Some((p.clips, p.loop_range, sel));
        }
        let (clips, loop_range, selected) = match view {
            Some(v) => v,
            None => (app.project.clips.clone(), app.project.loop_range, app.sel_clips.clone()),
        };

        // The song, the loop and room to grow into.
        let end = clips.iter().map(Clip::end).chain(loop_range.map(|l| l.1)).max().unwrap_or(0);
        let bars = (end.div_ceil(bl) + 8).clamp(16, MAX_BARS);
        let rows = app.project.tracks.len();
        let size = vec2(LABEL_W + bar_w * bars as f32, RULER_H + TRACK_H * rows as f32);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let lanes = Rect::from_min_max(Pos2::new(gx, rect.top() + RULER_H), rect.max);
        ui.painter().rect_filled(rect, 0, BG_DEEP);

        // Ruler: click selects a bar, drag sets the loop, double-click clears it.
        let ruler = Rect::from_min_max(Pos2::new(gx, rect.top()), Pos2::new(rect.right(), rect.top() + RULER_H));
        let resp = ui.interact(ruler, ui.scope_id().with("ruler"), Sense::click_and_drag()).on_hover_text("Click: select a bar · drag: loop that part · double-click: clear the loop");
        let snap = |t: f32| (t / unit as f32).round() as Tick * unit;
        if resp.drag_started()
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
        {
            app.clip_drag = Some(ClipDrag::Loop { from: snap(tick_at(o.x)) });
        }
        if resp.double_clicked() {
            app.project.loop_range = None;
        } else if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            app.sel_bar = tick_at(pos.x) as Tick / bl;
        }
        let p = ui.painter();
        if let Some((a, b)) = loop_range {
            let band = Rect::from_min_max(Pos2::new(x(a), ruler.top()), Pos2::new(x(b), ruler.bottom()));
            p.rect_filled(band, 2, ACCENT.gamma_multiply(0.45));
        }
        let label_every = ((40.0 / bar_w).ceil() as Tick).max(1);
        for bar in (0..bars).step_by(label_every as usize) {
            let col = if app.sel_bar == bar { ACCENT_HOT } else { Color32::from_gray(190) };
            p.text(Pos2::new(x(bar * bl) + 4.0, ruler.center().y), Align2::LEFT_CENTER, format!("{}", bar + 1), FontId::proportional(12.0), col);
        }

        for row in 0..rows {
            let t = &app.project.tracks[row];
            let (track, name, muted) = (t.id, t.name.clone(), t.mute);
            let y = lanes.top() + row as f32 * TRACK_H;
            // Track label: click mutes.
            let label = Rect::from_min_size(Pos2::new(rect.left(), y), vec2(LABEL_W - 4.0, TRACK_H)).shrink(2.0);
            let resp = ui.interact(label, ui.scope_id().with(("track", row)), Sense::click()).on_hover_text("Click to mute or unmute · right-click to rename or delete");
            if resp.clicked() {
                app.project.tracks[row].mute ^= true;
            }
            edit_menu(&resp, |ui| {
                ui.label(RichText::new("Name").small().weak());
                ui.text_edit_singleline(&mut app.project.tracks[row].name);
                if ui.button("🗑 Delete track and its clips").clicked() {
                    delete = Some(track);
                    ui.close();
                }
            });
            let p = ui.painter();
            p.rect_filled(label, 3, BG_WINDOW);
            p.rect_filled(Rect::from_min_size(label.min, vec2(4.0, label.height())), 2, if muted { Color32::from_gray(80) } else { channel_color(row * 2) });
            let text_col = if muted { Color32::from_gray(110) } else { Color32::from_gray(220) };
            p.text(label.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, &name, FontId::proportional(13.0), text_col);

            // The empty lane: click places the selected pattern.
            let lane = Rect::from_min_size(Pos2::new(gx, y), vec2(bar_w * bars as f32, TRACK_H));
            p.rect_filled(lane, 0, if row % 2 == 0 { Color32::from_rgb(40, 44, 50) } else { Color32::from_rgb(36, 40, 45) });
            let pattern_name = app.project.pattern(app.pattern).map_or("—".into(), |p| p.name.clone());
            let resp = ui.interact(lane, ui.scope_id().with(("lane", row)), Sense::click()).on_hover_text(format!("Click: place {pattern_name} (pick another in the browser or Channel Rack)"));
            if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                app.roll_focus = false;
                let at = tick_at(pos.x) as Tick / unit * unit;
                if let Some(c) = app.project.place_clip(track, app.pattern, at) {
                    app.sel_clips = vec![c];
                }
                (app.sel_track, app.sel_bar) = (Some(track), at / bl);
            }

            for &c in clips.iter().filter(|c| c.track == track) {
                let r = Rect::from_min_max(Pos2::new(x(c.start), y), Pos2::new(x(c.end()), y + TRACK_H)).shrink2(vec2(1.0, 2.0));
                let resp = ui.interact(r, ui.scope_id().with(("clip", c.id)), Sense::click_and_drag());
                let edge_w = EDGE_W.min(r.width() / 4.0);
                let edge_at = |px: f32| {
                    if px < r.left() + edge_w {
                        Some(Edge::Start)
                    } else if px > r.right() - edge_w {
                        Some(Edge::End)
                    } else {
                        None
                    }
                };
                let is_sel = selected.contains(&c.id);
                if resp.is_pointer_button_down_on() {
                    app.roll_focus = false;
                }
                if app.clip_drag.is_none()
                    && let Some(h) = resp.hover_pos()
                    && edge_at(h.x).is_some()
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let bar_here = || resp.interact_pointer_pos().map_or(c.start, |p| tick_at(p.x) as Tick) / bl;
                if resp.drag_started()
                    && let Some(o) = ui.input(|i| i.pointer.press_origin())
                {
                    let grab = (tick_at(o.x), row_at(o.y));
                    app.clip_drag = Some(match edge_at(o.x) {
                        Some(edge) => ClipDrag::Resize { clip: c.id, edge, grab },
                        None => {
                            if !app.sel_clips.contains(&c.id) {
                                app.sel_clips = vec![c.id];
                            }
                            ClipDrag::Move { clips: app.sel_clips.clone(), anchor: c.id, grab }
                        }
                    });
                }
                if resp.double_clicked() {
                    edit_pattern(app, c.pattern);
                } else if resp.clicked() {
                    if command {
                        if let Some(i) = app.sel_clips.iter().position(|&s| s == c.id) {
                            app.sel_clips.remove(i);
                        } else {
                            app.sel_clips.push(c.id);
                        }
                    } else {
                        app.sel_clips = vec![c.id];
                    }
                    (app.sel_track, app.sel_bar) = (Some(track), bar_here());
                }
                if resp.secondary_clicked() {
                    if !app.sel_clips.contains(&c.id) {
                        app.sel_clips = vec![c.id];
                    }
                    (app.sel_track, app.sel_bar) = (Some(track), bar_here());
                }
                draw_clip(ui, &app.project, &c, r, muted, bar_w, is_sel);
                let tip = "Drag: move (Ctrl: copy) · drag an edge: resize · double-click: edit · Ctrl+click: select more · right-click: more";
                resp.on_hover_text(tip).context_menu(|ui| {
                    let n = app.sel_clips.len();
                    if ui.button(if n > 1 { format!("🗑 Delete {n} clips") } else { "🗑 Delete clip".into() }).clicked() {
                        app.project.remove_clips(&app.sel_clips);
                        app.sel_clips.clear();
                        ui.close();
                        return;
                    }
                    if ui.button(format!("✂ Cut out bar {}", app.sel_bar + 1)).clicked() {
                        app.project.toggle_bar(track, app.sel_bar, app.pattern);
                        app.sel_clips.clear();
                        ui.close();
                        return;
                    }
                    ui.separator();
                    ui.label(RichText::new(format!("Chord on bar {}", app.sel_bar + 1)).small().weak());
                    for (k, chord) in CHORDS.iter().enumerate() {
                        if ui.button(if k == 0 { "— (clear)" } else { chord.name }).clicked() {
                            app.stamp_chord(track, app.sel_bar, k);
                            ui.close();
                        }
                    }
                });
            }
            if app.sel_track == Some(track) {
                let cell = Rect::from_min_size(Pos2::new(x(app.sel_bar * bl), y), vec2(bar_w, TRACK_H)).shrink(1.0);
                ui.painter().rect_stroke(cell, 3, Stroke::new(1.0, ACCENT_HOT.gamma_multiply(0.6)), StrokeKind::Inside);
            }
        }

        // Chords dragged from the browser drop on a bar.
        if let Some(pos) = pointer.filter(|p| lanes.contains(*p) && ui.clip_rect().contains(*p))
            && egui::DragAndDrop::has_payload_of_type::<usize>(ui.ctx())
            && let Some(track) = app.project.tracks.get(row_at(pos.y) as usize).map(|t| t.id)
        {
            let bar = tick_at(pos.x) as Tick / bl;
            let row = row_at(pos.y).floor();
            let cell = Rect::from_min_size(Pos2::new(x(bar * bl), lanes.top() + row * TRACK_H), vec2(bar_w, TRACK_H));
            ui.painter().rect_stroke(cell.shrink(2.0), 3, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
            if ui.input(|i| i.pointer.any_released())
                && let Some(j) = egui::DragAndDrop::take_payload::<usize>(ui.ctx())
            {
                if app.project.clip_at(track, bar * bl).is_none() {
                    app.project.toggle_bar(track, bar, app.pattern);
                }
                app.stamp_chord(track, bar, *j);
                (app.sel_track, app.sel_bar) = (Some(track), bar);
            }
        }

        // Grid: bars, and beats when there is room.
        let p = ui.painter();
        for bar in 0..=bars {
            p.line_segment([Pos2::new(x(bar * bl), rect.top()), Pos2::new(x(bar * bl), rect.bottom())], Stroke::new(1.0, Color32::from_gray(70)));
            if bar_w >= 80.0 && bar < bars {
                for beat in 1..app.project.beats_per_bar {
                    let bx = x(bar * bl + beat * PPQ);
                    p.line_segment([Pos2::new(bx, lanes.top()), Pos2::new(bx, rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(10)));
                }
            }
        }
        if let Some((a, b)) = loop_range {
            for t in [a, b] {
                p.line_segment([Pos2::new(x(t), rect.top()), Pos2::new(x(t), rect.bottom())], Stroke::new(1.0, ACCENT.gamma_multiply(0.7)));
            }
        }
        if let Some(c) = app.cur {
            let px = x(c);
            p.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, ACCENT_HOT));
            p.add(egui::Shape::convex_polygon(vec![Pos2::new(px - 5.0, rect.top()), Pos2::new(px + 5.0, rect.top()), Pos2::new(px, rect.top() + 7.0)], ACCENT_HOT, Stroke::NONE));
        }

        // Drop the drag when the button comes up; scroll when it nears the edge.
        if let (Some(_), Some(pos)) = (&app.clip_drag, pointer) {
            if !ui.input(|i| i.pointer.any_down()) {
                let drag = app.clip_drag.take().expect("checked above");
                let sel = drop_drag(&mut app.project, &drag, tick_at(pos.x), row_at(pos.y), unit, command);
                if !matches!(drag, ClipDrag::Loop { .. }) {
                    app.sel_clips = sel;
                }
            } else {
                let vis = ui.clip_rect();
                let push = if pos.x > vis.right() - 24.0 { -12.0 } else if pos.x < vis.left() + LABEL_W + 24.0 && vis.left() > rect.left() { 12.0 } else { 0.0 };
                if push != 0.0 {
                    ui.scroll_with_delta(vec2(push, 0.0));
                    ui.ctx().request_repaint();
                }
            }
        }
    });
    if let Some(id) = delete {
        app.project.remove_track(id);
        app.fix_selection();
    }
}

/// Double-click on a clip: patterns with melodic notes open in the piano roll, drum patterns in
/// the Channel Rack.
fn edit_pattern(app: &mut App, pattern: PatternId) {
    let project = &app.project;
    let melodic = project.pattern(pattern).and_then(|p| p.notes.iter().map(|n| n.channel).find(|&c| project.channel(c).is_some_and(|c| !c.instrument.is_drum())));
    app.pattern = pattern;
    match melodic {
        Some(ch) => app.edit_in_roll(pattern, ch),
        None => app.open[Win::Rack as usize] = true,
    }
}

/// A clip: the pattern's name, a sketch of its notes (with a notch where the pattern repeats),
/// and chord names on chord bars.
fn draw_clip(ui: &Ui, project: &Project, c: &Clip, r: Rect, muted: bool, bar_w: f32, selected: bool) {
    let Some((i, pattern)) = project.patterns.iter().enumerate().find(|(_, p)| p.id == c.pattern) else { return };
    let p = ui.painter_at(r);
    let dim = if muted { 0.3 } else { 0.8 };
    let bl = project.bar_len();
    let outline = if selected { Stroke::new(2.0, Color32::WHITE) } else { Stroke::new(1.0, CLIP_TEXT.gamma_multiply(0.6)) };
    let chords: Vec<(Tick, usize)> = (0..c.len.div_ceil(bl)).map(|b| (b, project.chord_at(c.pattern, project.clip_pos(c, b * bl)))).filter(|&(_, j)| j > 0).collect();
    if !chords.is_empty() {
        // Chord clips: one colored card per chord, named.
        p.rect_filled(r, 3, pattern_color(i).gamma_multiply(dim * 0.5));
        for (b, j) in chords {
            let card = Rect::from_min_max(Pos2::new(r.left() + b as f32 * bar_w, r.top()), Pos2::new((r.left() + (b + 1) as f32 * bar_w - 4.0).min(r.right()), r.bottom()));
            p.rect_filled(card, 3, chord_color(j).gamma_multiply(dim / 0.8));
            p.text(card.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, CHORDS[j].name, FontId::proportional(14.0), CLIP_TEXT);
        }
        p.rect_stroke(r, 3, outline, StrokeKind::Inside);
        return;
    }
    p.rect_filled(r, 3, pattern_color(i).gamma_multiply(dim));
    p.text(r.left_top() + vec2(5.0, 3.0), Align2::LEFT_TOP, &pattern.name, FontId::proportional(11.0), CLIP_TEXT);
    let tx = |t: Tick| r.left() + t as f32 / c.len as f32 * r.width();
    // Where the pattern starts over.
    if pattern.len > 0 {
        let first = (pattern.len - c.offset % pattern.len) % pattern.len;
        for t in (first.max(pattern.len)..c.len).step_by(pattern.len as usize).chain((first > 0).then_some(first)) {
            p.line_segment([Pos2::new(tx(t), r.top() + 3.0), Pos2::new(tx(t), r.top() + 9.0)], Stroke::new(1.5, CLIP_TEXT.gamma_multiply(0.7)));
        }
    }

    let notes: Vec<(Tick, &Note)> = project.clip_notes(*c).collect();
    // Drum patterns put every note on one pitch: sketch them by channel instead.
    let one_pitch = notes.windows(2).all(|w| w[0].1.pitch == w[1].1.pitch);
    let key = |n: &Note| if one_pitch { project.channel_index(n.channel).unwrap_or(0) as i32 } else { n.pitch as i32 };
    let (lo, hi) = notes.iter().fold((i32::MAX, i32::MIN), |(lo, hi), (_, n)| (lo.min(key(n)), hi.max(key(n))));
    let span = (hi - lo).max(1) as f32;
    for (t, n) in notes {
        let y = r.bottom() - 6.0 - (key(n) - lo) as f32 / span * (r.height() - 22.0);
        if one_pitch {
            p.circle_filled(Pos2::new(tx(t) + 2.0, y), 1.8, CLIP_TEXT);
        } else {
            let end = (t + n.len).min(c.len);
            p.rect_filled(Rect::from_min_max(Pos2::new(tx(t), y - 1.0), Pos2::new(tx(end).max(tx(t) + 2.0), y + 1.0)), 0, CLIP_TEXT);
        }
    }
    p.rect_stroke(r, 3, outline, StrokeKind::Inside);
}

// ---------------------------------------------------------------------------------------------
// Mixer

fn mixer(app: &mut App, ui: &mut Ui) {
    egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (m, track) in app.project.mixer.iter_mut().enumerate().take(MAX_MIXER) {
                strip(ui, &track.name, channel_color(m * 2), &mut track.vol, app.meters[m]);
            }
            ui.separator();
            strip(ui, "Master", ACCENT, &mut app.project.master.volume, app.meters[MASTER]);
            ui.separator();
            egui::Frame::new().fill(BG_PANEL).corner_radius(3).inner_margin(8).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Master insert · Lo-fi").strong());
                    ui.add_space(6.0);
                    let fx = &mut app.project.master;
                    egui::Grid::new("fx").spacing(vec2(14.0, 10.0)).show(ui, |ui| {
                        for (i, (v, name, default)) in [
                            (&mut fx.tone, "Tone", 0.55),
                            (&mut fx.crush, "Crush", 0.2),
                            (&mut fx.wobble, "Wobble", 0.35),
                            (&mut fx.vinyl, "Vinyl", 0.4),
                            (&mut fx.reverb, "Reverb", 0.3),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            ui.vertical_centered(|ui| {
                                knob(ui, v, 0.0..=1.0, default, name, ACCENT);
                                ui.label(RichText::new(name).small());
                            });
                            if i % 3 == 2 {
                                ui.end_row();
                            }
                        }
                    });
                });
            });
        });
    });
}

fn strip(ui: &mut Ui, name: &str, color: Color32, fader: &mut f32, level: f32) {
    egui::Frame::new().fill(BG_PANEL).corner_radius(3).inner_margin(6).show(ui, |ui| {
        ui.set_width(60.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(name).color(color).strong());
            ui.horizontal(|ui| {
                meter(ui, level, 150.0);
                ui.spacing_mut().slider_width = 150.0;
                ui.add(egui::Slider::new(fader, 0.0..=1.0).vertical().show_value(false)).on_hover_text(format!("{name} level"));
            });
            let db = 20.0 * fader.log10();
            ui.label(RichText::new(if db.is_finite() { format!("{db:+.1} dB") } else { "-inf".into() }).small().monospace());
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Sampler (chop)

/// Typing-keyboard key for pad `k` (pitch `CHOP_BASE + k`).
const PAD_KEYS: [&str; 16] = ["Z", "S", "X", "D", "C", "V", "G", "B", "H", "N", "J", "M", "Q", "2", "W", "3"];
const WAVE_H: f32 = 150.0;
const PAD: f32 = 52.0;

fn sampler(app: &mut App, ui: &mut Ui) {
    let chops: Vec<(ChannelId, String)> = app.project.channels.iter().filter(|c| c.chop.is_some()).map(|c| (c.id, c.name.clone())).collect();
    let current = app.chop_ch.filter(|id| chops.iter().any(|(c, _)| c == id));
    ui.horizontal(|ui| {
        if ui.button("📂 Load sample…").on_hover_text("Load WAV, FLAC, MP3, OGG or AIFF as a new chop channel (or drop files on the window)").clicked() {
            app.load_sample_dialog();
        }
        if chops.len() > 1 {
            let name = current.and_then(|id| chops.iter().find(|(c, _)| *c == id)).map_or("—".to_string(), |(_, n)| n.clone());
            egui::ComboBox::from_id_salt("chop-channel").selected_text(name).show_ui(ui, |ui| {
                for (id, n) in &chops {
                    if ui.selectable_label(current == Some(*id), n).clicked() {
                        app.select_chop(*id);
                    }
                }
            });
        }
    });
    let Some(ch) = current else {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Drop an audio file here, or load one, to chop it onto 16 pads.").size(15.0));
            ui.label(RichText::new("It is chopped at its hits automatically; then play the pads with Z S X D C V G B H N J M Q 2 W 3.").weak());
        });
        return;
    };
    let Some(chop) = app.project.chop(ch).cloned() else { return };
    let index = app.project.channel_index(ch).map_or(u16::MAX, |i| i as u16);
    let loaded = app.chop_sample(ch).map(|l| (l.data.sr, l.data.frames.len(), l.overview.clone()));
    let name = chop.path.file_name().map_or_else(|| chop.path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let Some((sr, frames, overview)) = loaded else {
        let why = match app.samples.get(&chop.path) {
            Some(Err(e)) if e != "loading…" => format!("Can't play {name}: {e}"),
            _ => format!("Loading {name}…"),
        };
        ui.label(RichText::new(why).color(ACCENT_HOT));
        return;
    };

    // Chop controls.
    ui.horizontal(|ui| {
        ui.label(RichText::new(&name).strong()).on_hover_text(chop.path.display().to_string());
        ui.label(RichText::new(format!("{:.1} s · {} pads", frames as f32 / sr as f32, chop.slices.len())).weak());
        ui.separator();
        let tapping = app.tap.is_some();
        ui.add_enabled_ui(!tapping, |ui| {
            if ui.button("Chop at hits").on_hover_text("Find the hits and put a chop at each (up to 16)").clicked()
                && let Some(l) = app.chop_sample(ch)
            {
                let points = sample::detect_hits(&l.data, app.sensitivity, MAX_SLICES);
                app.project.set_chop_points(ch, &points);
            }
            ui.add(egui::Slider::new(&mut app.sensitivity, 0.0..=1.0).show_value(false).text("sensitivity")).on_hover_text("Higher finds softer hits");
            for n in [8, 16] {
                if ui.button(format!("{n} slices")).on_hover_text(format!("Cut into {n} equal slices")).clicked() {
                    app.project.set_chop_points(ch, &sample::even_points(chop.len, n));
                }
            }
        });
        let tap_label = if tapping { "● Tap (Enter)" } else { "Tap to chop" };
        let tap = ui.add(egui::Button::selectable(tapping, tap_label)).on_hover_text("Plays the sample; tap Enter (or this button) where each chop starts. Esc ends.");
        if tap.clicked() {
            if tapping { app.tap() } else { app.start_tap(ch) }
        }
        if tapping && ui.button("Done").on_hover_text("Stop and chop at the taps (Esc)").clicked() {
            app.finish_tap();
        }
    });

    // Waveform with slices and chop points.
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, WAVE_H), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3, BG_DEEP);
    let x_of = |f: u32| rect.left() + f as f32 / frames.max(1) as f32 * rect.width();
    let frame_at = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * frames as f32) as u32;
    for (k, s) in chop.slices.iter().enumerate() {
        let r = Rect::from_min_max(Pos2::new(x_of(s.start), rect.top()), Pos2::new(x_of(s.end), rect.bottom()));
        let base = channel_color(k);
        let fill = if k == app.chop_sel { base.gamma_multiply(0.35) } else { base.gamma_multiply(if k % 2 == 0 { 0.12 } else { 0.07 }) };
        p.rect_filled(r, 0, fill);
        p.text(r.left_top() + vec2(4.0, 3.0), Align2::LEFT_TOP, format!("{}", k + 1), FontId::proportional(12.0), base);
        if s.reverse || s.pitch != 0.0 {
            let tag = format!("{}{}", if s.reverse { "◀ " } else { "" }, if s.pitch != 0.0 { format!("{:+.0}", s.pitch) } else { String::new() });
            p.text(r.left_bottom() + vec2(4.0, -3.0), Align2::LEFT_BOTTOM, tag, FontId::proportional(10.0), base);
        }
    }
    let mid = rect.center().y;
    let cols = rect.width() as usize;
    for c in 0..cols {
        let (a, b) = (c * overview.len() / cols, ((c + 1) * overview.len() / cols).max(c * overview.len() / cols + 1));
        let [lo, hi] = overview[a.min(overview.len() - 1)..b.min(overview.len())].iter().fold([0.0f32, 0.0f32], |[lo, hi], [l, h]| [lo.min(*l), hi.max(*h)]);
        let x = rect.left() + c as f32 + 0.5;
        p.line_segment([Pos2::new(x, mid - hi * WAVE_H * 0.48), Pos2::new(x, mid - lo * WAVE_H * 0.48)], Stroke::new(1.0, Color32::from_gray(185)));
    }
    let points: Vec<u32> = chop.slices.iter().map(|s| s.start).collect();
    for &pt in &points {
        let x = x_of(pt);
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, ACCENT));
        p.add(egui::Shape::convex_polygon(vec![Pos2::new(x - 5.0, rect.top()), Pos2::new(x + 5.0, rect.top()), Pos2::new(x, rect.top() + 8.0)], ACCENT, Stroke::NONE));
    }
    if let Some(t) = &app.tap {
        let x = x_of((t.started.elapsed().as_secs_f64() * t.sr as f64) as u32);
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, ACCENT_HOT));
        for &f in &t.points {
            p.line_segment([Pos2::new(x_of(f), rect.top()), Pos2::new(x_of(f), rect.bottom())], Stroke::new(1.5, ACCENT_HOT));
        }
    }
    // Interaction: drag a chop point (grab within 6 px), click a slice to play and select it,
    // double-click to add a chop point, right-click a point to remove it.
    let near = |x: f32| points.iter().enumerate().map(|(k, &pt)| (k, (x_of(pt) - x).abs())).filter(|&(_, d)| d <= 6.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(k, _)| k);
    if app.tap.is_none() {
        if resp.drag_started() {
            app.chop_drag = resp.interact_pointer_pos().and_then(|p| near(p.x));
        }
        if let (Some(k), Some(pos)) = (app.chop_drag, resp.interact_pointer_pos()) {
            app.project.move_chop_point(ch, k, frame_at(pos.x));
        }
        if resp.drag_stopped() {
            app.chop_drag = None;
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.double_clicked() {
                app.project.add_chop_point(ch, frame_at(pos.x));
            } else if resp.clicked() {
                let f = frame_at(pos.x);
                if let Some(k) = chop.slices.iter().position(|s| (s.start..s.end).contains(&f)) {
                    app.chop_sel = k;
                    app.live = ch;
                    app.send(Cmd::NoteOn { channel: index, pitch: CHOP_BASE + k as u8 });
                    app.after_send.push(Cmd::NoteOff { channel: index, pitch: CHOP_BASE + k as u8 });
                }
            } else if resp.secondary_clicked()
                && let Some(k) = near(pos.x)
            {
                app.project.remove_chop_point(ch, k);
            }
        }
    }
    resp.on_hover_text("Click a slice to hear it · drag a chop point · double-click to add one · right-click a point to remove it");

    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        // Pads, 4 x 4 with pad 1 at the bottom left.
        let (grid, _) = ui.allocate_exact_size(vec2(4.0 * PAD + 12.0, 4.0 * PAD + 12.0), Sense::hover());
        for (k, key) in PAD_KEYS.iter().enumerate() {
            let (row, col) = (3 - k / 4, k % 4);
            let r = Rect::from_min_size(grid.min + vec2(col as f32 * (PAD + 4.0), row as f32 * (PAD + 4.0)), vec2(PAD, PAD));
            let exists = k < chop.slices.len();
            let resp = ui.interact(r, ui.scope_id().with(("pad", k)), if exists { Sense::click() } else { Sense::hover() });
            let pitch = CHOP_BASE + k as u8;
            if resp.is_pointer_button_down_on() && exists && app.preview != Some(pitch) {
                app.send(Cmd::NoteOn { channel: index, pitch });
                (app.preview, app.chop_sel, app.live) = (Some(pitch), k, ch);
            } else if resp.clicked() && app.preview != Some(pitch) {
                // Pressed and released within one frame (a quick tap): play it once.
                app.send(Cmd::NoteOn { channel: index, pitch });
                app.after_send.push(Cmd::NoteOff { channel: index, pitch });
                (app.chop_sel, app.live) = (k, ch);
            }
            if !resp.is_pointer_button_down_on() && app.preview == Some(pitch) {
                app.send(Cmd::NoteOff { channel: index, pitch });
                app.preview = None;
            }
            let base = channel_color(k);
            let fill = match (exists, app.preview == Some(pitch), k == app.chop_sel) {
                (false, _, _) => BG_DEEP,
                (true, true, _) => base,
                (true, false, true) => base.gamma_multiply(0.6),
                _ => base.gamma_multiply(0.3),
            };
            let p = ui.painter();
            p.rect_filled(r, 5, fill);
            if k == app.chop_sel && exists {
                p.rect_stroke(r, 5, Stroke::new(2.0, ACCENT_HOT), StrokeKind::Inside);
            }
            let text = if exists { Color32::from_gray(235) } else { Color32::from_gray(80) };
            p.text(r.left_top() + vec2(6.0, 5.0), Align2::LEFT_TOP, format!("{}", k + 1), FontId::proportional(15.0), text);
            p.text(r.right_bottom() - vec2(6.0, 5.0), Align2::RIGHT_BOTTOM, *key, FontId::monospace(11.0), text.gamma_multiply(0.7));
            let label = format!("Pad {} (key {key})", k + 1);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, exists, &label));
            if exists {
                resp.on_hover_text(format!("{label}: click to play"));
            }
        }

        ui.add_space(10.0);
        ui.vertical(|ui| {
            let k = app.chop_sel.min(chop.slices.len().saturating_sub(1));
            ui.label(RichText::new(format!("Pad {} · key {}", k + 1, PAD_KEYS[k])).strong());
            let s = chop.slices[k];
            ui.label(RichText::new(format!("{:.3} – {:.3} s", s.start as f32 / sr as f32, s.end as f32 / sr as f32)).weak().monospace());
            ui.add_space(4.0);
            if let Some(slice) = app.project.slice_mut(ch, k) {
                egui::Grid::new("pad-knobs").spacing(vec2(16.0, 2.0)).show(ui, |ui| {
                    knob(ui, &mut slice.pitch, -24.0..=24.0, 0.0, "Pitch (semitones)", ACCENT);
                    knob(ui, &mut slice.gain, 0.0..=2.0, 1.0, "Level", ACCENT);
                    ui.end_row();
                    ui.label(RichText::new(format!("{:+.0} st", slice.pitch)).small());
                    ui.label(RichText::new("Level").small());
                });
                slice.pitch = slice.pitch.round();
                ui.checkbox(&mut slice.reverse, "Reverse").on_hover_text("Play this pad backwards");
            }
            ui.separator();
            if let Some(c) = app.project.channels.iter_mut().find(|c| c.id == ch).and_then(|c| c.chop.as_mut()) {
                ui.checkbox(&mut c.one_shot, "One-shot").on_hover_text("Play the whole slice however short the note is");
                ui.checkbox(&mut c.cut_itself, "Cut itself").on_hover_text("A new pad hit stops the chop that is still playing");
            }
            ui.add_space(4.0);
            if ui.button("Edit pattern in piano roll").on_hover_text("Each row is a pad").clicked() {
                app.edit_channel(ch);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(note_name(38), "D2");
        assert!(is_black(61) && !is_black(64));
    }
}
