//! Custom DAW widgets: theme, knob, LED, step button, level meter, LCD display.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use std::f32::consts::PI;
use std::ops::RangeInclusive;

pub const ACCENT: Color32 = Color32::from_rgb(242, 158, 56);
pub const ACCENT_HOT: Color32 = Color32::from_rgb(255, 205, 130);
pub const BG_DEEP: Color32 = Color32::from_rgb(24, 27, 31);
pub const BG_PANEL: Color32 = Color32::from_rgb(40, 45, 51);
pub const BG_WINDOW: Color32 = Color32::from_rgb(50, 56, 63);
pub const LCD_BG: Color32 = Color32::from_rgb(14, 22, 18);
pub const LCD_FG: Color32 = Color32::from_rgb(120, 230, 150);

/// Per-instrument colors, used in the channel rack, playlist and mixer.
pub const CH_COLORS: [Color32; 6] = [
    Color32::from_rgb(232, 96, 82),
    Color32::from_rgb(236, 170, 70),
    Color32::from_rgb(220, 214, 92),
    Color32::from_rgb(110, 170, 240),
    Color32::from_rgb(150, 120, 230),
    Color32::from_rgb(90, 200, 160),
];

pub fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG_PANEL;
    v.window_fill = BG_WINDOW;
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = Color32::from_rgb(46, 51, 58);
    v.selection.bg_fill = ACCENT.linear_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, ACCENT_HOT);
    v.hyperlink_color = ACCENT;
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(70, 78, 88));
    v.window_corner_radius = CornerRadius::same(4);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(62, 69, 78);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(78, 86, 97);
    v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.7);
    ctx.set_visuals(v);
}

/// Rotary knob: drag up/down, scroll, or arrow keys when focused; double-click resets.
pub fn knob(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, default: f32, label: &str, color: Color32) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click_and_drag());
    let old = *value;
    if resp.dragged() {
        *value -= resp.drag_delta().y / 120.0 * (hi - lo);
    }
    if resp.hovered() {
        *value += ui.input(|i| i.smooth_scroll_delta.y) / 600.0 * (hi - lo);
    }
    if resp.has_focus() {
        let step = (hi - lo) / 50.0;
        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowRight) {
                *value += step;
            }
            if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowLeft) {
                *value -= step;
            }
        });
    }
    if resp.double_clicked() {
        *value = default;
    }
    *value = value.clamp(lo, hi);
    if *value != old {
        resp.mark_changed();
    }
    let shown = *value;
    resp.widget_info(|| egui::WidgetInfo::slider(true, shown as f64, label));

    let p = ui.painter();
    let c = rect.center();
    let r = rect.width() / 2.0 - 2.0;
    p.circle_filled(c, r, Color32::from_rgb(30, 34, 39));
    let (a0, a1) = (0.75 * PI, 2.25 * PI);
    let t = (shown - lo) / (hi - lo);
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        (0..=24).map(|i| {
            let a = from + (to - from) * i as f32 / 24.0;
            c + Vec2::angled(a) * r
        }).collect()
    };
    p.add(Shape::line(arc(a0, a1), Stroke::new(2.0, Color32::from_gray(70))));
    // Bipolar ranges (pan) draw the value arc from the center.
    let from = if lo < 0.0 { 1.5 * PI } else { a0 };
    let at = a0 + (a1 - a0) * t;
    if (at - from).abs() > 0.01 {
        p.add(Shape::line(arc(from.min(at), from.max(at)), Stroke::new(2.5, color)));
    }
    p.line_segment([c, c + Vec2::angled(at) * (r - 2.0)], Stroke::new(2.0, Color32::from_gray(230)));
    if resp.has_focus() {
        p.circle_stroke(c, r + 1.5, Stroke::new(1.0, ACCENT_HOT));
    }
    resp.on_hover_text(format!("{label}: {}", if lo < 0.0 { format!("{:+.0}", shown * 100.0) } else { format!("{:.0}%", shown * 100.0) }))
}

/// A right-click menu that stays open while you click inside it (so a text field in it can be
/// focused); it closes on a click outside or when an item calls `ui.close()`.
pub fn edit_menu(resp: &Response, add_contents: impl FnOnce(&mut Ui)) {
    egui::Popup::context_menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(add_contents);
}

/// Small round on/off light.
pub fn led(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let state = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, state, label));
    let fill = if state { Color32::from_rgb(110, 240, 120) } else { Color32::from_rgb(40, 60, 44) };
    ui.painter().circle_filled(rect.center(), 5.0, fill);
    ui.painter().circle_stroke(rect.center(), 5.0, Stroke::new(1.0, Color32::from_gray(20)));
    resp.on_hover_text(label)
}

/// One sequencer step. `group` alternates the shade of each 4-step beat.
pub fn step_button(ui: &mut Ui, on: bool, group: bool, playing: bool, color: Color32, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(20.0, 28.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
    let fill = match (on, group) {
        (true, _) => color,
        (false, true) => Color32::from_rgb(72, 80, 90),
        (false, false) => Color32::from_rgb(58, 64, 72),
    };
    let fill = if resp.hovered() { fill.gamma_multiply(1.2) } else { fill };
    let r = rect.shrink(1.5);
    ui.painter().rect_filled(r, 3, fill);
    // Highlight strip, like a lit hardware pad.
    ui.painter().rect_filled(Rect::from_min_size(r.min + Vec2::new(2.0, 2.0), Vec2::new(r.width() - 4.0, 3.0)), 1, fill.gamma_multiply(1.3));
    if playing {
        ui.painter().rect_stroke(r, 3, Stroke::new(1.5, ACCENT_HOT), egui::StrokeKind::Outside);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(r, 3, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
    }
    resp.on_hover_text(label)
}

/// Linear level → 0..1 meter height on a -48..0 dB scale.
pub fn meter_pos(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 48.0) / 48.0).clamp(0.0, 1.0)
}

/// Vertical segmented level meter.
pub fn meter(ui: &mut Ui, level: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2, BG_DEEP);
    let segs = 24;
    let lit = (meter_pos(level) * segs as f32).round() as usize;
    let seg_h = rect.height() / segs as f32;
    for i in 0..segs {
        let y = rect.bottom() - (i + 1) as f32 * seg_h;
        let r = Rect::from_min_size(Pos2::new(rect.left() + 1.0, y + 1.0), Vec2::new(rect.width() - 2.0, seg_h - 1.5));
        let base = match i {
            i if i >= segs - 2 => Color32::from_rgb(240, 70, 60),
            i if i >= segs - 6 => Color32::from_rgb(235, 200, 70),
            _ => Color32::from_rgb(90, 220, 110),
        };
        p.rect_filled(r, 0, if i < lit { base } else { base.gamma_multiply(0.15) });
    }
}

/// Green-on-black segment-style display.
pub fn lcd(ui: &mut Ui, big: &str, small: &str) {
    egui::Frame::new()
        .fill(LCD_BG)
        .stroke(Stroke::new(1.0, Color32::from_rgb(30, 50, 38)))
        .corner_radius(3)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(big).monospace().size(20.0).color(LCD_FG));
                ui.label(egui::RichText::new(small).monospace().size(10.0).color(LCD_FG.gamma_multiply(0.7)));
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_scale() {
        assert_eq!(meter_pos(0.0), 0.0);
        assert!((meter_pos(1.0) - 1.0).abs() < 1e-6);
        assert!((meter_pos(10f32.powf(-24.0 / 20.0)) - 0.5).abs() < 1e-4);
        assert_eq!(meter_pos(2.0), 1.0);
    }
}
