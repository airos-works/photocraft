//! The displays and their colour profiles (`Services::read_displays`, Edit › Color Settings ›
//! Monitor Profile = `auto`), and the screen colour picker's conversion to sRGB.
//!
//! Neither Haiku nor air/OS keeps a colour profile for a screen: app_server has no colour
//! management and the Screen preferences none either. air/OS's display layout knows every
//! monitor (its connector, its EDID name, vendor and serial number, and where it is on the
//! desktop); plain Haiku has one screen and `BScreen::GetMonitorInfo`. So the profile of a
//! monitor is a file the user puts in `~/config/settings/ColorProfiles/` (made at the first
//! read, so it can be found), named after the monitor; the first of these that exists is used
//! (letter case and the `.icc`/`.icm` extension don't matter, `/` in a name becomes `-`):
//!
//! 1. `<monitor name> <serial number>.icc`, for one of two identical monitors;
//! 2. `<monitor name>.icc` (the name in the Screen preferences, e.g. `DELL U2720Q.icc`);
//! 3. `<connector>.icc` (e.g. `DP-2.icc`), for a monitor without EDID;
//! 4. `default.icc`, for every monitor without one of its own.
//!
//! A symbolic link to a profile elsewhere works too. The other Craft apps read the same folder
//! (LightCraft's Choose Monitor Profile panel starts there).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use photocraft_engine::display_color::Display;

/// The folder monitor profiles are put in: `~/config/settings/ColorProfiles`.
pub fn profiles_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).unwrap_or_else(|| "/boot/home".into());
    Some(PathBuf::from(home).join("config/settings/ColorProfiles"))
}

/// Larger files are no monitor profile.
const MAX_PROFILE_BYTES: u64 = 4 << 20;

/// One display as the host lists it (`pc_host_displays`).
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub id: u32,
    /// Left, top, right, bottom (exclusive), in the units window frames use.
    pub frame: [f64; 4],
    pub primary: bool,
    pub connector: String,
    pub name: String,
    pub vendor: String,
    pub serial: String,
}

/// The host's lines: id, left, top, right, bottom, primary, connector, monitor name, vendor,
/// serial number (tab-separated). Malformed lines are skipped.
pub fn parse(text: &str) -> Vec<Monitor> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let num = |i: usize| f.get(i).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
            let text = |i: usize| f.get(i).map_or(String::new(), |v| v.trim().to_string());
            let frame = [num(1)?, num(2)?, num(3)?, num(4)?];
            (frame[2] > frame[0] && frame[3] > frame[1]).then(|| Monitor {
                id: f.first().and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0),
                frame,
                primary: num(5) == Some(1.0),
                connector: text(6),
                name: text(7),
                vendor: text(8),
                serial: text(9),
            })
        })
        .collect()
}

/// The file names (without extension) that give `m` its profile, in order of preference.
pub fn profile_names(m: &Monitor) -> Vec<String> {
    let clean = |s: &str| s.replace('/', "-").trim().to_string();
    let mut names = Vec::new();
    if !m.name.is_empty() && !m.serial.is_empty() {
        names.push(clean(&format!("{} {}", m.name, m.serial)));
    }
    if !m.name.is_empty() {
        names.push(clean(&m.name));
    }
    if !m.connector.is_empty() {
        names.push(clean(&m.connector));
    }
    names.push("default".into());
    names
}

/// The profile file in `dir` for `m`, if there is one.
pub fn profile_file(dir: &Path, m: &Monitor) -> Option<PathBuf> {
    let files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            (ext == "icc" || ext == "icm").then(|| Some((path.file_stem()?.to_str()?.to_lowercase(), path.clone())))?
        })
        .collect();
    profile_names(m).iter().find_map(|name| {
        let name = name.to_lowercase();
        // Sorted, so the choice doesn't depend on the directory's order (`x.icc` before `x.icm`).
        let mut found: Vec<&PathBuf> = files.iter().filter(|(stem, _)| *stem == name).map(|(_, p)| p).collect();
        found.sort();
        found.first().map(|p| (*p).clone())
    })
}

/// The display's name in Color Settings: the monitor's own, or its connector.
fn display_name(m: &Monitor, all: &[Monitor]) -> String {
    let base = if !m.name.is_empty() {
        m.name.clone()
    } else if !m.connector.is_empty() {
        m.connector.clone()
    } else {
        format!("Display {}", m.id)
    };
    let same = all.iter().filter(|o| o.name == m.name && !m.name.is_empty()).count() > 1;
    if same && !m.connector.is_empty() { format!("{base} ({})", m.connector) } else { base }
}

/// The displays for the canvas, primary first, from the host's list and the profiles in `dir`.
/// Frames are divided by `ui_scale`: the host's points are the window's logical pixels over the
/// interface scale (see `Shell::viewport_info`).
pub fn read(text: &str, dir: Option<&Path>, ui_scale: f32) -> Result<Vec<Display>, String> {
    let mut monitors = parse(text);
    if monitors.is_empty() {
        return Err("the system reported no displays".into());
    }
    monitors.sort_by_key(|m| !m.primary);
    let s = f64::from(if ui_scale.is_finite() && ui_scale > 0.0 { ui_scale } else { 1.0 });
    Ok(monitors
        .iter()
        .map(|m| {
            let file = dir.and_then(|d| profile_file(d, m));
            let icc = file
                .as_ref()
                .filter(|f| std::fs::metadata(f).is_ok_and(|md| md.len() <= MAX_PROFILE_BYTES))
                .and_then(|f| std::fs::read(f).map_err(|e| log::warn!(target: "photocraft_haiku", "monitor profile {}: {e}", f.display())).ok())
                .map(Arc::new);
            let [l, t, r, b] = m.frame;
            Display {
                id: m.id,
                name: display_name(m, &monitors),
                frame: [l / s, t / s, (r - l) / s, (b - t) / s],
                profile_name: file.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()),
                icc,
            }
        })
        .collect())
}

/// The profile of the display at a screen point (the host's units), for the screen picker.
pub fn profile_at(text: &str, dir: Option<&Path>, x: f64, y: f64) -> Option<Vec<u8>> {
    let monitors = parse(text);
    let m = monitors.iter().find(|m| x >= m.frame[0] && x < m.frame[2] && y >= m.frame[1] && y < m.frame[3])?;
    let file = profile_file(dir?, m)?;
    std::fs::metadata(&file).ok().filter(|md| md.len() <= MAX_PROFILE_BYTES)?;
    std::fs::read(file).ok()
}

/// A screen pixel as sRGB: through the display's profile when it has a usable RGB one, as the
/// interface's own picker converts its snapshots (`ScreenImage::conversion`).
pub fn to_srgb(rgb: [u8; 3], profile: Option<&[u8]>) -> Result<[f32; 3], String> {
    let mut v = rgb.map(|c| f32::from(c) / 255.0);
    if let Some(bytes) = profile {
        let source = photocraft_cms::Profile::parse(bytes).map_err(|e| e.to_string())?;
        if source.color_space != photocraft_cms::ColorSpace::Rgb {
            return Err("Screen profile must describe RGB pixels".into());
        }
        let t = photocraft_cms::Transform::new(&source, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)
            .map_err(|e| e.to_string())?;
        t.apply(&mut v, 3);
    }
    Ok(v.map(|c| c.clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "1\t0\t0\t2560\t1440\t1\tDP-1\tDELL U2720Q\tDEL\tABC123\n\
                       2\t2560\t0\t4480\t1080\t0\tHDMI-1\t\t\t\n\
                       bad line\n";

    /// A fresh folder holding Display P3 profiles under `files`, removed when dropped.
    struct Dir(PathBuf);
    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir(files: &[&str]) -> Dir {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("photocraft-haiku-profiles-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), photocraft_cms::Builtin::DisplayP3.profile().to_bytes().as_slice()).unwrap();
        }
        Dir(d)
    }

    #[test]
    fn parses_the_hosts_lines_and_skips_malformed_ones() {
        let m = parse(TWO);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].frame, [0.0, 0.0, 2560.0, 1440.0]);
        assert!(m[0].primary && !m[1].primary);
        assert_eq!((m[0].name.as_str(), m[0].serial.as_str(), m[1].connector.as_str()), ("DELL U2720Q", "ABC123", "HDMI-1"));
    }

    #[test]
    fn profile_names_go_from_most_to_least_specific() {
        let m = parse(TWO);
        assert_eq!(profile_names(&m[0]), ["DELL U2720Q ABC123", "DELL U2720Q", "DP-1", "default"]);
        assert_eq!(profile_names(&m[1]), ["HDMI-1", "default"]);
    }

    #[test]
    fn the_monitors_own_profile_wins_over_the_default_and_case_is_ignored() {
        let d = dir(&["dell u2720q.ICC", "default.icm"]);
        let m = parse(TWO);
        assert_eq!(profile_file(d.path(), &m[0]).unwrap().file_name().unwrap(), "dell u2720q.ICC");
        assert_eq!(profile_file(d.path(), &m[1]).unwrap().file_name().unwrap(), "default.icm");
        let empty = dir(&["notes.txt"]);
        assert_eq!(profile_file(empty.path(), &m[0]), None);
    }

    #[test]
    fn read_gives_display_frames_in_points_with_their_profiles() {
        let d = dir(&["DP-1.icc"]);
        let displays = read(TWO, Some(d.path()), 2.0).unwrap();
        assert_eq!(displays.len(), 2);
        assert_eq!((displays[0].id, displays[0].name.as_str()), (1, "DELL U2720Q"));
        assert_eq!(displays[0].frame, [0.0, 0.0, 1280.0, 720.0]);
        assert_eq!(displays[0].profile_name.as_deref(), Some("DP-1.icc"));
        assert!(displays[0].icc.is_some());
        assert_eq!(displays[1].name, "HDMI-1");
        assert_eq!(displays[1].frame, [1280.0, 0.0, 960.0, 540.0]);
        assert!(displays[1].icc.is_none() && displays[1].profile_name.is_none());
        assert!(read("", Some(d.path()), 1.0).is_err());
    }

    #[test]
    fn two_identical_monitors_are_told_apart_by_connector() {
        let text = "1\t0\t0\t100\t100\t1\tDP-1\tLG\t\t\n2\t100\t0\t200\t100\t0\tDP-2\tLG\t\t\n";
        let names: Vec<String> = read(text, None, 1.0).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["LG (DP-1)", "LG (DP-2)"]);
    }

    #[test]
    fn picked_pixels_convert_through_the_profile_of_their_display() {
        let d = dir(&["HDMI-1.icc"]);
        assert!(profile_at(TWO, Some(d.path()), 100.0, 100.0).is_none());
        let p3 = profile_at(TWO, Some(d.path()), 3000.0, 100.0).unwrap();
        assert_eq!(to_srgb([255, 0, 0], None).unwrap(), [1.0, 0.0, 0.0]);
        // Display P3's green is outside sRGB: clipped, with red at its floor.
        let g = to_srgb([0, 255, 0], Some(&p3)).unwrap();
        assert!(g[1] > 0.99 && g[0] < 0.01, "{g:?}");
        let lab = photocraft_cms::Builtin::LabD50.profile().to_bytes();
        assert!(to_srgb([1, 2, 3], Some(lab.as_slice())).is_err());
    }
}
