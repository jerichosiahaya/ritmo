mod engine;
mod widgets;
mod windows;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui::{self, Color32, Key, RichText};
use engine::{Cmd, Engine, Song, CHORDS, METERS, STEPS};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use widgets::*;

/// Holds the audio stream open. `pos` is the playing step + 1 (0 = stopped);
/// `meters` hold f32 peak bits (positive floats order like their bits, so fetch_max works).
struct Audio {
    _stream: cpal::Stream,
    tx: mpsc::Sender<Cmd>,
    pos: Arc<AtomicUsize>,
    meters: Arc<[AtomicU32; METERS]>,
}

fn start_audio() -> Result<Audio, String> {
    let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let pos = Arc::new(AtomicUsize::new(0));
    let meters: Arc<[AtomicU32; METERS]> = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let shared = (rx, pos.clone(), meters.clone());
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config.into(), shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, config.into(), shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, config.into(), shared),
        f => return Err(format!("unsupported sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Audio { _stream: stream, tx, pos, meters })
}

type Shared = (mpsc::Receiver<Cmd>, Arc<AtomicUsize>, Arc<[AtomicU32; METERS]>);

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(device: &cpal::Device, config: cpal::StreamConfig, shared: Shared) -> Result<cpal::Stream, String> {
    let (rx, pos, meters) = shared;
    let channels = config.channels as usize;
    let mut eng = Engine::new(config.sample_rate as f32);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                while let Ok(cmd) = rx.try_recv() {
                    eng.handle(cmd);
                }
                for frame in data.chunks_mut(channels) {
                    let [l, r] = eng.next_frame();
                    for (i, s) in frame.iter_mut().enumerate() {
                        *s = T::from_sample(if i % 2 == 0 { l } else { r });
                    }
                }
                pos.store(if eng.playing { eng.step + 1 } else { 0 }, Ordering::Relaxed);
                for (m, p) in meters.iter().zip(eng.take_peaks()) {
                    m.fetch_max(p.to_bits(), Ordering::Relaxed);
                }
            },
            |e| eprintln!("audio error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Typing keyboard as piano: bottom row = C3 octave, top row = C4 octave.
fn key_note(k: Key) -> Option<u8> {
    use Key::*;
    let n = match k {
        Z => 48, S => 49, X => 50, D => 51, C => 52, V => 53, G => 54, B => 55, H => 56, N => 57, J => 58, M => 59,
        Q => 60, Num2 => 61, W => 62, Num3 => 63, E => 64, R => 65, Num5 => 66, T => 67, Num6 => 68, Y => 69,
        Num7 => 70, U => 71, I => 72, Num9 => 73, O => 74, Num0 => 75, P => 76,
        _ => return None,
    };
    Some(n)
}

/// Floating windows, in toolbar order.
#[derive(Clone, Copy, PartialEq)]
enum Win {
    Playlist,
    Rack,
    Roll,
    Mixer,
}

const WINDOWS: [(Win, &str, Key); 4] = [
    (Win::Playlist, "▤ Playlist", Key::F5),
    (Win::Rack, "▦ Rack", Key::F6),
    (Win::Roll, "♫ Roll", Key::F7),
    (Win::Mixer, "☰ Mixer", Key::F9),
];

struct App {
    song: Song,
    sent: Option<Song>,
    playing: bool,
    audio: Option<Audio>,
    status: String,
    export_rx: Option<mpsc::Receiver<String>>,
    open: [bool; 4],
    /// Smoothed meter levels for display.
    meters: [f32; METERS],
    /// Playlist bar that a clicked chord in the browser goes to.
    selected_bar: usize,
    /// Piano-roll drag: Some(true) draws notes, Some(false) erases.
    roll_paint: Option<bool>,
    /// Note sounding from a click on the piano-roll keyboard.
    preview: Option<u8>,
    /// Current playing step, if playing.
    cur: Option<usize>,
}

impl App {
    fn send(&self, cmd: Cmd) {
        if let Some(a) = &self.audio {
            let _ = a.tx.send(cmd);
        }
    }

    fn set_playing(&mut self, on: bool) {
        self.playing = on;
        self.send(Cmd::Play(on));
    }

    fn is_open(&self, w: Win) -> bool {
        self.open[w as usize]
    }

    fn export(&mut self) {
        let song = self.song;
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let dir = std::path::Path::new(&home).join("Music");
        let dir = if dir.is_dir() { dir } else { std::path::PathBuf::from(".") };
        let path = dir.join("ritmo-export.wav");
        let (tx, rx) = mpsc::channel();
        self.status = "Exporting…".into();
        self.export_rx = Some(rx);
        std::thread::spawn(move || {
            let msg = match engine::export_wav(&path, song) {
                Ok(()) => format!("Exported {}", path.display()),
                Err(e) => format!("Export failed: {e}"),
            };
            let _ = tx.send(msg);
        });
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        for ev in ctx.input(|i| i.events.clone()) {
            let egui::Event::Key { key, pressed, repeat: false, .. } = ev else { continue };
            if let Some(&(w, _, _)) = WINDOWS.iter().find(|(_, _, k)| *k == key) {
                if pressed {
                    self.open[w as usize] ^= true;
                }
            } else if key == Key::Space && pressed {
                self.set_playing(!self.playing);
            } else if let Some(n) = key_note(key) {
                self.send(if pressed { Cmd::NoteOn(n) } else { Cmd::NoteOff(n) });
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("◆ RITMO").strong().size(18.0).color(ACCENT));
            ui.separator();
            if ui.add(egui::Button::selectable(self.playing, RichText::new("▶").size(16.0))).on_hover_text("Play (Space)").clicked() {
                self.set_playing(true);
            }
            if ui.add(egui::Button::new(RichText::new("■").size(16.0))).on_hover_text("Stop (Space)").clicked() {
                self.set_playing(false);
            }
            let (bar, step) = self.cur.map_or((1, 1), |c| (c / STEPS + 1, c % STEPS + 1));
            lcd(ui, &format!("{bar}:{step:02}"), &format!("{:.0} BPM\nBAR:STEP", self.song.bpm));
            ui.add(egui::DragValue::new(&mut self.song.bpm).range(50.0..=180.0).speed(0.5).suffix(" bpm")).on_hover_text("Tempo: drag or type");
            ui.label("Swing");
            knob(ui, &mut self.song.swing, 0.5..=0.75, 0.6, "Swing", ACCENT);
            ui.label("Master");
            knob(ui, &mut self.song.fx.volume, 0.0..=1.0, 0.8, "Master volume", ACCENT);
            ui.separator();
            for (w, name, key) in WINDOWS {
                let open = &mut self.open[w as usize];
                if ui.add(egui::Button::selectable(*open, name)).on_hover_text(format!("{name} ({key:?})")).clicked() {
                    *open ^= true;
                }
            }
            ui.separator();
            if ui.add_enabled(self.export_rx.is_none(), egui::Button::new("⬇ Export")).on_hover_text("Export WAV to ~/Music/ritmo-export.wav").clicked() {
                self.export();
            }
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("BROWSER").strong().color(Color32::GRAY));
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::CollapsingHeader::new("🎹 Instruments").default_open(true).show(ui, |ui| {
                for (i, (name, kind)) in [("Kick", "synth drum"), ("Snare", "synth drum"), ("Hat", "synth drum"), ("Keys", "FM Rhodes"), ("Bass", "sine bass"), ("Lead", "FM Rhodes")].iter().enumerate() {
                    let text = RichText::new(format!("■ {name}")).color(CH_COLORS[i]);
                    if ui.selectable_label(false, text).on_hover_text(*kind).clicked() {
                        self.open[if i == 5 { Win::Roll } else { Win::Rack } as usize] = true;
                    }
                }
            });
            egui::CollapsingHeader::new("🎵 Chords").default_open(true).show(ui, |ui| {
                ui.label(RichText::new(format!("Click: set bar {}\nDrag: drop on a playlist bar", self.selected_bar + 1)).small().weak());
                for (j, c) in CHORDS.iter().enumerate().skip(1) {
                    let id = egui::Id::unique(("chord-src", j));
                    let resp = ui.dnd_drag_source(id, j, |ui| {
                        ui.label(RichText::new(format!("♪ {}", c.name)).color(chord_color(j)));
                    }).response;
                    if resp.clicked() {
                        self.song.chords[self.selected_bar] = j;
                    }
                }
            });
            egui::CollapsingHeader::new("▦ Patterns").default_open(true).show(ui, |ui| {
                if ui.selectable_label(false, "Pattern 1 (drums)").clicked() {
                    self.open[Win::Rack as usize] = true;
                }
                if ui.selectable_label(false, "Melody (lead)").clicked() {
                    self.open[Win::Roll as usize] = true;
                }
            });
        });
    }
}

pub fn chord_color(j: usize) -> Color32 {
    let hues = [(120, 150, 220), (200, 120, 200), (110, 190, 170), (220, 150, 110), (150, 200, 110), (200, 110, 120), (120, 180, 220), (210, 190, 110)];
    let (r, g, b) = hues[(j + hues.len() - 1) % hues.len()];
    Color32::from_rgb(r, g, b)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        if let Some(msg) = self.export_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.status = msg;
            self.export_rx = None;
        }
        if let Some(a) = &self.audio {
            self.cur = a.pos.load(Ordering::Relaxed).checked_sub(1);
            for (shown, m) in self.meters.iter_mut().zip(a.meters.iter()) {
                *shown = (*shown * 0.85).max(f32::from_bits(m.swap(0, Ordering::Relaxed)));
            }
        }

        egui::Panel::top("toolbar")
            .exact_size(44.0)
            .frame(egui::Frame::new().fill(BG_DEEP).inner_margin(egui::Margin::symmetric(8, 4)))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .exact_size(22.0)
            .frame(egui::Frame::new().fill(BG_DEEP).inner_margin(egui::Margin::symmetric(8, 2)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Keys: Z–M = C3 octave · Q–P = C4 octave · Space play/stop · F5 Playlist · F6 Rack · F7 Piano Roll · F9 Mixer").small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(&self.status).small()));
                });
            });
        egui::Panel::left("browser").default_size(170.0).resizable(true).show(ui, |ui| self.browser(ui));
        let area = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().rect_filled(r, 0, Color32::from_rgb(33, 37, 42));
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "ritmo", egui::FontId::proportional(64.0), Color32::from_white_alpha(6));
                r
            })
            .inner;

        windows::show(self, &ctx, area);

        if self.sent != Some(self.song) {
            self.send(Cmd::Song(self.song));
            self.sent = Some(self.song);
        }
        if self.playing || self.export_rx.is_some() || self.meters.iter().any(|&m| m > 1e-3) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

fn main() -> eframe::Result {
    let (audio, status) = match start_audio() {
        Ok(a) => (Some(a), "Ready".to_string()),
        Err(e) => (None, format!("Audio unavailable ({e}); Export WAV still works.")),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_min_inner_size([900.0, 600.0]).with_title("Ritmo"),
        ..Default::default()
    };
    eframe::run_native(
        "Ritmo",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(App {
                song: Song::default(),
                sent: None,
                playing: false,
                audio,
                status,
                export_rx: None,
                open: [true, true, false, false],
                meters: [0.0; METERS],
                selected_bar: 0,
                roll_paint: None,
                preview: None,
                cur: None,
            }))
        }),
    )
}

