//! The TUN adapter and what bliss puts through it.
//!
//! The machine's proxy settings only ever carried TCP, so a call in Discord, a
//! game or anything else on UDP went straight out and was cut. A TUN adapter
//! takes the packets themselves; they come back out of a userspace stack as
//! connections and datagrams, which is the seam the relay already has.
//!
//! Everything here needs administrator rights: creating an adapter and moving
//! routes does. See §7 of AGENTS.md for how it is probed.

use std::io;
use std::path::{Path, PathBuf};

/// The signed library, carried inside bliss.exe.
///
/// Wintun installs a kernel driver out of this file and Windows checks the
/// signature of what it installs, so it has to be a file on disk: bliss writes
/// it out beside the engine and loads it from there. Loading a library straight
/// from memory would leave nothing to check and is what malware does — bliss.exe
/// is unsigned and has already collected one false positive without any help.
const LIBRARY: &[u8] = include_bytes!("../assets/wintun.dll");

/// The terms it travels under. Clause 3d is the one that lets bliss ship it:
/// alongside software that uses it only through the documented API.
const LICENCE: &str = include_str!("../assets/LICENSE-wintun.txt");

/// Write the library and its licence into the work dir; answer where it went.
///
/// Written only when what is already there is not this, byte for byte: a copy
/// another bliss has loaded cannot be replaced anyway, and one that is already
/// right does not need to be.
pub fn place(work_dir: &Path) -> io::Result<PathBuf> {
    let library = work_dir.join("wintun.dll");
    if std::fs::read(&library).is_ok_and(|found| found == LIBRARY) {
        return Ok(library);
    }
    std::fs::create_dir_all(work_dir)?;
    // Beside it, so that whoever finds the file finds its terms.
    std::fs::write(work_dir.join("LICENSE-wintun.txt"), LICENCE)?;
    // Through a temporary name: half a library is worse than none, because it
    // would pass the "is it there?" check and fail at load.
    let beside = work_dir.join("wintun.dll.new");
    std::fs::write(&beside, LIBRARY)?;
    std::fs::rename(&beside, &library)?;
    Ok(library)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// What `LIBRARY` hashes to: the amd64 library out of wintun.net's
    /// `wintun-0.14.1.zip`, whose own checksum that site publishes.
    const LIBRARY_SHA256: &str = "e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce";

    /// The library has to be the signed one, byte for byte: it is what installs
    /// the driver, and anything else would not be signed. Its licence travels
    /// with it, and `place` writes that out too.
    #[test]
    fn the_library_is_the_signed_one_and_licensed() {
        assert_eq!(format!("{:x}", Sha256::digest(LIBRARY)), LIBRARY_SHA256);
        assert_eq!(LIBRARY.len(), 427_552);
        assert!(
            LICENCE.starts_with("Prebuilt Binaries License"),
            "the wrong licence"
        );
        assert!(
            LICENCE.contains("distributed alongside"),
            "clause 3d, which is why bliss may ship it at all"
        );
    }

    #[test]
    fn a_library_already_in_place_is_left_alone() {
        let dir = std::env::temp_dir().join("bliss-tun-places-the-library");
        let _ = std::fs::remove_dir_all(&dir);
        let first = place(&dir).expect("written");
        assert_eq!(std::fs::read(&first).expect("readable"), LIBRARY);
        assert!(dir.join("LICENSE-wintun.txt").exists(), "the terms too");
        let written_at = std::fs::metadata(&first)
            .expect("there")
            .modified()
            .expect("a time");

        let again = place(&dir).expect("asked again");
        assert_eq!(first, again);
        assert_eq!(
            std::fs::metadata(&again)
                .expect("there")
                .modified()
                .expect("a time"),
            written_at,
            "written again for nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
