//! The libraries the program cannot start without, read from the program
//! itself.
//!
//! A unit test cannot see this: its binary has no `main` of bliss's, so the
//! linker drops what only the window reaches, and libxdo with it. The
//! program's own file is the one to read: `CARGO_BIN_EXE_bliss`, its
//! `DT_NEEDED` entries.

#![cfg(target_os = "linux")]

/// The `DT_NEEDED` names of a little-endian ELF64 file: the libraries the
/// loader must find before the program's first line runs.
fn needed(path: &str) -> Vec<String> {
    let elf = std::fs::read(path).expect("the program's file");
    assert_eq!(&elf[..4], b"\x7fELF", "an ELF file");
    assert_eq!(elf[4], 2, "64-bit");
    assert_eq!(elf[5], 1, "little-endian");

    let u16_at = |at: usize| u16::from_le_bytes(elf[at..at + 2].try_into().unwrap()) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap()) as usize;
    let u64_at = |at: usize| u64::from_le_bytes(elf[at..at + 8].try_into().unwrap()) as usize;

    let (sections, size, count) = (u64_at(0x28), u16_at(0x3a), u16_at(0x3c));
    let section = |index: usize| sections + index * size;
    const SHT_DYNAMIC: usize = 6;
    let dynamic = (0..count)
        .map(section)
        .find(|&header| u32_at(header + 4) == SHT_DYNAMIC)
        .expect("a dynamic section");
    // Its strings are the section its `sh_link` names.
    let strings = u64_at(section(u32_at(dynamic + 40)) + 24);

    const DT_NULL: usize = 0;
    const DT_NEEDED: usize = 1;
    let (start, end) = (
        u64_at(dynamic + 24),
        u64_at(dynamic + 24) + u64_at(dynamic + 32),
    );
    let mut names = Vec::new();
    for entry in (start..end).step_by(16) {
        match u64_at(entry) {
            DT_NULL => break,
            DT_NEEDED => {
                let name = &elf[strings + u64_at(entry + 8)..];
                let name = &name[..name.iter().position(|&b| b == 0).expect("a NUL")];
                names.push(String::from_utf8_lossy(name).into_owned());
            }
            _ => {}
        }
    }
    names
}

/// libxdo is not among them (`no_xdo.rs`). Ubuntu ships `libxdo.so.3`,
/// Arch `libxdo.so.4`, and a program that needs one does not start where the
/// other is: the build of 15 October, made on Ubuntu, needed `libxdo.so.3`,
/// which Arch's xdotool no longer has.
#[test]
fn the_program_does_not_need_libxdo() {
    let needed = needed(env!("CARGO_BIN_EXE_bliss"));
    assert!(
        needed.iter().any(|name| name.starts_with("libc.so")),
        "read as an ELF's own list: {needed:?}"
    );
    assert!(
        needed.iter().all(|name| !name.starts_with("libxdo")),
        "{needed:?}"
    );
}
