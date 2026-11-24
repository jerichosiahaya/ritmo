//! The four floating windows: Playlist, Channel Rack, Piano Roll and Mixer.

use crate::engine::*;
use crate::widgets::*;
use crate::{chord_color, App, Win, LIVE_CHANNEL};
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

const CLIP_TEXT: Color32 = Color32::from_rgb(22, 24, 28);

pub fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 1)
}

fn is_black(n: u8) -> bool {
    matches!(n % 12, 1 | 3 | 6 | 8 | 10)
}

pub fn show(app: &mut App, ctx: &egui::Context, area: Rect) {
    let o = area.min;
    let specs: [(Win, &str, Pos2, [f32; 2]); 4] = [
        (Win::Playlist, "Playlist", o + vec2(16.0, 16.0), [700.0, 190.0]),
        (Win::Rack, "Channel Rack", o + vec2(16.0, 250.0), [560.0, 250.0]),
        (Win::Roll, "Piano Roll — Lead", o + vec2(80.0, 60.0), [760.0, 420.0]),
        (Win::Mixer, "Mixer", o + vec2(600.0, 250.0), [620.0, 270.0]),
    ];
    for (w, title, pos, size) in specs {
        let mut open = app.is_open(w);
        egui::Window::new(title)
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
            });
        if !open {
            app.open[w as usize] = false;
        }
    }
}

fn rack(app: &mut App, ui: &mut Ui) {
    let cur = app.cur;
    for ch in 0..CHANNELS {
        let name = CHANNEL_NAMES[ch];
        let color = CH_COLORS[ch];
        ui.horizontal(|ui| {
            let c = &mut app.song.channels[ch];
            let mut on = !c.mute;
            if led(ui, &mut on, &format!("{name} on/off")).changed() {
                c.mute = !on;
            }
            knob(ui, &mut c.pan, -1.0..=1.0, 0.0, &format!("{name} pan"), color);
            knob(ui, &mut c.vol, 0.0..=1.0, 0.8, &format!("{name} volume"), color);
            let btn = egui::Button::new(RichText::new(name).color(CLIP_TEXT).strong()).fill(color.gamma_multiply(if c.mute { 0.4 } else { 0.9 })).min_size(vec2(84.0, 26.0));
            let hint = match ch {
                5 => "Open piano roll",
                3 | 4 => "Chords come from the Playlist",
                _ => "Synth drum",
            };
            if ui.add(btn).on_hover_text(hint).clicked() {
                match ch {
                    5 => app.open[Win::Roll as usize] = true,
                    3 | 4 => app.open[Win::Playlist as usize] = true,
                    _ => {}
                }
            }
            ui.add_space(6.0);
            if ch < TRACKS {
                ui.spacing_mut().item_spacing.x = 1.0;
                for s in 0..STEPS {
                    if s > 0 && s % 4 == 0 {
                        ui.add_space(4.0);
                    }
                    let on = app.song.drums[ch][s];
                    let playing = cur.is_some_and(|c| c % STEPS == s && app.song.drum_bars[c / STEPS]);
                    if step_button(ui, on, s / 4 % 2 == 0, playing, color, &format!("{name} step {}", s + 1)).clicked() {
                        app.song.drums[ch][s] = !on;
                    }
                }
            } else {
                strip_preview(ui, &app.song, ch, cur);
            }
        });
    }
}

/// Non-drum rows show what they will play instead of step buttons.
fn strip_preview(ui: &mut Ui, song: &Song, ch: usize, cur: Option<usize>) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0 * 21.0 + 12.0, 26.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3, BG_DEEP);
    let bw = rect.width() / BARS as f32;
    for bar in 0..BARS {
        let r = Rect::from_min_size(rect.min + vec2(bar as f32 * bw, 0.0), vec2(bw, rect.height())).shrink(2.0);
        if ch == 5 {
            if !song.lead_bars[bar] {
                continue;
            }
            let sw = r.width() / STEPS as f32;
            for (k, row) in song.melody.iter().enumerate() {
                for s in 0..STEPS {
                    if row >> (bar * STEPS + s) & 1 == 1 {
                        let y = r.bottom() - (k as f32 + 1.0) / LEAD_KEYS as f32 * r.height();
                        p.rect_filled(Rect::from_min_size(Pos2::new(r.left() + s as f32 * sw, y), vec2(sw, 2.0)), 0, CH_COLORS[5]);
                    }
                }
            }
        } else {
            let c = &CHORDS[song.chords[bar]];
            if c.notes.is_empty() {
                continue;
            }
            p.rect_filled(r, 2, CH_COLORS[ch].gamma_multiply(0.35));
            let text = if ch == 3 { c.name.to_string() } else { note_name(c.bass) };
            p.text(r.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(11.0), Color32::from_gray(220));
        }
    }
    if let Some(c) = cur {
        let x = rect.left() + c as f32 / TOTAL_STEPS as f32 * rect.width();
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, ACCENT_HOT));
    }
}

const KEY_W: f32 = 46.0;
const CELL_W: f32 = 16.0;
const ROW_H: f32 = 14.0;
const RULER_H: f32 = 18.0;

fn piano_roll(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Lead · FM Rhodes").color(CH_COLORS[5]).strong());
        if ui.button("Clear").clicked() {
            app.song.melody = [0; LEAD_KEYS];
        }
        ui.label(RichText::new("Left-drag draws · right-drag erases · click a key to hear it").small().weak());
    });
    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        let size = vec2(KEY_W + CELL_W * TOTAL_STEPS as f32, RULER_H + ROW_H * LEAD_KEYS as f32);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let grid = Rect::from_min_max(rect.min + vec2(KEY_W, RULER_H), rect.max);
        let pitch_at = |y: f32| {
            let row = ((y - grid.top()) / ROW_H).floor();
            (0.0..LEAD_KEYS as f32).contains(&row).then(|| LEAD_LOW + (LEAD_KEYS - 1) as u8 - row as u8)
        };

        // Interaction: draw/erase cells, or preview from the keyboard.
        let pointer = resp.is_pointer_button_down_on().then(|| resp.interact_pointer_pos()).flatten();
        match pointer.and_then(|p| pitch_at(p.y).map(|n| (p, n))) {
            Some((p, pitch)) if p.x < grid.left() => {
                if app.preview != Some(pitch) {
                    if let Some(old) = app.preview {
                        app.send(Cmd::NoteOff { channel: LIVE_CHANNEL, pitch: old });
                    }
                    app.send(Cmd::NoteOn { channel: LIVE_CHANNEL, pitch });
                    app.preview = Some(pitch);
                }
            }
            Some((p, pitch)) => {
                let step = (((p.x - grid.left()) / CELL_W) as usize).min(TOTAL_STEPS - 1);
                let erase = ui.input(|i| i.pointer.secondary_down());
                let draw = *app.roll_paint.get_or_insert(!erase && !app.song.note(pitch, step));
                app.song.set_note(pitch, step, draw);
            }
            None => {}
        }
        if pointer.is_none() {
            app.roll_paint = None;
            if let Some(n) = app.preview.take() {
                app.send(Cmd::NoteOff { channel: LIVE_CHANNEL, pitch: n });
            }
        }

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0, BG_DEEP);
        // Ruler.
        for s in (0..TOTAL_STEPS).step_by(4) {
            let x = grid.left() + s as f32 * CELL_W;
            if s % STEPS == 0 {
                p.text(Pos2::new(x + 3.0, rect.top() + RULER_H / 2.0), Align2::LEFT_CENTER, format!("{}", s / STEPS + 1), FontId::proportional(12.0), Color32::from_gray(200));
            } else {
                p.line_segment([Pos2::new(x, rect.top() + RULER_H - 5.0), Pos2::new(x, rect.top() + RULER_H)], Stroke::new(1.0, Color32::from_gray(110)));
            }
        }
        // Rows and keyboard.
        for k in 0..LEAD_KEYS {
            let pitch = LEAD_LOW + (LEAD_KEYS - 1 - k) as u8;
            let y = grid.top() + k as f32 * ROW_H;
            let row = Rect::from_min_size(Pos2::new(grid.left(), y), vec2(grid.width(), ROW_H));
            p.rect_filled(row, 0, if is_black(pitch) { Color32::from_rgb(36, 40, 45) } else { Color32::from_rgb(47, 52, 58) });
            // Black keys are shorter, drawn over the white key area like a real keyboard.
            let key_w = if is_black(pitch) { (KEY_W - 2.0) * 0.62 } else { KEY_W - 2.0 };
            let key = Rect::from_min_size(Pos2::new(rect.left(), y), vec2(key_w, ROW_H));
            if is_black(pitch) {
                p.rect_filled(Rect::from_min_size(Pos2::new(rect.left(), y), vec2(KEY_W - 2.0, ROW_H)), 0, Color32::from_rgb(222, 222, 216));
            }
            let pressed = app.preview == Some(pitch);
            let fill = match (is_black(pitch), pressed) {
                (_, true) => ACCENT,
                (true, _) => Color32::from_rgb(28, 28, 30),
                (false, _) => Color32::from_rgb(222, 222, 216),
            };
            p.rect_filled(key.shrink2(vec2(0.0, 0.5)), 1, fill);
            if pitch.is_multiple_of(12) {
                p.text(key.right_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, note_name(pitch), FontId::proportional(10.0), Color32::from_gray(60));
            }
        }
        // Grid lines.
        for s in 0..=TOTAL_STEPS {
            let x = grid.left() + s as f32 * CELL_W;
            let (w, c) = match s {
                s if s % STEPS == 0 => (1.5, Color32::from_gray(120)),
                s if s % 4 == 0 => (1.0, Color32::from_gray(78)),
                _ => (1.0, Color32::from_gray(56)),
            };
            p.line_segment([Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())], Stroke::new(w, c));
        }
        // Notes: each run of adjacent cells is one note.
        for k in 0..LEAD_KEYS {
            let pitch = LEAD_LOW + k as u8;
            let y = grid.top() + (LEAD_KEYS - 1 - k) as f32 * ROW_H;
            let mut s = 0;
            while s < TOTAL_STEPS {
                if !app.song.note(pitch, s) {
                    s += 1;
                    continue;
                }
                let start = s;
                while s < TOTAL_STEPS && app.song.note(pitch, s) {
                    s += 1;
                }
                let r = Rect::from_min_size(Pos2::new(grid.left() + start as f32 * CELL_W, y), vec2((s - start) as f32 * CELL_W, ROW_H)).shrink(1.0);
                let col = if app.song.lead_bars[start / STEPS] { CH_COLORS[5] } else { CH_COLORS[5].gamma_multiply(0.35) };
                p.rect_filled(r, 3, col);
                p.rect_stroke(r, 3, Stroke::new(1.0, col.gamma_multiply(0.5)), StrokeKind::Inside);
                if r.width() > 26.0 {
                    p.text(r.left_center() + vec2(3.0, 0.0), Align2::LEFT_CENTER, note_name(pitch), FontId::proportional(9.0), CLIP_TEXT);
                }
            }
        }
        if let Some(c) = app.cur {
            let x = grid.left() + c as f32 * CELL_W;
            p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, grid.bottom())], Stroke::new(2.0, ACCENT_HOT));
        }
    });
}

const LABEL_W: f32 = 96.0;
const BAR_W: f32 = 140.0;
const TRACK_H: f32 = 40.0;

fn playlist(app: &mut App, ui: &mut Ui) {
    egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
        let size = vec2(LABEL_W + BAR_W * BARS as f32, RULER_H + TRACK_H * 3.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let gx = rect.left() + LABEL_W;
        ui.painter().rect_filled(rect, 0, BG_DEEP);
        let clip_rect = |row: usize, bar: usize| Rect::from_min_size(Pos2::new(gx + bar as f32 * BAR_W, rect.top() + RULER_H + row as f32 * TRACK_H), vec2(BAR_W, TRACK_H)).shrink(2.0);

        // Ruler: click a bar number to select that bar.
        for bar in 0..BARS {
            let r = Rect::from_min_size(Pos2::new(gx + bar as f32 * BAR_W, rect.top()), vec2(BAR_W, RULER_H));
            let resp = ui.interact(r, ui.scope_id().with(("ruler", bar)), Sense::click()).on_hover_text("Select bar");
            if resp.clicked() {
                app.selected_bar = bar;
            }
            let col = if app.selected_bar == bar { ACCENT_HOT } else { Color32::from_gray(190) };
            ui.painter().text(r.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, format!("{}", bar + 1), FontId::proportional(12.0), col);
        }
        // Track labels and lane backgrounds.
        for (row, (name, color)) in [("Chords", CH_COLORS[3]), ("Drums", CH_COLORS[0]), ("Melody", CH_COLORS[5])].into_iter().enumerate() {
            let y = rect.top() + RULER_H + row as f32 * TRACK_H;
            let label = Rect::from_min_size(Pos2::new(rect.left(), y), vec2(LABEL_W - 4.0, TRACK_H)).shrink(2.0);
            ui.painter().rect_filled(label, 3, BG_WINDOW);
            ui.painter().rect_filled(Rect::from_min_size(label.min, vec2(4.0, label.height())), 2, color);
            ui.painter().text(label.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(13.0), Color32::from_gray(220));
            let lane = Rect::from_min_size(Pos2::new(gx, y), vec2(BAR_W * BARS as f32, TRACK_H));
            ui.painter().rect_filled(lane, 0, if row % 2 == 0 { Color32::from_rgb(40, 44, 50) } else { Color32::from_rgb(36, 40, 45) });
        }
        for bar in 0..=BARS {
            let x = gx + bar as f32 * BAR_W;
            ui.painter().line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, Color32::from_gray(70)));
        }

        for bar in 0..BARS {
            // Chord clip: click selects, right-click picks, drop a chord from the browser.
            let r = clip_rect(0, bar);
            let resp = ui.interact(r, ui.scope_id().with(("chord", bar)), Sense::click());
            if resp.clicked() {
                app.selected_bar = bar;
            }
            if let Some(j) = resp.dnd_release_payload::<usize>() {
                app.song.chords[bar] = *j;
                app.selected_bar = bar;
            }
            let j = app.song.chords[bar];
            let p = ui.painter();
            if j == 0 {
                p.rect_stroke(r, 3, Stroke::new(1.0, Color32::from_gray(80)), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, "drop chord", FontId::proportional(11.0), Color32::from_gray(110));
            } else {
                p.rect_filled(r, 3, chord_color(j));
                p.text(r.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, CHORDS[j].name, FontId::proportional(14.0), CLIP_TEXT);
            }
            if app.selected_bar == bar {
                p.rect_stroke(r, 3, Stroke::new(2.0, ACCENT_HOT), StrokeKind::Outside);
            }
            if resp.dnd_hover_payload::<usize>().is_some() {
                p.rect_stroke(r, 3, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
            }
            resp.on_hover_text("Click to select · right-click to choose a chord").context_menu(|ui| {
                for (k, c) in CHORDS.iter().enumerate() {
                    if ui.selectable_label(app.song.chords[bar] == k, c.name).clicked() {
                        app.song.chords[bar] = k;
                        ui.close();
                    }
                }
            });

            // Drum pattern clip: click toggles.
            let r = clip_rect(1, bar);
            let resp = ui.interact(r, ui.scope_id().with(("drums", bar)), Sense::click()).on_hover_text("Click to turn the drum pattern on/off for this bar");
            if resp.clicked() {
                app.song.drum_bars[bar] ^= true;
            }
            let p = ui.painter();
            if app.song.drum_bars[bar] {
                p.rect_filled(r, 3, CH_COLORS[0].gamma_multiply(0.75));
                p.text(r.left_top() + vec2(5.0, 3.0), Align2::LEFT_TOP, "Pattern 1", FontId::proportional(11.0), CLIP_TEXT);
                let sw = (r.width() - 10.0) / STEPS as f32;
                for (tr, row) in app.song.drums.iter().enumerate() {
                    for (s, &on) in row.iter().enumerate() {
                        if on {
                            let c = Pos2::new(r.left() + 5.0 + (s as f32 + 0.5) * sw, r.bottom() - 6.0 - tr as f32 * 6.0);
                            p.circle_filled(c, 1.8, CLIP_TEXT);
                        }
                    }
                }
            } else {
                p.rect_stroke(r, 3, Stroke::new(1.0, Color32::from_gray(80)), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, "+", FontId::proportional(16.0), Color32::from_gray(110));
            }

            // Melody clip: click toggles.
            let r = clip_rect(2, bar);
            let resp = ui.interact(r, ui.scope_id().with(("melody", bar)), Sense::click()).on_hover_text("Click to turn the melody on/off for this bar · double-click to edit");
            if resp.double_clicked() {
                app.open[Win::Roll as usize] = true;
            } else if resp.clicked() {
                app.song.lead_bars[bar] ^= true;
            }
            let p = ui.painter();
            if app.song.lead_bars[bar] {
                p.rect_filled(r, 3, CH_COLORS[5].gamma_multiply(0.75));
                p.text(r.left_top() + vec2(5.0, 3.0), Align2::LEFT_TOP, "Melody", FontId::proportional(11.0), CLIP_TEXT);
                let sw = (r.width() - 10.0) / STEPS as f32;
                for (k, row) in app.song.melody.iter().enumerate() {
                    for s in 0..STEPS {
                        if row >> (bar * STEPS + s) & 1 == 1 {
                            let y = r.bottom() - 4.0 - k as f32 / LEAD_KEYS as f32 * (r.height() - 18.0);
                            p.rect_filled(Rect::from_min_size(Pos2::new(r.left() + 5.0 + s as f32 * sw, y - 1.0), vec2(sw, 2.0)), 0, CLIP_TEXT);
                        }
                    }
                }
            } else {
                p.rect_stroke(r, 3, Stroke::new(1.0, Color32::from_gray(80)), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, "+", FontId::proportional(16.0), Color32::from_gray(110));
            }
        }

        if let Some(c) = app.cur {
            let x = gx + c as f32 / STEPS as f32 * BAR_W;
            ui.painter().line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, ACCENT_HOT));
            ui.painter().add(egui::Shape::convex_polygon(
                vec![Pos2::new(x - 5.0, rect.top()), Pos2::new(x + 5.0, rect.top()), Pos2::new(x, rect.top() + 7.0)],
                ACCENT_HOT,
                Stroke::NONE,
            ));
        }
    });
}

fn mixer(app: &mut App, ui: &mut Ui) {
    ui.horizontal_top(|ui| {
        for b in 0..BUSES {
            let color = CH_COLORS[[0, 3, 4, 5][b]];
            strip(ui, BUS_NAMES[b], color, &mut app.song.bus[b], app.meters[b]);
        }
        ui.separator();
        strip(ui, "Master", ACCENT, &mut app.song.fx.volume, app.meters[MASTER]);
        ui.separator();
        egui::Frame::new().fill(BG_PANEL).corner_radius(3).inner_margin(8).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Master insert · Lo-fi").strong());
                ui.add_space(6.0);
                let fx = &mut app.song.fx;
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
