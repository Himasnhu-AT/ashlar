//! The code-signature check an unpacked bundle passes before anything in it runs.
//!
//! [`Codesign`] (macOS) finds every Mach-O file in the bundle and checks each executable with
//! `codesign --verify --strict` against a Developer ID requirement for the expected team id. Libraries
//! are left to the system: under the hardened runtime, library validation refuses to load a library
//! that isn't signed by the same team. When an executable's entitlements turn library validation off,
//! every Mach-O file is checked instead. A symbolic link that leads out of the bundle fails the check.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Most files looked at in one bundle.
const MAX_FILES: usize = 200_000;
/// Deepest folder nesting looked at.
const MAX_DEPTH: usize = 64;

/// Checks an unpacked bundle; `Err` says why it must not run.
pub trait Verify: Send + Sync {
    fn verify(&self, bundle: &Path, entry: &Path) -> Result<(), String>;
}

/// `codesign` against a Developer ID team.
#[derive(Debug, Clone)]
pub struct Codesign {
    team_id: String,
}

impl Codesign {
    /// `team_id`: the 10-character Apple team identifier the bundle must be signed with.
    pub fn new(team_id: &str) -> Codesign {
        Codesign { team_id: team_id.to_string() }
    }

    fn requirement(&self) -> String {
        // A Developer ID Application certificate (its leaf and intermediate markers) of this team.
        format!(
            "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and \
             certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{}\"",
            self.team_id
        )
    }

    fn check(&self, file: &Path) -> Result<(), String> {
        let out = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "-R", &self.requirement()])
            .arg(file)
            .output()
            .map_err(|e| format!("couldn't run codesign: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let why = String::from_utf8_lossy(&out.stderr);
        Err(format!("{} isn't signed by team {}: {}", file.display(), self.team_id, why.trim().chars().take(300).collect::<String>()))
    }

    fn disables_library_validation(file: &Path) -> bool {
        Command::new("/usr/bin/codesign")
            .args(["-d", "--entitlements", "-", "--xml"])
            .arg(file)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("com.apple.security.cs.disable-library-validation"))
            .unwrap_or(true)
    }
}

impl Verify for Codesign {
    fn verify(&self, bundle: &Path, entry: &Path) -> Result<(), String> {
        if !cfg!(target_os = "macos") {
            return Err("code signatures can only be checked on macOS".into());
        }
        if !(self.team_id.len() == 10 && self.team_id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())) {
            return Err(format!("{:?} isn't a team id", self.team_id));
        }
        let files = scan(bundle)?;
        if !files.iter().any(|f| f.path == entry && f.kind == Kind::Executable) {
            return Err(format!("{} isn't an executable in the bundle", entry.display()));
        }
        let executables: Vec<&MachO> = files.iter().filter(|f| f.kind == Kind::Executable).collect();
        for f in &executables {
            self.check(&f.path)?;
        }
        if executables.iter().any(|f| Codesign::disables_library_validation(&f.path)) {
            for f in files.iter().filter(|f| f.kind == Kind::Library) {
                self.check(&f.path)?;
            }
        }
        Ok(())
    }
}

/// Accepts any bundle: for tests only.
#[cfg(any(test, feature = "fake-server"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptAll;

#[cfg(any(test, feature = "fake-server"))]
impl Verify for AcceptAll {
    fn verify(&self, bundle: &Path, _entry: &Path) -> Result<(), String> {
        scan(bundle).map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Executable,
    Library,
}

#[derive(Debug)]
struct MachO {
    path: PathBuf,
    kind: Kind,
}

/// Every Mach-O file in `root`; fails on a link out of `root`.
fn scan(root: &Path) -> Result<Vec<MachO>, String> {
    let root = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut out = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err("the bundle is nested too deeply".into());
        }
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            seen += 1;
            if seen > MAX_FILES {
                return Err("the bundle has too many files".into());
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                let target = fs::canonicalize(&path).map_err(|_| format!("{} is a broken link", path.display()))?;
                if !target.starts_with(&root) {
                    return Err(format!("{} links out of the bundle", path.display()));
                }
                continue; // its target is scanned where it lives
            }
            if meta.is_dir() {
                stack.push((path, depth + 1));
            } else if meta.is_file()
                && let Some(kind) = macho_kind(&path)
            {
                out.push(MachO { path, kind });
            }
        }
    }
    Ok(out)
}

/// A Mach-O file's kind from its header: `MH_EXECUTE` is an executable, anything else (dylibs, bundles,
/// Python extension modules) a library. A universal file counts as an executable when its first slice is.
fn macho_kind(path: &Path) -> Option<Kind> {
    let mut head = [0u8; 64];
    let mut f = fs::File::open(path).ok()?;
    let n = f.read(&mut head).ok()?;
    let word = |at: usize, big: bool| -> Option<u32> {
        let b: [u8; 4] = head.get(at..at + 4)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    if n < 16 {
        return None;
    }
    let magic = word(0, true)?;
    let filetype = match magic {
        0xcffa_edfe | 0xcefa_edfe => word(12, false)?, // 64/32-bit, little-endian
        0xfeed_facf | 0xfeed_face => word(12, true)?,
        0xcafe_babe | 0xcafe_babf => {
            // Universal: read the first slice's header.
            let offset = word(16, true)?;
            let mut slice = [0u8; 16];
            use std::io::{Seek, SeekFrom};
            f.seek(SeekFrom::Start(u64::from(offset))).ok()?;
            f.read_exact(&mut slice).ok()?;
            let b: [u8; 4] = slice.get(12..16)?.try_into().ok()?;
            u32::from_le_bytes(b)
        }
        _ => return None,
    };
    Some(if filetype == 2 { Kind::Executable } else { Kind::Library })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mach_o_files_are_found_and_links_out_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("b");
        fs::create_dir_all(root.join("lib")).unwrap();
        let mut exe = vec![0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01, 0, 0, 0, 0, 2, 0, 0, 0];
        exe.resize(64, 0);
        let mut lib = exe.clone();
        lib[12] = 6; // MH_DYLIB
        fs::write(root.join("tool"), &exe).unwrap();
        fs::write(root.join("lib/x.dylib"), &lib).unwrap();
        fs::write(root.join("notes.txt"), b"hello there, not a binary").unwrap();
        let mut found: Vec<(String, Kind)> =
            scan(&root).unwrap().into_iter().map(|m| (m.path.file_name().unwrap().to_string_lossy().into_owned(), m.kind)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found, [("tool".to_string(), Kind::Executable), ("x.dylib".to_string(), Kind::Library)]);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("lib/x.dylib", root.join("inside")).unwrap();
            assert!(scan(&root).is_ok());
            std::os::unix::fs::symlink("/bin", root.join("outside")).unwrap();
            assert!(scan(&root).unwrap_err().contains("links out"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsigned_or_foreign_binaries_fail_the_team_check() {
        // A system binary is signed by Apple, not by this team.
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("true");
        fs::copy("/usr/bin/true", &exe).unwrap();
        let err = Codesign::new("ABCDE12345").verify(dir.path(), &fs::canonicalize(&exe).unwrap()).unwrap_err();
        assert!(err.contains("isn't signed by team"), "{err}");
        assert!(Codesign::new("bad").verify(dir.path(), &exe).is_err());
    }
}
