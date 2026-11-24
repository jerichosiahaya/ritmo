mod engine;
mod file;
mod project;
mod rt;
mod widgets;
mod windows;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui::{self, Color32, Key, RichText};
use engine::{Cmd, Engine, Instrument, METERS, Snapshot, STEP};
use project::{CHORDS, ChannelId, PatternId, Project, Tick, TrackId};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
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

/// What to do once unsaved changes are dealt with.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Then {
    Nothing,
    New { demo: bool },
    Open,
    Quit,
}

/// A file dialog's answer, from its thread.
enum Dialog {
    Open(Option<PathBuf>),
    SaveAs(Option<PathBuf>, Then),
}

/// How often the project is autosaved while it changes.
const AUTOSAVE_EVERY: Duration = Duration::from_secs(60);

struct App {
    project: Project,
    /// The file the project was opened from or saved to.
    path: Option<PathBuf>,
    /// The project as last saved or opened; anything else is unsaved.
    saved: Project,
    /// The project as last autosaved.
    autosaved: Option<Project>,
    last_autosave: Instant,
    /// An open file dialog.
    dialog_rx: Option<mpsc::Receiver<Dialog>>,
    /// Waiting for an answer to "Save changes?" before doing this.
    ask_save: Option<Then>,
    /// The user confirmed closing the window.
    quitting: bool,
    /// Window title as last set.
    title: String,
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
    fn new(project: Project, path: Option<PathBuf>, audio: Option<Audio>, status: String) -> Self {
        let mut app = App {
            sel_track: None,
            saved: Project::default(),
            project: Project::default(),
            path: None,
            autosaved: None,
            last_autosave: Instant::now(),
            dialog_rx: None,
            ask_save: None,
            quitting: false,
            title: String::new(),
            sent: None,
            snap: Snapshot::default(),
            playing: false,
            audio,
            status,
            export_rx: None,
            open: [true, true, false, false],
            meters: [0.0; METERS],
            sel_bar: 0,
            pattern: PatternId(0),
            roll: (PatternId(0), ChannelId(0)),
            roll_low: 60,
            live: ChannelId(0),
            roll_paint: None,
            preview: None,
            cur: None,
        };
        app.set_project(project, path);
        app
    }

    /// Starts on `project`, read from `path` if it came from a file: resets the selections and
    /// counts it as saved.
    fn set_project(&mut self, project: Project, path: Option<PathBuf>) {
        let has_notes = |p: &project::Pattern, f: &dyn Fn(Instrument) -> bool| p.notes.iter().any(|n| project.channel(n.channel).is_some_and(|c| f(c.instrument)));
        let pattern = project.patterns.iter().find(|p| has_notes(p, &Instrument::is_drum)).or(project.patterns.first()).map_or(PatternId(0), |p| p.id);
        // The piano roll starts on the last melodic channel (the lead in the demo).
        let roll_ch = project.channels.iter().rev().find(|c| !c.instrument.is_drum()).or(project.channels.first()).map_or(ChannelId(0), |c| c.id);
        let roll_pat = project.patterns.iter().find(|p| p.notes.iter().any(|n| n.channel == roll_ch)).map_or(pattern, |p| p.id);
        self.sel_track = project.tracks.first().map(|t| t.id);
        self.sel_bar = 0;
        self.pattern = pattern;
        self.saved = project.clone();
        self.project = project;
        self.path = path;
        let roll_open = self.is_open(Win::Roll);
        self.edit_in_roll(roll_pat, roll_ch);
        self.open[Win::Roll as usize] = roll_open;
    }

    fn dirty(&self) -> bool {
        self.project != self.saved
    }

    fn file_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or("Untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    /// Runs `then`, first asking to save unsaved changes.
    fn request(&mut self, ctx: &egui::Context, then: Then) {
        if self.dirty() { self.ask_save = Some(then) } else { self.run(ctx, then) }
    }

    fn run(&mut self, ctx: &egui::Context, then: Then) {
        match then {
            Then::Nothing => {}
            Then::New { demo } => {
                self.set_project(if demo { Project::demo() } else { Project::blank() }, None);
                self.status = if demo { "New project from the demo song".into() } else { "New empty project".into() };
            }
            Then::Open => {
                let dir = self.dialog_dir();
                self.file_dialog(move || Dialog::Open(rfd::FileDialog::new().set_title("Open project").add_filter("Ritmo project", &[file::EXTENSION]).set_directory(dir).pick_file()));
            }
            Then::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Saves to the current file (or asks where), then runs `then`.
    fn save(&mut self, ctx: &egui::Context, then: Then) {
        match self.path.clone() {
            Some(path) => {
                if self.write(&path) {
                    self.run(ctx, then);
                }
            }
            None => self.save_as(then),
        }
    }

    fn save_as(&mut self, then: Then) {
        let (dir, name) = (self.dialog_dir(), format!("{}.{}", self.path.as_ref().and_then(|p| p.file_stem()).map_or("Untitled".into(), |s| s.to_string_lossy().into_owned()), file::EXTENSION));
        self.file_dialog(move || {
            let picked = rfd::FileDialog::new().set_title("Save project").add_filter("Ritmo project", &[file::EXTENSION]).set_directory(dir).set_file_name(name).save_file();
            Dialog::SaveAs(picked.map(file::with_extension), then)
        });
    }

    /// Writes the project to `path`; false (with the reason in the status bar) if it failed.
    fn write(&mut self, path: &Path) -> bool {
        match file::save(path, &self.project) {
            Ok(()) => {
                self.saved = self.project.clone();
                self.path = Some(path.to_path_buf());
                self.status = format!("Saved {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Could not save {}: {e}", path.display());
                false
            }
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        match file::load(&path) {
            Ok(p) => {
                self.status = format!("Opened {}", path.display());
                self.set_project(p, Some(path));
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
        }
    }

    /// Folder the file dialogs start in: the project's, else ~/Music, else home.
    fn dialog_dir(&self) -> PathBuf {
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            return dir.to_path_buf();
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
        let music = home.join("Music");
        if music.is_dir() { music } else { home }
    }

    /// Shows a system file dialog on another thread so the window keeps drawing.
    fn file_dialog(&mut self, show: impl FnOnce() -> Dialog + Send + 'static) {
        if self.dialog_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.dialog_rx = Some(rx);
        self.status = "Waiting for the file dialog…".into();
        std::thread::spawn(move || {
            let _ = tx.send(show());
        });
    }

    /// Handles a closed file dialog.
    fn dialog_done(&mut self, ctx: &egui::Context, d: Dialog) {
        match d {
            Dialog::Open(Some(path)) => self.open_file(path),
            Dialog::SaveAs(Some(path), then) => {
                if self.write(&path) {
                    self.run(ctx, then);
                }
            }
            // Also what comes back when the desktop has no file chooser at all.
            Dialog::Open(None) | Dialog::SaveAs(None, _) => {
                self.status = "No file chosen. (No dialog appeared? Install xdg-desktop-portal-gtk, -gnome or -kde, or zenity.)".into();
            }
        }
    }

    /// Writes the autosave file if the project changed since the last one.
    fn autosave(&mut self) {
        if self.autosaved.as_ref() == Some(&self.project) {
            return;
        }
        match file::autosave(&self.project) {
            Ok(_) => self.autosaved = Some(self.project.clone()),
            Err(e) => self.status = format!("Autosave failed: {e}"),
        }
        self.last_autosave = Instant::now();
    }

    /// File shortcuts: Ctrl+N new, Ctrl+O open, Ctrl+S save, Ctrl+Shift+S save as.
    fn file_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{KeyboardShortcut as K, Modifiers as M};
        let hit = |m, k| ctx.input_mut(|i| i.consume_shortcut(&K::new(m, k)));
        // Shift first: Ctrl+S also matches Ctrl+Shift+S.
        if hit(M::COMMAND | M::SHIFT, Key::S) {
            self.save_as(Then::Nothing);
        } else if hit(M::COMMAND, Key::S) {
            self.save(ctx, Then::Nothing);
        } else if hit(M::COMMAND, Key::O) {
            self.request(ctx, Then::Open);
        } else if hit(M::COMMAND, Key::N) {
            self.request(ctx, Then::New { demo: false });
        }
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let item = |ui: &mut egui::Ui, text: &str, keys: &str| ui.add(egui::Button::new(text).shortcut_text(keys));
        ui.menu_button("File", |ui| {
            if item(ui, "New empty project", "Ctrl+N").clicked() {
                self.request(&ctx, Then::New { demo: false });
            }
            if item(ui, "New from demo song", "").clicked() {
                self.request(&ctx, Then::New { demo: true });
            }
            if item(ui, "Open…", "Ctrl+O").clicked() {
                self.request(&ctx, Then::Open);
            }
            ui.separator();
            if item(ui, "Save", "Ctrl+S").clicked() {
                self.save(&ctx, Then::Nothing);
            }
            if item(ui, "Save as…", "Ctrl+Shift+S").clicked() {
                self.save_as(Then::Nothing);
            }
            ui.separator();
            if let Some(path) = file::autosave_path() {
                let home = std::env::var("HOME").unwrap_or_default();
                let shown = path.display().to_string().replacen(&home, "~", 1);
                ui.label(RichText::new(format!("Autosaved every minute to\n{shown}")).small().weak());
            }
        })
        .response
        .on_hover_text("New, open and save projects");
    }

    /// "Save changes?" before New, Open or quitting with unsaved changes.
    fn ask_save_modal(&mut self, ctx: &egui::Context) {
        let Some(then) = self.ask_save else { return };
        let name = self.file_name();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("ask-save")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(format!("Save changes to {name}?"));
            ui.label("If you don't save, the changes since the last save are lost.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(true);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.ask_save = None;
                }
            });
        });
        if modal.should_close() {
            self.ask_save = None;
        }
        if let Some(save) = choice {
            self.ask_save = None;
            if save { self.save(ctx, then) } else { self.run(ctx, then) }
        }
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
        let dir = Path::new(&home).join("Music");
        let dir = if dir.is_dir() { dir } else { PathBuf::from(".") };
        let stem = self.path.as_ref().and_then(|p| p.file_stem()).map_or("ritmo-export".into(), |s| s.to_string_lossy().into_owned());
        let path = dir.join(format!("{stem}.wav"));
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
            let egui::Event::Key { key, pressed, repeat: false, modifiers, .. } = ev else { continue };
            // Ctrl/Alt combinations are shortcuts, not notes.
            if modifiers.command || modifiers.ctrl || modifiers.alt {
                continue;
            }
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
            self.file_menu(ui);
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
        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            if self.dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.ask_save = Some(Then::Quit);
            } else {
                self.quitting = true;
            }
        }
        if self.quitting {
            self.autosave();
        }
        if let Some(d) = self.dialog_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.dialog_rx = None;
            self.dialog_done(&ctx, d);
        }
        self.file_shortcuts(&ctx);
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
        self.ask_save_modal(&ctx);

        let title = format!("{}{} — Ritmo", if self.dirty() { "• " } else { "" }, self.file_name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.last_autosave.elapsed() >= AUTOSAVE_EVERY {
            self.autosave();
        }

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
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.dialog_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(AUTOSAVE_EVERY);
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
            // `ritmo song.ritmo` opens that file; otherwise start with the demo song.
            let mut app = App::new(Project::demo(), None, audio, status);
            if let Some(path) = std::env::args_os().nth(1) {
                app.open_file(PathBuf::from(path));
            }
            Ok(Box::new(app))
        }),
    )
}
