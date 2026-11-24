mod engine;
mod project;
mod rt;
mod widgets;
mod windows;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui::{self, Color32, Key, RichText};
use engine::{Cmd, Engine, Instrument, METERS, Snapshot, STEP};
use project::{CHORDS, ChannelId, PatternId, Project, Tick, TrackId};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use widgets::*;

/// Holds the audio stream open. `pos` is the playing tick + 1 (0 = stopped);
/// `meters` hold f32 peak bits (positive floats order like their bits, so fetch_max works).
/// `trash` brings back snapshots the engine replaced, to be freed on the UI thread.
struct Audio {
    _stream: cpal::Stream,
    tx: mpsc::SyncSender<Cmd>,
    trash: mpsc::Receiver<Box<Snapshot>>,
    pos: Arc<AtomicUsize>,
    meters: Arc<[AtomicU32; METERS]>,
}

fn start_audio() -> Result<Audio, String> {
    let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let (tx, rx) = engine::command_channel();
    let (trash_tx, trash) = engine::trash_channel();
    let pos = Arc::new(AtomicUsize::new(0));
    let meters: Arc<[AtomicU32; METERS]> = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let shared = (rx, trash_tx, pos.clone(), meters.clone());
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config.into(), shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, config.into(), shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, config.into(), shared),
        f => return Err(format!("unsupported sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Audio { _stream: stream, tx, trash, pos, meters })
}

type Shared = (mpsc::Receiver<Cmd>, mpsc::SyncSender<Box<Snapshot>>, Arc<AtomicUsize>, Arc<[AtomicU32; METERS]>);

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(device: &cpal::Device, config: cpal::StreamConfig, shared: Shared) -> Result<cpal::Stream, String> {
    let (rx, trash, pos, meters) = shared;
    let channels = config.channels as usize;
    let mut eng = Engine::new(config.sample_rate as f32).with_trash(trash);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                rt::enter();
                while let Ok(cmd) = rx.try_recv() {
                    eng.handle(cmd);
                }
                for frame in data.chunks_mut(channels) {
                    let [l, r] = eng.next_frame();
                    for (i, s) in frame.iter_mut().enumerate() {
                        *s = T::from_sample(if i % 2 == 0 { l } else { r });
                    }
                }
                pos.store(if eng.playing { eng.tick as usize + 1 } else { 0 }, Ordering::Relaxed);
                for (m, p) in meters.iter().zip(eng.take_peaks()) {
                    m.fetch_max(p.to_bits(), Ordering::Relaxed);
                }
                rt::exit();
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
    project: Project,
    /// The project as last sent to the engine, and its compiled form (the windows draw previews
    /// from it).
    sent: Option<Project>,
    snap: Snapshot,
    playing: bool,
    audio: Option<Audio>,
    status: String,
    export_rx: Option<mpsc::Receiver<String>>,
    open: [bool; 4],
    /// Smoothed meter levels for display.
    meters: [f32; METERS],
    /// Playlist selection; a chord clicked in the browser is stamped there.
    sel_track: Option<TrackId>,
    sel_bar: Tick,
    /// Pattern the Channel Rack's step grid edits.
    pattern: PatternId,
    /// Pattern and channel the Piano Roll edits, and its lowest visible key.
    roll: (PatternId, ChannelId),
    roll_low: u8,
    /// Channel the typing keyboard and the piano-roll keys play.
    live: ChannelId,
    /// Piano-roll drag: Some(true) draws notes, Some(false) erases.
    roll_paint: Option<bool>,
    /// Note sounding from a click on the piano-roll keyboard.
    preview: Option<u8>,
    /// Playing tick, if playing.
    cur: Option<Tick>,
}

impl App {
    fn new(project: Project, audio: Option<Audio>, status: String) -> Self {
        let has_notes = |p: &project::Pattern, f: &dyn Fn(Instrument) -> bool| p.notes.iter().any(|n| project.channel(n.channel).is_some_and(|c| f(c.instrument)));
        let pattern = project.patterns.iter().find(|p| has_notes(p, &Instrument::is_drum)).or(project.patterns.first()).map_or(PatternId(0), |p| p.id);
        // The piano roll starts on the last melodic channel (the lead in the demo).
        let roll_ch = project.channels.iter().rev().find(|c| !c.instrument.is_drum()).or(project.channels.first()).map_or(ChannelId(0), |c| c.id);
        let roll_pat = project.patterns.iter().find(|p| p.notes.iter().any(|n| n.channel == roll_ch)).map_or(pattern, |p| p.id);
        let mut app = App {
            sel_track: project.tracks.first().map(|t| t.id),
            project,
            sent: None,
            snap: Snapshot::default(),
            playing: false,
            audio,
            status,
            export_rx: None,
            open: [true, true, false, false],
            meters: [0.0; METERS],
            sel_bar: 0,
            pattern,
            roll: (roll_pat, roll_ch),
            roll_low: 60,
            live: roll_ch,
            roll_paint: None,
            preview: None,
            cur: None,
        };
        app.edit_in_roll(roll_pat, roll_ch);
        app.open[Win::Roll as usize] = false;
        app
    }

    /// Queue a command for the audio thread without blocking. False if there is no audio
    /// or the queue is full (the audio thread drains it every callback, so it rarely is).
    fn send(&self, cmd: Cmd) -> bool {
        self.audio.as_ref().is_some_and(|a| a.tx.try_send(cmd).is_ok())
    }

    fn set_playing(&mut self, on: bool) {
        self.playing = on;
        self.send(Cmd::Play(on));
    }

    fn is_open(&self, w: Win) -> bool {
        self.open[w as usize]
    }

    /// Engine index of the channel the keyboard plays.
    fn live_index(&self) -> u16 {
        self.project.channel_index(self.live).map_or(u16::MAX, |i| i as u16)
    }

    /// Opens the piano roll on `channel`'s notes in `pattern`, scrolled to them, and makes the
    /// keyboard play that channel.
    fn edit_in_roll(&mut self, pattern: PatternId, channel: ChannelId) {
        self.roll = (pattern, channel);
        self.live = channel;
        let lowest = self.project.pattern(pattern).and_then(|p| p.notes.iter().filter(|n| n.channel == channel).map(|n| n.pitch).min());
        let default = if self.project.channel(channel).is_some_and(|c| c.instrument == Instrument::Bass) { 36 } else { 60 };
        let low = lowest.map_or(default, |n| n - n % 12);
        self.roll_low = low.min(127 - windows::ROLL_KEYS as u8 + 1);
        self.open[Win::Roll as usize] = true;
    }

    /// Opens the piano roll on the pattern `channel` plays most from (or the rack's pattern).
    fn edit_channel(&mut self, channel: ChannelId) {
        let count = |p: &&project::Pattern| p.notes.iter().filter(|n| n.channel == channel).count();
        let pattern = self.project.patterns.iter().filter(|p| count(p) > 0).max_by_key(count).map_or(self.pattern, |p| p.id);
        self.edit_in_roll(pattern, channel);
    }

    /// Stamps chord `j` on the playlist bar `bar` of `track`, in whatever clip plays there.
    fn stamp_chord(&mut self, track: TrackId, bar: Tick, j: usize) {
        let bl = self.project.bar_len();
        let Some(c) = self.project.clip_at(track, bar * bl).copied() else {
            self.status = "Pick a bar that has a clip to put the chord in.".into();
            return;
        };
        if !self.project.stamp_chord(c.pattern, bar * bl - c.start + c.offset, bl, j) {
            self.status = "Chords need an electric-piano channel in the Channel Rack.".into();
        }
    }

    fn export(&mut self) {
        let snap = self.project.compile();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let dir = std::path::Path::new(&home).join("Music");
        let dir = if dir.is_dir() { dir } else { std::path::PathBuf::from(".") };
        let path = dir.join("ritmo-export.wav");
        let (tx, rx) = mpsc::channel();
        self.status = "Exporting…".into();
        self.export_rx = Some(rx);
        std::thread::spawn(move || {
            let msg = match engine::export_wav(&path, &snap) {
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
            } else if let Some(pitch) = key_note(key) {
                let channel = self.live_index();
                self.send(if pressed { Cmd::NoteOn { channel, pitch } } else { Cmd::NoteOff { channel, pitch } });
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
            let bl = self.project.bar_len();
            let (bar, step) = self.cur.map_or((1, 1), |t| (t / bl + 1, t % bl / STEP + 1));
            lcd(ui, &format!("{bar}:{step:02}"), &format!("{:.0} BPM\nBAR:STEP", self.project.bpm));
            ui.add(egui::DragValue::new(&mut self.project.bpm).range(50.0..=180.0).speed(0.5).suffix(" bpm")).on_hover_text("Tempo: drag or type");
            ui.label("Swing");
            knob(ui, &mut self.project.swing, 0.5..=0.75, 0.6, "Swing", ACCENT);
            ui.label("Master");
            knob(ui, &mut self.project.master.volume, 0.0..=1.0, 0.8, "Master volume", ACCENT);
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
                for i in 0..self.project.channels.len() {
                    let c = &self.project.channels[i];
                    let (id, inst) = (c.id, c.instrument);
                    let text = RichText::new(format!("■ {}", c.name)).color(channel_color(i));
                    let hint = if inst.is_drum() { "Synth drum · click to play it from the keyboard" } else { "Click to edit in the piano roll" };
                    if ui.selectable_label(self.live == id, text).on_hover_text(hint).clicked() {
                        if inst.is_drum() {
                            self.live = id;
                            self.open[Win::Rack as usize] = true;
                        } else {
                            self.edit_channel(id);
                        }
                    }
                }
            });
            egui::CollapsingHeader::new("🎵 Chords").default_open(true).show(ui, |ui| {
                ui.label(RichText::new(format!("Click: put on bar {}\nDrag: drop on a playlist bar", self.sel_bar + 1)).small().weak());
                for (j, c) in CHORDS.iter().enumerate().skip(1) {
                    let id = egui::Id::unique(("chord-src", j));
                    let resp = ui.dnd_drag_source(id, j, |ui| {
                        ui.label(RichText::new(format!("♪ {}", c.name)).color(chord_color(j)));
                    }).response;
                    if resp.clicked()
                        && let Some(track) = self.sel_track
                    {
                        self.stamp_chord(track, self.sel_bar, j);
                    }
                }
            });
            egui::CollapsingHeader::new("▦ Patterns").default_open(true).show(ui, |ui| {
                for (i, p) in self.project.patterns.iter().enumerate() {
                    let resp = ui
                        .selectable_label(self.pattern == p.id, RichText::new(&p.name).color(pattern_color(i)))
                        .on_hover_text("Click: edit its steps in the Channel Rack · double-click: piano roll");
                    if resp.double_clicked() {
                        let ch = p.notes.iter().map(|n| n.channel).find(|&c| self.project.channel(c).is_some_and(|c| !c.instrument.is_drum()));
                        let (pid, ch) = (p.id, ch.unwrap_or(self.roll.1));
                        self.edit_in_roll(pid, ch);
                        break;
                    } else if resp.clicked() {
                        self.pattern = p.id;
                        self.open[Win::Rack as usize] = true;
                    }
                }
            });
        });
    }
}

/// Color for the `i`th channel or mixer track.
pub fn channel_color(i: usize) -> Color32 {
    CH_COLORS[i % CH_COLORS.len()]
}

/// Color for the `i`th pattern's clips.
pub fn pattern_color(i: usize) -> Color32 {
    chord_color(i * 3 + 1)
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
            self.cur = a.pos.load(Ordering::Relaxed).checked_sub(1).map(|t| t as Tick);
            // Snapshots the engine replaced are freed here, off the audio thread.
            while a.trash.try_recv().is_ok() {}
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
                    let live = self.project.channel(self.live).map_or("—", |c| c.name.as_str());
                    ui.label(RichText::new(format!("Keys play {live}: Z–M = C3 · Q–P = C4 · Space play/stop · F5 Playlist · F6 Rack · F7 Piano Roll · F9 Mixer")).small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.status).small());
                        // Debug builds only: the real-time guard saw the audio thread use the heap.
                        let heap = rt::violations();
                        if heap > 0 {
                            ui.label(RichText::new(format!("⚠ audio thread used the heap {heap}×")).small().color(ACCENT_HOT))
                                .on_hover_text("Debug build check: the audio callback allocated or freed memory, which can cause dropouts. See AGENTS.md.");
                        }
                    });
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

        // Send the project whenever it changed. If the queue was full, `sent` stays stale and it
        // goes out on the next frame.
        if self.sent.as_ref() != Some(&self.project) {
            // Debug builds check that every edit keeps the project valid.
            if let Err(e) = self.project.validate() {
                debug_assert!(false, "edit broke the project: {e}");
            }
            let snap = self.project.compile();
            if self.send(Cmd::Snapshot(Box::new(snap.clone()))) || self.audio.is_none() {
                self.snap = snap;
                self.sent = Some(self.project.clone());
            }
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
            Ok(Box::new(App::new(Project::demo(), audio, status)))
        }),
    )
}
