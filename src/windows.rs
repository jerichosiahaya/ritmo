//! The four floating windows: Playlist, Channel Rack, Piano Roll and Mixer. They edit
//! `app.project` through its editing operations and draw previews from `app.snap`.

use crate::engine::*;
use crate::project::{CHOP_BASE, CHORDS, ChannelId, Clip, MAX_SLICES, Note, PatternId, Project};
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
        (Win::Playlist, "Playlist", o + vec2(16.0, 16.0), [700.0, 190.0]),
        (Win::Rack, "Channel Rack", o + vec2(16.0, 250.0), [560.0, 250.0]),
        (Win::Roll, &roll_title, o + vec2(80.0, 60.0), [760.0, 420.0]),
        (Win::Mixer, "Mixer", o + vec2(600.0, 250.0), [620.0, 270.0]),
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

fn piano_roll(app: &mut App, ui: &mut Ui) {
    let (pat, ch) = app.roll;
    let (Some(channel), Some(pattern)) = (app.project.channel(ch), app.project.pattern(pat)) else {
        ui.label("Pick a channel in the Channel Rack or the browser to edit its notes.");
        return;
    };
    let (ch_name, color) = (channel.name.clone(), channel_color(app.project.channel_index(ch).unwrap_or(0)));
    let is_chop = channel.instrument == Instrument::Chop;
    let ch_index = app.project.channel_index(ch).map_or(u16::MAX, |i| i as u16);
    let steps = ((pattern.len / STEP) as usize).max(STEPS_PER_BAR);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&ch_name).color(color).strong());
        if ui.button("Clear").on_hover_text(format!("Remove all of {ch_name}'s notes from this pattern")).clicked()
            && let Some(p) = app.project.pattern_mut(pat)
        {
            p.notes.retain(|n| n.channel != ch);
        }
        let top = 127 - ROLL_KEYS as u8 + 1;
        if ui.add_enabled(app.roll_low < top, egui::Button::new("▲")).on_hover_text("Octave up").clicked() {
            app.roll_low = (app.roll_low + 12).min(top);
        }
        if ui.add_enabled(app.roll_low > 0, egui::Button::new("▼")).on_hover_text("Octave down").clicked() {
            app.roll_low = app.roll_low.saturating_sub(12);
        }
        ui.label(RichText::new("Left-drag draws · right-drag erases · click a key to hear it").small().weak());
    });
    let low = app.roll_low;
    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        let size = vec2(KEY_W + CELL_W * steps as f32, RULER_H + ROW_H * ROLL_KEYS as f32);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let grid = Rect::from_min_max(rect.min + vec2(KEY_W, RULER_H), rect.max);
        let pitch_at = |y: f32| {
            let row = ((y - grid.top()) / ROW_H).floor();
            (0.0..ROLL_KEYS as f32).contains(&row).then(|| low + (ROLL_KEYS - 1) as u8 - row as u8)
        };

        // Interaction: draw/erase cells, or preview from the keyboard.
        let pointer = resp.is_pointer_button_down_on().then(|| resp.interact_pointer_pos()).flatten();
        match pointer.and_then(|p| pitch_at(p.y).map(|n| (p, n))) {
            Some((p, pitch)) if p.x < grid.left() => {
                if app.preview != Some(pitch) {
                    if let Some(old) = app.preview {
                        app.send(Cmd::NoteOff { channel: ch_index, pitch: old });
                    }
                    app.send(Cmd::NoteOn { channel: ch_index, pitch });
                    app.preview = Some(pitch);
                }
            }
            Some((p, pitch)) => {
                let step = (((p.x - grid.left()) / CELL_W) as usize).min(steps - 1) as Tick;
                let erase = ui.input(|i| i.pointer.secondary_down());
                let draw = *app.roll_paint.get_or_insert(!erase && !app.project.cell(pat, ch, pitch, step));
                app.project.set_cell(pat, ch, pitch, step, draw);
            }
            None => {}
        }
        if pointer.is_none() {
            app.roll_paint = None;
            if let Some(n) = app.preview.take() {
                app.send(Cmd::NoteOff { channel: ch_index, pitch: n });
            }
        }

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0, BG_DEEP);
        // Ruler.
        for s in (0..steps).step_by(4) {
            let x = grid.left() + s as f32 * CELL_W;
            if s % STEPS_PER_BAR == 0 {
                p.text(Pos2::new(x + 3.0, rect.top() + RULER_H / 2.0), Align2::LEFT_CENTER, format!("{}", s / STEPS_PER_BAR + 1), FontId::proportional(12.0), Color32::from_gray(200));
            } else {
                p.line_segment([Pos2::new(x, rect.top() + RULER_H - 5.0), Pos2::new(x, rect.top() + RULER_H)], Stroke::new(1.0, Color32::from_gray(110)));
            }
        }
        // Rows and keyboard.
        for k in 0..ROLL_KEYS {
            let pitch = low + (ROLL_KEYS - 1 - k) as u8;
            // Chop pads aren't notes of a scale: every row is a plain row.
            let black = !is_chop && is_black(pitch);
            let y = grid.top() + k as f32 * ROW_H;
            let row = Rect::from_min_size(Pos2::new(grid.left(), y), vec2(grid.width(), ROW_H));
            p.rect_filled(row, 0, if black { Color32::from_rgb(36, 40, 45) } else { Color32::from_rgb(47, 52, 58) });
            // Black keys are shorter, drawn over the white key area like a real keyboard.
            let key_w = if black { (KEY_W - 2.0) * 0.62 } else { KEY_W - 2.0 };
            let key = Rect::from_min_size(Pos2::new(rect.left(), y), vec2(key_w, ROW_H));
            if black {
                p.rect_filled(Rect::from_min_size(Pos2::new(rect.left(), y), vec2(KEY_W - 2.0, ROW_H)), 0, Color32::from_rgb(222, 222, 216));
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
        // Grid lines.
        for s in 0..=steps {
            let x = grid.left() + s as f32 * CELL_W;
            let (w, c) = match s {
                s if s % STEPS_PER_BAR == 0 => (1.5, Color32::from_gray(120)),
                s if s % 4 == 0 => (1.0, Color32::from_gray(78)),
                _ => (1.0, Color32::from_gray(56)),
            };
            p.line_segment([Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())], Stroke::new(w, c));
        }
        // Notes; dimmed where no playlist clip plays that part of the pattern.
        let project = &app.project;
        let audible = |t: Tick| project.clips.iter().any(|c| c.pattern == pat && (c.offset..c.offset + c.len).contains(&t));
        let px = |t: Tick| grid.left() + t as f32 / STEP as f32 * CELL_W;
        let visible = low..low.saturating_add(ROLL_KEYS as u8);
        for n in project.pattern(pat).into_iter().flat_map(|p| &p.notes).filter(|n| n.channel == ch && visible.contains(&n.pitch)) {
            let y = grid.top() + (low + ROLL_KEYS as u8 - 1 - n.pitch) as f32 * ROW_H;
            let r = Rect::from_min_max(Pos2::new(px(n.start), y), Pos2::new(px(n.end()), y + ROW_H)).shrink(1.0);
            let col = if audible(n.start) { color } else { color.gamma_multiply(0.35) };
            p.rect_filled(r, 3, col);
            p.rect_stroke(r, 3, Stroke::new(1.0, col.gamma_multiply(0.5)), StrokeKind::Inside);
            if r.width() > 26.0 {
                p.text(r.left_center() + vec2(3.0, 0.0), Align2::LEFT_CENTER, note_name(n.pitch), FontId::proportional(9.0), CLIP_TEXT);
            }
        }
        if let Some(t) = app.cur.and_then(|t| project.pattern_pos(pat, t)) {
            p.line_segment([Pos2::new(px(t), rect.top()), Pos2::new(px(t), grid.bottom())], Stroke::new(2.0, ACCENT_HOT));
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Playlist

const LABEL_W: f32 = 96.0;
const BAR_W: f32 = 140.0;
const TRACK_H: f32 = 40.0;

fn playlist(app: &mut App, ui: &mut Ui) {
    let bl = app.project.bar_len();
    // The song's bars plus one empty bar to extend into.
    let bars = (app.project.end().div_ceil(bl) + 1).max(4);
    let rows = app.project.tracks.len();
    if ui.small_button("＋ Track").on_hover_text("Add an empty playlist track").clicked() {
        app.project.new_track();
    }
    let mut delete = None;
    egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
        let size = vec2(LABEL_W + BAR_W * bars as f32, RULER_H + TRACK_H * rows as f32);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let gx = rect.left() + LABEL_W;
        let x = |t: Tick| gx + t as f32 / bl as f32 * BAR_W;
        ui.painter().rect_filled(rect, 0, BG_DEEP);

        // Ruler: click a bar number to select that bar.
        for bar in 0..bars {
            let r = Rect::from_min_size(Pos2::new(gx + bar as f32 * BAR_W, rect.top()), vec2(BAR_W, RULER_H));
            if ui.interact(r, ui.scope_id().with(("ruler", bar)), Sense::click()).on_hover_text("Select bar").clicked() {
                app.sel_bar = bar;
            }
            let col = if app.sel_bar == bar { ACCENT_HOT } else { Color32::from_gray(190) };
            ui.painter().text(r.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, format!("{}", bar + 1), FontId::proportional(12.0), col);
        }

        for row in 0..rows {
            let t = &app.project.tracks[row];
            let (track, name, muted) = (t.id, t.name.clone(), t.mute);
            let y = rect.top() + RULER_H + row as f32 * TRACK_H;
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
            let lane = Rect::from_min_size(Pos2::new(gx, y), vec2(BAR_W * bars as f32, TRACK_H));
            p.rect_filled(lane, 0, if row % 2 == 0 { Color32::from_rgb(40, 44, 50) } else { Color32::from_rgb(36, 40, 45) });

            for c in app.project.clips.iter().filter(|c| c.track == track) {
                let r = Rect::from_min_max(Pos2::new(x(c.start), y), Pos2::new(x(c.end()), y + TRACK_H)).shrink(2.0);
                draw_clip(ui, &app.project, c, r, muted);
            }

            // Bar cells: click places the track's pattern or selects the bar; double-click edits;
            // right-click stamps chords or cuts the bar; chords from the browser drop here.
            for bar in 0..bars {
                let r = Rect::from_min_size(Pos2::new(gx + bar as f32 * BAR_W, y), vec2(BAR_W, TRACK_H)).shrink(2.0);
                let resp = ui.interact(r, ui.scope_id().with(("cell", row, bar)), Sense::click());
                let clip = app.project.clip_at(track, bar * bl).copied();
                if resp.double_clicked() {
                    if let Some(c) = clip {
                        edit_pattern(app, c.pattern);
                    }
                } else if resp.clicked() {
                    if clip.is_none() {
                        app.project.toggle_bar(track, bar, app.pattern);
                    }
                    (app.sel_track, app.sel_bar) = (Some(track), bar);
                }
                if let Some(j) = resp.dnd_release_payload::<usize>() {
                    if clip.is_none() {
                        app.project.toggle_bar(track, bar, app.pattern);
                    }
                    app.stamp_chord(track, bar, *j);
                    (app.sel_track, app.sel_bar) = (Some(track), bar);
                }
                let p = ui.painter();
                if app.sel_track == Some(track) && app.sel_bar == bar {
                    p.rect_stroke(r, 3, Stroke::new(2.0, ACCENT_HOT), StrokeKind::Outside);
                }
                if resp.dnd_hover_payload::<usize>().is_some() {
                    p.rect_stroke(r, 3, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
                }
                let hint = if clip.is_some() { "Click: select · double-click: edit · right-click: chord or cut" } else { "Click: place this track's pattern here" };
                resp.on_hover_text(hint).context_menu(|ui| {
                    if clip.is_none() {
                        if ui.button("Place pattern").clicked() {
                            app.project.toggle_bar(track, bar, app.pattern);
                            ui.close();
                        }
                        return;
                    }
                    if ui.button("✂ Cut this bar").clicked() {
                        app.project.toggle_bar(track, bar, app.pattern);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(RichText::new("Put a chord here").small().weak());
                    for (k, chord) in CHORDS.iter().enumerate() {
                        if ui.button(if k == 0 { "— (clear)" } else { chord.name }).clicked() {
                            app.stamp_chord(track, bar, k);
                            ui.close();
                        }
                    }
                });
            }
        }
        for bar in 0..=bars {
            let px = gx + bar as f32 * BAR_W;
            ui.painter().line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.0, Color32::from_gray(70)));
        }

        if let Some(c) = app.cur {
            let px = x(c);
            ui.painter().line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, ACCENT_HOT));
            ui.painter().add(egui::Shape::convex_polygon(
                vec![Pos2::new(px - 5.0, rect.top()), Pos2::new(px + 5.0, rect.top()), Pos2::new(px, rect.top() + 7.0)],
                ACCENT_HOT,
                Stroke::NONE,
            ));
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
    match melodic {
        Some(ch) => app.edit_in_roll(pattern, ch),
        None => {
            app.pattern = pattern;
            app.open[Win::Rack as usize] = true;
        }
    }
}

/// A clip: the pattern's name, a sketch of its notes, and chord names on chord bars.
fn draw_clip(ui: &Ui, project: &Project, c: &Clip, r: Rect, muted: bool) {
    let Some((i, pattern)) = project.patterns.iter().enumerate().find(|(_, p)| p.id == c.pattern) else { return };
    let p = ui.painter_at(r);
    let dim = if muted { 0.3 } else { 0.8 };
    let bl = project.bar_len();
    let chords: Vec<(Tick, usize)> = (0..c.len.div_ceil(bl)).map(|b| (b, project.chord_at(c.pattern, c.offset + b * bl))).filter(|&(_, j)| j > 0).collect();
    if !chords.is_empty() {
        // Chord clips: one colored card per chord, named.
        p.rect_filled(r, 3, pattern_color(i).gamma_multiply(dim * 0.5));
        for (b, j) in chords {
            let card = Rect::from_min_max(Pos2::new(r.left() + b as f32 * BAR_W, r.top()), Pos2::new((r.left() + (b + 1) as f32 * BAR_W - 4.0).min(r.right()), r.bottom()));
            p.rect_filled(card, 3, chord_color(j).gamma_multiply(dim / 0.8));
            p.text(card.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, CHORDS[j].name, FontId::proportional(14.0), CLIP_TEXT);
        }
        return;
    }
    p.rect_filled(r, 3, pattern_color(i).gamma_multiply(dim));
    p.rect_stroke(r, 3, Stroke::new(1.0, CLIP_TEXT.gamma_multiply(0.6)), StrokeKind::Inside);
    p.text(r.left_top() + vec2(5.0, 3.0), Align2::LEFT_TOP, &pattern.name, FontId::proportional(11.0), CLIP_TEXT);

    let notes: Vec<&Note> = pattern.notes.iter().filter(|n| (c.offset..c.offset + c.len).contains(&n.start)).collect();
    // Drum patterns put every note on one pitch: sketch them by channel instead.
    let one_pitch = notes.windows(2).all(|w| w[0].pitch == w[1].pitch);
    let key = |n: &Note| if one_pitch { project.channel_index(n.channel).unwrap_or(0) as i32 } else { n.pitch as i32 };
    let (lo, hi) = notes.iter().fold((i32::MAX, i32::MIN), |(lo, hi), n| (lo.min(key(n)), hi.max(key(n))));
    let span = (hi - lo).max(1) as f32;
    let tx = |t: Tick| r.left() + 5.0 + (t.min(c.offset + c.len) - c.offset) as f32 / c.len as f32 * (r.width() - 10.0);
    for n in notes {
        let y = r.bottom() - 6.0 - (key(n) - lo) as f32 / span * (r.height() - 22.0);
        if one_pitch {
            p.circle_filled(Pos2::new(tx(n.start) + 2.0, y), 1.8, CLIP_TEXT);
        } else {
            p.rect_filled(Rect::from_min_max(Pos2::new(tx(n.start), y - 1.0), Pos2::new(tx(n.end()).max(tx(n.start) + 2.0), y + 1.0)), 0, CLIP_TEXT);
        }
    }
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
