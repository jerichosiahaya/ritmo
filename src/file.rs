//! Project files (`.ritmo`): versioned JSON. Saving writes a temporary file and renames it over
//! the old one, so a crash never leaves half a file. Opening upgrades older format versions and
//! checks the project with `Project::validate`.

use crate::project::Project;
use serde_json::Value;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const EXTENSION: &str = "ritmo";
const FORMAT: &str = "ritmo";

/// File format version this build writes. When a saved field changes shape or meaning: bump it,
/// add an upgrade step to `migrate`, and run `cargo test -- --ignored write_fixture` to add a
/// fixture under `tests/files/` that every later version must keep opening.
pub const VERSION: u64 = 2;

#[derive(Debug)]
pub enum FileError {
    Io(std::io::Error),
    NotRitmo,
    TooNew(u64),
    Broken(String),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FileError::Io(e) => write!(f, "{e}"),
            FileError::NotRitmo => write!(f, "not a Ritmo project file"),
            FileError::TooNew(v) => write!(f, "made by a newer Ritmo (file format {v}; this version opens up to {VERSION})"),
            FileError::Broken(why) => write!(f, "the file is damaged: {why}"),
        }
    }
}

impl From<std::io::Error> for FileError {
    fn from(e: std::io::Error) -> Self {
        FileError::Io(e)
    }
}

/// What a file holds, in the order written.
#[derive(serde::Serialize)]
struct Doc<'a> {
    format: &'static str,
    version: u64,
    project: &'a Project,
}

pub fn to_string(p: &Project) -> String {
    // Every field is a number, string, bool, list or struct, so this cannot fail.
    serde_json::to_string_pretty(&Doc { format: FORMAT, version: VERSION, project: p }).expect("projects always serialize")
}

pub fn from_str(text: &str) -> Result<Project, FileError> {
    let mut doc: Value = serde_json::from_str(text).map_err(|e| FileError::Broken(e.to_string()))?;
    if doc.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(FileError::NotRitmo);
    }
    let version = doc.get("version").and_then(Value::as_u64).ok_or_else(|| FileError::Broken("no format version".into()))?;
    if version > VERSION {
        return Err(FileError::TooNew(version));
    }
    migrate(&mut doc, version)?;
    let project: Project = serde_json::from_value(doc["project"].take()).map_err(|e| FileError::Broken(e.to_string()))?;
    project.validate().map_err(FileError::Broken)?;
    Ok(project)
}

/// Upgrades a document saved in format `from` to `VERSION`, one step per version.
///
/// - 1 → 2: channels gained optional chop settings (`chop`); version 1 files have none, so
///   nothing needs rewriting.
fn migrate(_doc: &mut Value, from: u64) -> Result<(), FileError> {
    if from < 1 {
        return Err(FileError::Broken(format!("unknown file format {from}")));
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<Project, FileError> {
    from_str(&fs::read_to_string(path)?)
}

/// Writes next to `path` first, then renames over it.
pub fn save(path: &Path, p: &Project) -> Result<(), FileError> {
    let name = path.file_name().ok_or_else(|| FileError::Io(std::io::Error::other("no file name")))?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let written = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(to_string(p).as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// Adds the `.ritmo` extension if the path has none.
pub fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() { path } else { path.with_extension(EXTENSION) }
}

/// Where autosaves go: `$XDG_DATA_HOME/ritmo/autosave.ritmo`, else `~/.local/share/ritmo/…`.
pub fn autosave_path() -> Option<PathBuf> {
    data_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME")).map(|d| d.join("ritmo").join(format!("autosave.{EXTENSION}")))
}

fn data_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The XDG spec says to ignore relative paths.
    let xdg = xdg.map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.or_else(|| home.map(|h| PathBuf::from(h).join(".local/share")))
}

pub fn autosave(p: &Project) -> Result<PathBuf, FileError> {
    let path = autosave_path().ok_or_else(|| FileError::Io(std::io::Error::other("no home directory")))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    save(&path, p)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{CHOP_BASE, ChannelId, PatternId, STEP};

    /// The demo plus a chop channel (its audio file need not exist) playing pad 2.
    fn with_chops() -> Project {
        let mut p = Project::demo();
        let ch = p.add_chop_channel("Break", PathBuf::from("samples/break.wav"), 88200).unwrap();
        p.set_chop_points(ch, &[0, 22050, 44100, 66150]);
        p.slice_mut(ch, 1).unwrap().pitch = 3.0;
        p.slice_mut(ch, 2).unwrap().reverse = true;
        let beat = p.patterns[0].id;
        p.add_note(beat, ch, CHOP_BASE + 1, 0, STEP, 100);
        p
    }

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/files")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ritmo-{name}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn projects_round_trip() {
        let mut edited = Project::demo();
        let pat = edited.patterns[0].id;
        let ch = edited.channels[0].id;
        edited.toggle_step(pat, ch, 5);
        edited.loop_range = Some((0, 384));
        edited.tracks[1].mute = true;
        edited.channels[2].pan = -0.37;
        for p in [Project::demo(), Project::blank(), Project::default(), with_chops(), edited] {
            let text = to_string(&p);
            let back = from_str(&text).unwrap();
            assert_eq!(back, p);
            // IDs keep counting from where they were.
            let mut b = back;
            let new = b.add_pattern("New", 96);
            assert!(p.patterns.iter().all(|x| x.id < new));
        }
    }

    #[test]
    fn file_says_what_it_is() {
        let doc: Value = serde_json::from_str(&to_string(&Project::demo())).unwrap();
        assert_eq!((doc["format"].as_str(), doc["version"].as_u64()), (Some("ritmo"), Some(VERSION)));
        assert_eq!(doc["project"]["bpm"].as_f64(), Some(80.0));
    }

    #[test]
    fn bad_files_give_clear_errors() {
        let good: Value = serde_json::from_str(&to_string(&Project::demo())).unwrap();
        let with = |f: &dyn Fn(&mut Value)| {
            let mut d = good.clone();
            f(&mut d);
            from_str(&d.to_string()).unwrap_err()
        };
        assert!(matches!(from_str("{ not json"), Err(FileError::Broken(_))));
        assert!(matches!(from_str("{}"), Err(FileError::NotRitmo)));
        assert!(matches!(with(&|d| d["format"] = "other".into()), FileError::NotRitmo));
        assert!(matches!(with(&|d| d["version"] = (VERSION + 1).into()), FileError::TooNew(v) if v == VERSION + 1));
        assert!(matches!(with(&|d| d["version"] = 0.into()), FileError::Broken(_)));
        assert!(matches!(with(&|d| d["version"] = Value::Null), FileError::Broken(_)));
        assert!(matches!(with(&|d| d["project"]["bpm"] = "fast".into()), FileError::Broken(_)));
        // Parses, but the project is invalid: a clip pointing at a missing pattern, IDs the
        // counter never handed out.
        assert!(matches!(with(&|d| d["project"]["clips"][0]["pattern"] = 9999.into()), FileError::Broken(_)));
        assert!(matches!(with(&|d| d["project"]["next_id"] = 1.into()), FileError::Broken(_)));
        let msg = with(&|d| d["version"] = 99.into()).to_string();
        assert!(msg.contains("newer Ritmo"), "{msg}");
    }

    #[test]
    fn save_replaces_the_file_and_leaves_no_temp() {
        let dir = temp_dir("save");
        let path = dir.join("song.ritmo");
        save(&path, &Project::blank()).unwrap();
        save(&path, &Project::demo()).unwrap();
        assert_eq!(load(&path).unwrap(), Project::demo());
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["song.ritmo"]);
        assert!(matches!(load(&dir.join("missing.ritmo")), Err(FileError::Io(_))));
        assert!(save(&dir.join("no-such-dir/x.ritmo"), &Project::demo()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paths() {
        assert_eq!(with_extension("/a/song".into()), PathBuf::from("/a/song.ritmo"));
        assert_eq!(with_extension("/a/song.ritmo".into()), PathBuf::from("/a/song.ritmo"));
        assert_eq!(data_dir(Some("/x/data".into()), Some("/home/u".into())), Some(PathBuf::from("/x/data")));
        assert_eq!(data_dir(Some("relative".into()), Some("/home/u".into())), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(data_dir(None, None), None);
    }

    /// Every fixture ever written must keep opening, whatever version made it.
    #[test]
    fn old_files_still_open() {
        let mut seen = 0;
        for entry in fs::read_dir(fixtures()).unwrap() {
            let path = entry.unwrap().path();
            let p = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!p.compile().notes.is_empty(), "{}", path.display());
            seen += 1;
        }
        assert!(seen >= 1);
        // The first format's demo, read back exactly.
        let v1 = load(&fixtures().join("format-1-demo.ritmo")).unwrap();
        assert_eq!(v1.compile().notes.len(), 13 * 4 + 20 + 6);
        assert!(v1.step(PatternId(11), ChannelId(5), 0) && v1.bar_len() == 16 * STEP);
        // Format 2 added chop channels.
        let v2 = load(&fixtures().join("format-2-chops.ritmo")).unwrap();
        let chop = v2.channels.iter().find_map(|c| c.chop.as_ref()).unwrap();
        assert_eq!((chop.slices.len(), chop.slices[1].pitch, chop.slices[2].reverse), (4, 3.0, true));
        // Files without chops don't mention them.
        assert!(!to_string(&Project::demo()).contains("chop"));
    }

    /// Writes the fixture for the current format version. Run after bumping `VERSION`.
    #[test]
    #[ignore = "writes tests/files/format-<VERSION>-chops.ritmo"]
    fn write_fixture() {
        // Version 1's fixture is format-1-demo.ritmo (the demo song alone).
        let path = fixtures().join(format!("format-{VERSION}-chops.{EXTENSION}"));
        fs::create_dir_all(fixtures()).unwrap();
        save(&path, &with_chops()).unwrap();
    }
}
