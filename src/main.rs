mod engine;
mod file;
mod history;
mod project;
mod rt;
mod sample;
mod settings;
mod widgets;
mod windows;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui::{self, Color32, Key, RichText};
use engine::{Cmd, Engine, Instrument, METERS, SampleData, Snapshot, STEP};
use std::collections::HashMap;
use history::History;
use project::{CHOP_BASE, CHORDS, ChannelId, ChordError, ClipId, NoteId, MAX_SLICES, PatternId, Project, Tick, TrackId};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use settings::Settings;
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
    /// The device, sample rate and buffer in use, for the settings window.
    info: String,
}

/// Output devices: ID (to save in the settings) and name.
fn output_devices() -> Vec<(String, String)> {
    let devices = cpal::default_host().output_devices();
    devices.map(|ds| ds.filter_map(|d| Some((d.id().ok()?.to_string(), d.to_string()))).collect()).unwrap_or_default()
}

/// Opens the output in `settings`; a saved device that is gone falls back to the default.
fn start_audio(settings: &Settings) -> Result<Audio, String> {
    let host = cpal::default_host();
    let chosen = settings.device.as_ref().and_then(|id| id.parse::<cpal::DeviceId>().ok()).and_then(|id| host.device_by_id(&id));
    let device = chosen.or_else(|| host.default_output_device()).ok_or("no audio output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let mut config = supported.config();
    if let Some(n) = settings.buffer {
        let n = match supported.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => n.clamp(*min, *max),
            cpal::SupportedBufferSize::Unknown => n,
        };
        config.buffer_size = cpal::BufferSize::Fixed(n);
    }
    let sr = config.sample_rate;
    let buffer = match config.buffer_size {
        cpal::BufferSize::Fixed(n) => format!("{n} frames ({:.1} ms)", n as f32 * 1000.0 / sr as f32),
        cpal::BufferSize::Default => "system buffer size".into(),
    };
    let info = format!("{device} · {sr} Hz · {buffer}");
    let (tx, rx) = engine::command_channel();
    let (trash_tx, trash) = engine::trash_channel();
    let pos = Arc::new(AtomicUsize::new(0));
    let meters: Arc<[AtomicU32; METERS]> = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let shared = (rx, trash_tx, pos.clone(), meters.clone());
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, config, shared),
        f => return Err(format!("unsupported sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Audio { _stream: stream, tx, trash, pos, meters, info })
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

/// Typing keyboard as piano: bottom row = C3 octave, top row = C4 octave (before the octave
/// shift).
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
    Chop,
}

const WINDOWS: [(Win, &str, Key); 5] = [
    (Win::Playlist, "▤ Playlist", Key::F5),
    (Win::Rack, "▦ Rack", Key::F6),
    (Win::Roll, "♫ Roll", Key::F7),
    (Win::Chop, "✂ Sampler", Key::F8),
    (Win::Mixer, "☰ Mixer", Key::F9),
];

/// Audio files the sampler can load.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "aif", "aiff"];

/// Tap-to-chop in progress.
struct Tap {
    channel: ChannelId,
    index: u16,
    started: Instant,
    sr: u32,
    len: u32,
    points: Vec<u32>,
}

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
    Sample(Option<PathBuf>),
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
    history: History<Project>,
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
    open: [bool; 5],
    /// Smoothed meter levels for display.
    meters: [f32; METERS],
    /// Playlist selection; a chord clicked in the browser is stamped there.
    sel_track: Option<TrackId>,
    sel_bar: Tick,
    /// Selected playlist clips, a playlist drag in progress, what drags snap to, and the zoom
    /// (pixels per bar).
    sel_clips: Vec<ClipId>,
    clip_drag: Option<windows::ClipDrag>,
    grid: windows::Snap,
    bar_w: f32,
    /// Pattern the Channel Rack's step grid edits.
    pattern: PatternId,
    /// Pattern and channel the Piano Roll edits, and its lowest visible key.
    roll: (PatternId, ChannelId),
    roll_low: u8,
    /// Channel the typing keyboard and the piano-roll keys play.
    live: ChannelId,
    /// Piano roll: selected notes, a drag in progress, what it snaps to, the length new notes
    /// get (the last one drawn or resized), whether it was clicked more recently than the
    /// Playlist (Delete and Ctrl+C/V/X/A act on it then), the copied notes and where the next
    /// paste goes.
    roll_sel: Vec<NoteId>,
    roll_drag: Option<windows::NoteDrag>,
    roll_snap: windows::NoteSnap,
    note_len: Tick,
    roll_focus: bool,
    clipboard: Vec<project::Note>,
    paste_at: Tick,
    /// Octaves the typing keyboard is shifted by (- and = keys), and the notes its held keys
    /// started (released with the same pitch even if the octave changed meanwhile).
    octave: i8,
    held: Vec<(Key, u8)>,
    /// Audio and metronome settings, the settings window's edited copy and the output devices
    /// it lists.
    settings: Settings,
    settings_open: bool,
    draft: Settings,
    devices: Vec<(String, String)>,
    /// Note sounding from a click on the piano-roll keyboard.
    preview: Option<u8>,
    /// Playing tick, if playing.
    cur: Option<Tick>,
    /// Decoded chop samples by path (or why they could not be read).
    samples: HashMap<PathBuf, Result<Loaded, String>>,
    /// Bumped whenever a sample arrives, so the engine gets a new snapshot.
    samples_rev: u64,
    sent_rev: u64,
    loader: (mpsc::Sender<LoadResult>, mpsc::Receiver<LoadResult>),
    /// Samples being loaded to become new chop channels.
    new_chops: Vec<PathBuf>,
    /// The chop channel in the Sampler window, its selected pad, and a chop point being dragged.
    chop_ch: Option<ChannelId>,
    chop_sel: usize,
    chop_drag: Option<usize>,
    /// Auto-chop sensitivity, 0..1.
    sensitivity: f32,
    tap: Option<Tap>,
    /// Commands to send once this frame's snapshot is on its way.
    after_send: Vec<Cmd>,
}

/// A sample path and what loading it gave.
type LoadResult = (PathBuf, Result<Loaded, String>);

/// A decoded sample and its waveform overview for drawing.
pub struct Loaded {
    pub data: Arc<SampleData>,
    pub overview: Vec<[f32; 2]>,
}

/// Points in a sample's waveform overview.
pub const OVERVIEW: usize = 4096;

impl Loaded {
    fn decode(path: &Path) -> Result<Loaded, String> {
        let data = sample::decode(path)?;
        let overview = sample::overview(&data, 0, data.frames.len(), OVERVIEW);
        Ok(Loaded { data: Arc::new(data), overview })
    }
}

impl App {
    fn new(project: Project, path: Option<PathBuf>, audio: Option<Audio>, status: String, settings: Settings) -> Self {
        let mut app = App {
            sel_track: None,
            saved: Project::default(),
            history: History::new(&Project::default()),
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
            open: [true, true, false, false, false],
            meters: [0.0; METERS],
            sel_bar: 0,
            sel_clips: Vec::new(),
            clip_drag: None,
            grid: windows::Snap::Bar,
            bar_w: 140.0,
            pattern: PatternId(0),
            roll: (PatternId(0), ChannelId(0)),
            roll_low: 60,
            live: ChannelId(0),
            roll_sel: Vec::new(),
            roll_drag: None,
            roll_snap: windows::NoteSnap::Step,
            note_len: STEP,
            roll_focus: false,
            clipboard: Vec::new(),
            paste_at: 0,
            octave: 0,
            held: Vec::new(),
            settings: settings.clone(),
            settings_open: false,
            draft: settings,
            devices: Vec::new(),
            preview: None,
            cur: None,
            samples: HashMap::new(),
            samples_rev: 0,
            sent_rev: 0,
            loader: mpsc::channel(),
            new_chops: Vec::new(),
            chop_ch: None,
            chop_sel: 0,
            chop_drag: None,
            sensitivity: 0.5,
            tap: None,
            after_send: Vec::new(),
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
        (self.sel_clips, self.clip_drag) = (Vec::new(), None);
        (self.roll_sel, self.roll_drag) = (Vec::new(), None);
        self.chop_ch = project.channels.iter().find(|c| c.chop.is_some()).map(|c| c.id);
        (self.chop_sel, self.chop_drag) = (0, None);
        self.tap = None;
        self.pattern = pattern;
        self.saved = project.clone();
        self.history.reset(&project);
        self.project = project;
        self.path = path;
        let roll_open = self.is_open(Win::Roll);
        self.edit_in_roll(roll_pat, roll_ch);
        self.open[Win::Roll as usize] = roll_open;
    }

    /// The project as the engine plays it, with the decoded chop samples.
    fn compile(&self) -> Snapshot {
        self.project.compile_with(&|p| self.samples.get(p).and_then(|s| s.as_ref().ok()).map(|s| s.data.clone()))
    }

    /// Starts decoding chop samples the project uses and the cache lacks (on a background
    /// thread), and takes in the ones that finished.
    fn load_samples(&mut self) {
        while let Ok((path, loaded)) = self.loader.1.try_recv() {
            if let Err(e) = &loaded {
                self.status = format!("Could not load {}: {e}", path.display());
            }
            let ok = loaded.is_ok();
            self.samples.insert(path.clone(), loaded);
            self.samples_rev += 1;
            if let Some(i) = self.new_chops.iter().position(|p| *p == path) {
                self.new_chops.remove(i);
                if ok {
                    self.new_chop_channel(&path);
                }
            }
        }
        let missing: Vec<PathBuf> = self.project.channels.iter().filter_map(|c| c.chop.as_ref()).map(|c| c.path.clone()).filter(|p| !self.samples.contains_key(p)).collect();
        for path in missing {
            self.start_loading(path);
        }
    }

    /// The chop channel the Sampler window shows; makes the keyboard play its pads.
    fn select_chop(&mut self, channel: ChannelId) {
        self.chop_ch = Some(channel);
        self.chop_sel = 0;
        self.live = channel;
        self.open[Win::Chop as usize] = true;
    }

    /// The decoded audio of a chop channel, if it has loaded.
    pub fn chop_sample(&self, channel: ChannelId) -> Option<&Loaded> {
        let path = &self.project.chop(channel)?.path;
        self.samples.get(path)?.as_ref().ok()
    }

    fn load_sample_dialog(&mut self) {
        let dir = self.dialog_dir();
        self.file_dialog(move || {
            let pick = rfd::FileDialog::new().set_title("Load a sample to chop").add_filter("Audio", AUDIO_EXTENSIONS).set_directory(dir).pick_file();
            Dialog::Sample(pick)
        });
    }

    /// Makes a new chop channel from the audio at `path`, chopped at its hits, once it decodes.
    fn add_sample(&mut self, path: PathBuf) {
        self.status = format!("Loading {}…", path.display());
        match self.samples.get(&path) {
            Some(Ok(_)) => self.new_chop_channel(&path),
            // Loading already, or failed before: try again.
            _ => {
                self.samples.remove(&path);
                self.new_chops.push(path.clone());
                self.start_loading(path);
            }
        }
    }

    fn start_loading(&mut self, path: PathBuf) {
        self.samples.insert(path.clone(), Err("loading…".into()));
        let tx = self.loader.0.clone();
        std::thread::spawn(move || {
            let loaded = Loaded::decode(&path);
            let _ = tx.send((path, loaded));
        });
    }

    fn new_chop_channel(&mut self, path: &Path) {
        let Some(Ok(loaded)) = self.samples.get(path) else { return };
        let data = loaded.data.clone();
        let name = path.file_stem().map_or("Chop".into(), |s| s.to_string_lossy().into_owned());
        let len = data.frames.len().min(u32::MAX as usize) as u32;
        let Some(ch) = self.project.add_chop_channel(&name, path.to_path_buf(), len) else {
            self.status = "No room for another channel.".into();
            return;
        };
        let points = sample::detect_hits(&data, self.sensitivity, MAX_SLICES);
        self.project.set_chop_points(ch, &points);
        self.status = format!("Chopped {name} into {} pads at its hits — play them with Z S X D C V…", points.len());
        self.select_chop(ch);
    }

    /// Tap to chop: plays the whole sample; each tap (Enter or the Tap button) starts a new pad.
    fn start_tap(&mut self, channel: ChannelId) {
        let Some(sr) = self.chop_sample(channel).map(|l| l.data.sr) else { return };
        self.project.set_chop_points(channel, &[0]);
        if let Some(s) = self.project.slice_mut(channel, 0) {
            (s.pitch, s.reverse) = (0.0, false);
        }
        let len = self.project.chop(channel).map_or(0, |c| c.len);
        let index = self.project.channel_index(channel).map_or(u16::MAX, |i| i as u16);
        // After the snapshot with the single whole-sample slice reaches the engine.
        self.after_send.push(Cmd::NoteOn { channel: index, pitch: CHOP_BASE });
        self.tap = Some(Tap { channel, index, started: Instant::now(), sr, len, points: Vec::new() });
        self.status = "Tap Enter (or the Tap button) where each chop starts…".into();
    }

    fn tap(&mut self) {
        if let Some(t) = &mut self.tap {
            let at = (t.started.elapsed().as_secs_f64() * t.sr as f64) as u32;
            if at < t.len && t.points.len() < MAX_SLICES {
                t.points.push(at);
            }
        }
    }

    /// Ends tapping (also when the sample has played through) and chops at the taps.
    fn finish_tap(&mut self) {
        let Some(t) = self.tap.take() else { return };
        self.send(Cmd::NoteOff { channel: t.index, pitch: CHOP_BASE });
        // The sound before the first tap stays as pad 1; each tap starts the next pad.
        let points: Vec<u32> = std::iter::once(0).chain(t.points).collect();
        self.project.set_chop_points(t.channel, &points);
        let pads = self.project.chop_points(t.channel).len();
        self.status = format!("{pads} pads from your taps");
    }

    fn undo(&mut self) {
        if self.history.undo(&mut self.project) {
            self.fix_selection();
            self.status = "Undo".into();
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.project) {
            self.fix_selection();
            self.status = "Redo".into();
        }
    }

    /// After an undo or a delete: anything selected that no longer exists falls back to
    /// something that does.
    fn fix_selection(&mut self) {
        let p = &self.project;
        let first_pattern = p.patterns.first().map_or(PatternId(0), |x| x.id);
        let first_channel = p.channels.first().map_or(ChannelId(0), |c| c.id);
        if p.pattern(self.pattern).is_none() {
            self.pattern = first_pattern;
        }
        if p.pattern(self.roll.0).is_none() {
            self.roll.0 = first_pattern;
        }
        if p.channel(self.roll.1).is_none() {
            self.roll.1 = p.channels.iter().find(|c| !c.instrument.is_drum()).map_or(first_channel, |c| c.id);
        }
        if p.channel(self.live).is_none() {
            self.live = self.roll.1;
        }
        if self.chop_ch.is_some_and(|c| p.chop(c).is_none()) {
            self.chop_ch = p.channels.iter().find(|c| c.chop.is_some()).map(|c| c.id);
        }
        let pads = self.chop_ch.and_then(|c| p.chop(c)).map_or(1, |c| c.slices.len());
        self.chop_sel = self.chop_sel.min(pads.saturating_sub(1));
        if self.sel_track.is_some_and(|t| p.track(t).is_none()) {
            self.sel_track = p.tracks.first().map(|t| t.id);
        }
        self.sel_clips.retain(|&id| p.clips.iter().any(|c| c.id == id));
        self.clip_drag = None;
        let notes = p.pattern(self.roll.0).map_or(&[][..], |x| &x.notes[..]);
        self.roll_sel.retain(|&id| notes.iter().any(|n| n.id == id && n.channel == self.roll.1));
        self.roll_drag = None;
        if self.tap.as_ref().is_some_and(|t| p.chop(t.channel).is_none()) {
            self.tap = None;
        }
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
            Dialog::Sample(Some(path)) => self.add_sample(path),
            Dialog::SaveAs(Some(path), then) => {
                if self.write(&path) {
                    self.run(ctx, then);
                }
            }
            // Also what comes back when the desktop has no file chooser at all.
            Dialog::Open(None) | Dialog::Sample(None) | Dialog::SaveAs(None, _) => {
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
        // Undo and redo, unless a text field is being edited (it has its own undo).
        // Shift first: Ctrl+Z also matches Ctrl+Shift+Z (and Ctrl+S matches Ctrl+Shift+S).
        if !ctx.egui_wants_keyboard_input() {
            if hit(M::COMMAND | M::SHIFT, Key::Z) || hit(M::COMMAND, Key::Y) {
                self.redo();
            } else if hit(M::COMMAND, Key::Z) {
                self.undo();
            }
            // Piano roll: select all, duplicate, copy, cut, paste.
            if self.roll_focused() {
                if hit(M::COMMAND, Key::A) {
                    let (pat, ch) = self.roll;
                    self.roll_sel = self.project.pattern(pat).map_or(Vec::new(), |p| p.notes.iter().filter(|n| n.channel == ch).map(|n| n.id).collect());
                } else if hit(M::COMMAND, Key::D) {
                    let notes = self.roll_selection();
                    if !notes.is_empty() {
                        self.clipboard = notes;
                        self.paste_at = self.clipboard_span().1;
                        self.paste_notes();
                    }
                }
                for ev in ctx.input(|i| i.events.clone()) {
                    match ev {
                        egui::Event::Copy => self.copy_notes(ctx, false),
                        egui::Event::Cut => self.copy_notes(ctx, true),
                        egui::Event::Paste(_) => self.paste_notes(),
                        _ => {}
                    }
                }
            }
        }
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

    fn set_metronome(&mut self, on: bool) {
        self.settings.metronome = on;
        self.draft.metronome = on;
        self.send(Cmd::Metronome(on));
        self.save_settings();
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.status = format!("Couldn't save settings: {e}");
        }
    }

    /// Reopens the audio output with `new`; if that fails, goes back to the previous output.
    fn restart_audio(&mut self, new: Settings) {
        self.set_playing(false);
        // Close the old stream first: some devices take one stream at a time.
        self.audio = None;
        match start_audio(&new) {
            Ok(a) => {
                self.status = format!("Audio: {}", a.info);
                self.audio = Some(a);
                self.settings = new;
                self.save_settings();
            }
            Err(e) => {
                self.status = format!("Couldn't open that output ({e}); back to the previous one.");
                self.audio = start_audio(&self.settings).ok();
            }
        }
        self.draft = self.settings.clone();
        // The new engine starts empty: send it the song and the metronome.
        self.sent = None;
        self.send(Cmd::Metronome(self.settings.metronome));
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Audio Settings").open(&mut open).resizable(false).collapsible(false).default_pos(egui::pos2(420.0, 140.0)).show(ctx, |ui| {
            let current = self.audio.as_ref().map_or("No audio output".to_string(), |a| a.info.clone());
            ui.label(RichText::new(current).small().weak());
            ui.add_space(6.0);
            egui::Grid::new("audio-settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Output");
                let name = |id: &Option<String>| match id {
                    None => "System default".to_string(),
                    Some(id) => self.devices.iter().find(|(d, _)| d == id).map_or(id.clone(), |(_, n)| n.clone()),
                };
                let selected = name(&self.draft.device);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("audio-device")
                        .width(280.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.draft.device, None, "System default");
                            for (id, n) in &self.devices {
                                ui.selectable_value(&mut self.draft.device, Some(id.clone()), n);
                            }
                        })
                        .response
                        .on_hover_text("Where Ritmo plays its sound");
                    if ui.small_button("⟳").on_hover_text("Look for output devices again").clicked() {
                        self.devices = output_devices();
                    }
                });
                ui.end_row();
                ui.label("Buffer");
                let text = |b: Option<u32>| b.map_or("System default".to_string(), |n| format!("{n} frames"));
                egui::ComboBox::from_id_salt("audio-buffer")
                    .selected_text(text(self.draft.buffer))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.draft.buffer, None, text(None));
                        for n in settings::BUFFERS {
                            ui.selectable_value(&mut self.draft.buffer, Some(n), text(Some(n)));
                        }
                    })
                    .response
                    .on_hover_text("Smaller buffers make the keyboard and pads respond faster but need more of the computer. If the sound crackles, pick a bigger one.");
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let changed = self.draft.device != self.settings.device || self.draft.buffer != self.settings.buffer;
                if ui.add_enabled(changed, egui::Button::new("Apply")).on_hover_text("Reopen the audio output with these settings").clicked() {
                    self.restart_audio(self.draft.clone());
                }
                if ui.button("Restart audio").on_hover_text("Reopen the audio output, e.g. after plugging in headphones").clicked() {
                    self.restart_audio(self.settings.clone());
                }
            });
        });
        self.settings_open = open;
    }

    fn is_open(&self, w: Win) -> bool {
        self.open[w as usize]
    }

    fn live_is_chop(&self) -> bool {
        self.project.channel(self.live).is_some_and(|c| c.instrument == Instrument::Chop)
    }

    /// Whether Delete, arrows and Ctrl+A/C/X/V/D act on the piano roll's notes (it was clicked
    /// more recently than the Playlist and is open).
    fn roll_focused(&self) -> bool {
        self.roll_focus && self.is_open(Win::Roll)
    }

    /// The selected notes of the piano roll, earliest first.
    fn roll_selection(&self) -> Vec<project::Note> {
        let mut notes: Vec<project::Note> = self.project.pattern(self.roll.0).into_iter().flat_map(|p| p.notes.iter().filter(|n| self.roll_sel.contains(&n.id)).copied()).collect();
        notes.sort_by_key(|n| (n.start, n.pitch));
        notes
    }

    /// The span the clipboard's notes fill, in whole bars: pasting steps by it, so pasted notes
    /// keep their place in the bar.
    fn clipboard_span(&self) -> (Tick, Tick) {
        let bl = self.project.bar_len();
        let first = self.clipboard.iter().map(|n| n.start).min().unwrap_or(0) / bl * bl;
        let last = self.clipboard.iter().map(|n| n.end()).max().unwrap_or(0).div_ceil(bl) * bl;
        (first, last)
    }

    /// Copies (or cuts) the selected notes. A paste then goes right after them (after a cut: back
    /// where they were).
    fn copy_notes(&mut self, ctx: &egui::Context, cut: bool) {
        let notes = self.roll_selection();
        if notes.is_empty() {
            return;
        }
        self.clipboard = notes;
        let (first, last) = self.clipboard_span();
        self.paste_at = if cut { first } else { last };
        if cut {
            self.project.remove_notes(self.roll.0, &self.roll_sel);
            self.roll_sel.clear();
        }
        // Ctrl+V reaches the app only while the system clipboard holds text.
        ctx.copy_text(format!("{} notes from Ritmo", self.clipboard.len()));
        self.status = format!("{} {} notes", if cut { "Cut" } else { "Copied" }, self.clipboard.len());
    }

    /// Pastes the copied notes into the piano roll's pattern and channel, each paste after the
    /// last; the pasted notes become the selection.
    fn paste_notes(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let (first, last) = self.clipboard_span();
        let earliest = self.clipboard.iter().map(|n| n.start).min().unwrap_or(0);
        let (pat, ch) = self.roll;
        self.roll_sel = self.project.paste_notes(pat, ch, &self.clipboard, self.paste_at + earliest - first);
        let bl = self.project.bar_len();
        self.status = format!("Pasted {} notes at bar {}", self.roll_sel.len(), (self.paste_at + earliest - first) / bl + 1);
        self.paste_at += last - first;
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
        let inst = self.project.channel(channel).map(|c| c.instrument);
        let default = match inst {
            Some(Instrument::Bass) => 36,
            Some(Instrument::Chop) => CHOP_BASE,
            _ => 60,
        };
        // Chop channels show pads 1-16 from the bottom row.
        let low = if inst == Some(Instrument::Chop) { CHOP_BASE } else { lowest.map_or(default, |n| n - n % 12) };
        self.roll_low = low.min(127 - windows::ROLL_KEYS as u8 + 1);
        self.open[Win::Roll as usize] = true;
    }

    /// Opens the piano roll on the pattern `channel` plays most from (or the rack's pattern).
    fn edit_channel(&mut self, channel: ChannelId) {
        let count = |p: &&project::Pattern| p.notes.iter().filter(|n| n.channel == channel).count();
        let pattern = self.project.patterns.iter().filter(|p| count(p) > 0).max_by_key(count).map_or(self.pattern, |p| p.id);
        self.edit_in_roll(pattern, channel);
    }

    /// Puts chord `j` on playlist bar `bar` of `track` (see `Project::stamp_chord_on_bar`).
    fn stamp_chord(&mut self, track: TrackId, bar: Tick, j: usize) {
        self.status = match self.project.stamp_chord_on_bar(track, bar, j) {
            Ok(()) => return,
            Err(ChordError::NoClip) => "Pick a bar that has a clip to put the chord in.".into(),
            Err(ChordError::NoKeys) => "Chords need an electric-piano channel in the Channel Rack.".into(),
        };
    }

    /// Exports the whole song, whatever the loop range.
    fn export(&mut self) {
        let mut snap = self.compile();
        (snap.loop_start, snap.loop_end) = self.project.song_range();
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
            if self.tap.is_some() && pressed && (key == Key::Enter || key == Key::Escape) {
                if key == Key::Enter { self.tap() } else { self.finish_tap() }
            } else if pressed && matches!(key, Key::Delete | Key::Backspace) {
                if self.roll_focused() {
                    self.project.remove_notes(self.roll.0, &self.roll_sel);
                    self.roll_sel.clear();
                } else {
                    self.project.remove_clips(&self.sel_clips);
                    self.sel_clips.clear();
                }
            } else if pressed && key == Key::Escape {
                // Cancels a drag, else clears the selection.
                if self.roll_focused() {
                    if self.roll_drag.take().is_none() {
                        self.roll_sel.clear();
                    }
                } else if self.clip_drag.take().is_none() {
                    self.sel_clips.clear();
                }
            } else if pressed && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight) {
                // Arrows move the selected notes: a semitone (Shift: an octave) or a snap step.
                if self.roll_focused() {
                    let (dt, dp) = match key {
                        Key::ArrowUp => (0, if modifiers.shift { 12 } else { 1 }),
                        Key::ArrowDown => (0, if modifiers.shift { -12 } else { -1 }),
                        Key::ArrowLeft => (-i64::from(self.roll_snap.ticks()), 0),
                        _ => (i64::from(self.roll_snap.ticks()), 0),
                    };
                    self.project.shift_notes(self.roll.0, &self.roll_sel, dt, dp, false);
                }
            } else if pressed && matches!(key, Key::Minus | Key::Equals) {
                self.octave = (self.octave + if key == Key::Equals { 1 } else { -1 }).clamp(-3, 3);
                self.status = format!("Keyboard octave {:+}", self.octave);
            } else if let Some(&(w, _, _)) = WINDOWS.iter().find(|(_, _, k)| *k == key) {
                if pressed {
                    self.open[w as usize] ^= true;
                }
            } else if key == Key::Space && pressed {
                self.set_playing(!self.playing);
            } else if let Some(pitch) = key_note(key) {
                let channel = self.live_index();
                if pressed {
                    // Chop pads stay on their keys; other instruments follow the octave shift.
                    let shift = if self.live_is_chop() { 0 } else { 12 * i32::from(self.octave) };
                    let pitch = (i32::from(pitch) + shift).clamp(0, 127) as u8;
                    self.held.push((key, pitch));
                    self.send(Cmd::NoteOn { channel, pitch });
                } else if let Some(i) = self.held.iter().position(|&(k, _)| k == key) {
                    let (_, pitch) = self.held.remove(i);
                    self.send(Cmd::NoteOff { channel, pitch });
                }
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("◆ RITMO").strong().size(18.0).color(ACCENT));
            self.file_menu(ui);
            if ui.add_enabled(self.history.can_undo(&self.project), egui::Button::new("↶")).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("↷")).on_hover_text("Redo (Ctrl+Shift+Z or Ctrl+Y)").clicked() {
                self.redo();
            }
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
            if ui.add(egui::Button::selectable(self.settings.metronome, "♩ Click")).on_hover_text("Metronome: a click on every beat while playing (not exported)").clicked() {
                self.set_metronome(!self.settings.metronome);
            }
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
            if ui.add(egui::Button::selectable(self.settings_open, "⚙ Audio")).on_hover_text("Audio settings: output device and buffer size").clicked() {
                self.settings_open ^= true;
                if self.settings_open {
                    self.draft = self.settings.clone();
                    self.devices = output_devices();
                }
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
                    let hint = match inst {
                        _ if inst.is_drum() => "Synth drum · click to play it from the keyboard",
                        Instrument::Chop => "Chop sampler · click to open it in the Sampler",
                        _ => "Click to edit in the piano roll",
                    };
                    if ui.selectable_label(self.live == id, text).on_hover_text(hint).clicked() {
                        if inst.is_drum() {
                            self.live = id;
                            self.open[Win::Rack as usize] = true;
                        } else if inst == Instrument::Chop {
                            self.select_chop(id);
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
                if ui.small_button("＋ New pattern").on_hover_text("Add an empty one-bar pattern").clicked() {
                    self.pattern = self.project.new_pattern();
                    self.open[Win::Rack as usize] = true;
                }
                let (mut clone, mut delete) = (None, None);
                let only_one = self.project.patterns.len() <= 1;
                for i in 0..self.project.patterns.len() {
                    let p = &self.project.patterns[i];
                    let resp = ui
                        .selectable_label(self.pattern == p.id, RichText::new(&p.name).color(pattern_color(i)))
                        .on_hover_text("Click: edit its steps in the Channel Rack · double-click: piano roll · right-click: rename, clone, delete");
                    if resp.double_clicked() {
                        let ch = p.notes.iter().map(|n| n.channel).find(|&c| self.project.channel(c).is_some_and(|c| !c.instrument.is_drum()));
                        let (pid, ch) = (p.id, ch.unwrap_or(self.roll.1));
                        self.edit_in_roll(pid, ch);
                    } else if resp.clicked() {
                        self.pattern = p.id;
                        self.open[Win::Rack as usize] = true;
                    }
                    let id = self.project.patterns[i].id;
                    edit_menu(&resp, |ui| {
                        ui.label(RichText::new("Name").small().weak());
                        ui.text_edit_singleline(&mut self.project.patterns[i].name);
                        if ui.button("⧉ Clone").clicked() {
                            clone = Some(id);
                            ui.close();
                        }
                        if ui.add_enabled(!only_one, egui::Button::new("🗑 Delete pattern and its clips")).on_disabled_hover_text("A project keeps at least one pattern").clicked() {
                            delete = Some(id);
                            ui.close();
                        }
                    });
                }
                if let Some(id) = clone.and_then(|id| self.project.clone_pattern(id)) {
                    self.pattern = id;
                }
                if let Some(id) = delete {
                    self.project.remove_pattern(id);
                    self.fix_selection();
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
        // Audio files dropped on the window become chop channels.
        for f in ctx.input(|i| i.raw.dropped_files.clone()) {
            let path = f.path();
            if path.extension().is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str())) {
                self.add_sample(path.to_path_buf());
            }
        }
        if self.tap.as_ref().is_some_and(|t| t.started.elapsed().as_secs_f64() * t.sr as f64 >= t.len as f64) {
            self.finish_tap();
        }
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
                    let oct = |base: i8| base + self.octave;
                    let keys = if self.live_is_chop() { "Z–3 = pads 1–16".to_string() } else { format!("Z–M = C{} · Q–P = C{} (−/= octave)", oct(3), oct(4)) };
                    ui.label(RichText::new(format!("Keys play {live}: {keys} · Space play/stop · F5 Playlist · F6 Rack · F7 Piano Roll · F8 Sampler · F9 Mixer")).small().weak());
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
        if self.settings_open {
            self.settings_window(&ctx);
        }
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
        // One undo step per settled edit: not while a button is held, a scroll is running or a
        // text field has focus.
        let settled = !ctx.input(|i| i.pointer.any_down() || i.smooth_scroll_delta != egui::Vec2::ZERO) && !ctx.egui_wants_keyboard_input();
        self.history.observe(&self.project, settled);
        self.load_samples();
        if self.sent.as_ref() != Some(&self.project) || self.sent_rev != self.samples_rev {
            // Debug builds check that every edit keeps the project valid.
            if let Err(e) = self.project.validate() {
                debug_assert!(false, "edit broke the project: {e}");
            }
            let snap = self.compile();
            if self.send(Cmd::Snapshot(Box::new(snap.clone()))) || self.audio.is_none() {
                self.snap = snap;
                self.sent = Some(self.project.clone());
                self.sent_rev = self.samples_rev;
            }
        }
        for cmd in std::mem::take(&mut self.after_send) {
            self.send(cmd);
        }
        if self.playing || self.export_rx.is_some() || self.meters.iter().any(|&m| m > 1e-3) {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.dialog_rx.is_some() || self.tap.is_some() || !self.new_chops.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(AUTOSAVE_EVERY);
        }
    }
}

fn main() -> eframe::Result {
    let settings = Settings::load();
    let (audio, status) = match start_audio(&settings) {
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
            let mut app = App::new(Project::demo(), None, audio, status, settings);
            app.send(Cmd::Metronome(app.settings.metronome));
            if let Some(path) = std::env::args_os().nth(1) {
                app.open_file(PathBuf::from(path));
            }
            Ok(Box::new(app))
        }),
    )
}
