use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum PartKind {
    /// `title_<digits>.pkg` — numbered continuation piece.
    Piece(u32),
    /// Any other suffix; becomes the root if the file carries the PKG magic.
    RootCandidate,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub kind: PartKind,
    pub path: PathBuf,
}

pub fn parse_pkg_name(name: &str) -> Option<Entry> {
    // ponytail: C++ checked extension == ".pkg" case-sensitively; we accept any case.
    if !name.to_ascii_lowercase().ends_with(".pkg") {
        return None;
    }
    if name.contains("-merged") {
        return None;
    }

    let dot = name.find('.')?;
    if let Some(u) = name.rfind('_')
        && u < dot
    {
        let title = &name[..u];
        let suffix = &name[u + 1..dot];
        if !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = suffix.parse::<u32>()
        {
            return Some(Entry {
                title: title.to_string(),
                kind: PartKind::Piece(n),
                path: PathBuf::from(name),
            });
        }
        // Non-numeric suffix: root candidate (C++ strtol leniency, fixed: no
        // silent piece-0 assignment, needs the PKG magic check later).
        return Some(Entry {
            title: title.to_string(),
            kind: PartKind::RootCandidate,
            path: PathBuf::from(name),
        });
    }
    Some(Entry {
        title: name[..dot].to_string(),
        kind: PartKind::RootCandidate,
        path: PathBuf::from(name),
    })
}

const PKG_MAGIC: [u8; 4] = [0x7F, 0x43, 0x4E, 0x54];

#[derive(Debug, Default)]
pub struct Group {
    pub root: Option<PathBuf>,
    /// part number -> file. Sorted by number at merge time.
    pub parts: std::collections::BTreeMap<u32, PathBuf>,
}

/// Groups parsed entries by title. `read_magic` decouples file IO for tests.
/// Piece arriving before its root is buffered, not dropped (fixes C++ bug).
pub fn build_groups<E: IntoIterator<Item = Entry>, M>(
    entries: E,
    mut read_magic: M,
) -> std::collections::BTreeMap<String, Group>
where
    M: FnMut(&PathBuf) -> Option<[u8; 4]>,
{
    let mut groups: std::collections::BTreeMap<String, Group> = Default::default();
    for e in entries {
        let g = groups.entry(e.title.clone()).or_default();
        match e.kind {
            PartKind::Piece(n) => {
                // Only the true root carries the PKG header; numbered pieces
                // start with raw data (observed: 7F B9 8B E8, 25 BB BF 8D).
                if read_magic(&e.path) == Some(PKG_MAGIC) {
                    if g.root.is_some() {
                        println!("[warn] extra root PKG for '{}' rejected", e.title);
                    } else {
                        println!("[success] found root PKG file for {}", e.title);
                        g.root = Some(e.path);
                    }
                } else if g.parts.insert(n, e.path).is_some() {
                    println!("[warn] duplicate piece {} for '{}' skipped", n, e.title);
                } else {
                    println!("[success] found piece {} for PKG file {}", n, e.title);
                }
            }
            PartKind::RootCandidate => {
                match read_magic(&e.path) {
                    Some(m) if m == PKG_MAGIC => {
                        if g.root.is_some() {
                            println!("[warn] extra root PKG for '{}' rejected", e.title);
                        } else {
                            g.root = Some(e.path);
                        }
                    }
                    Some(m) => println!(
                        "[warn] assumed root PKG file '{}' doesn't match PKG magic (is {:02X?}, wants {:02X?}). skipping...",
                        e.path.display(),
                        m,
                        PKG_MAGIC
                    ),
                    None => println!(
                        "[warn] assumed root PKG file '{}' unreadable. skipping...",
                        e.path.display()
                    ),
                }
            }
        }
    }
    groups
}

use std::io::{Read, Write};

pub fn merge_group(title: &str, group: &Group) -> std::io::Result<u64> {
    let root = match group.root.as_ref() {
        Some(r) => r,
        None => {
            eprintln!("[error] no root PKG found for '{}', skipping merge", title);
            return Err(std::io::Error::other("no root PKG"));
        }
    };

    // Pieces must be exactly 1..=N with no holes: a missing part would
    // silently produce a corrupt PKG (the C++ tool never checked this).
    let mut expected = 1u32;
    for n in group.parts.keys() {
        if *n != expected {
            let missing: Vec<String> = (expected..*n).map(|m| m.to_string()).collect();
            eprintln!(
                "[error] package '{}' is missing piece(s) {}; skipping merge",
                title,
                missing.join(", ")
            );
            return Err(std::io::Error::other("piece gap"));
        }
        expected += 1;
    }

    let pieces = group.parts.len();
    println!(
        "[work] beginning to merge {} {} for package {}...",
        pieces,
        if pieces == 1 { "piece" } else { "pieces" },
        title
    );

    let merged_path = root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join(format!("{}-merged.pkg", title));

    println!("\t[work] copying root package file to new file...");
    std::fs::copy(root, &merged_path)?;
    let mut total = std::fs::metadata(&merged_path)?.len();

    let mut merged = std::fs::OpenOptions::new().append(true).open(&merged_path)?;
    for (n, path) in &group.parts {
        let piece_size = std::fs::metadata(path)?.len();
        let mut piece = std::fs::File::open(path)?;
        let mut buf = vec![0u8; 1024 * 512];
        let mut copied: u64 = 0;
        loop {
            let read = piece.read(&mut buf)?;
            if read == 0 {
                break;
            }
            merged.write_all(&buf[..read])?;
            copied += read as u64;
            if piece_size > 0 {
                print!(
                    "\r\t[work] merged {}/{} bytes ({}%) for part {}...",
                    copied,
                    piece_size,
                    copied * 100 / piece_size,
                    n
                );
                std::io::stdout().flush().ok();
            }
        }
        println!("done");
        total += piece_size;
    }
    Ok(total)
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("pkg-merge-rs-test-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn group_with(root: Option<PathBuf>, parts: &[(u32, PathBuf)]) -> Group {
        Group {
            root,
            parts: parts.iter().map(|(n, p)| (*n, p.clone())).collect(),
        }
    }

    #[test]
    fn merged_output_is_root_then_parts_in_order() {
        let t = TempDir::new("merge");
        let root = t.write("t_0.pkg", &[PKG_MAGIC.as_slice(), b"ROOT"].concat());
        let p1 = t.write("t_1.pkg", b"P1");
        let p2 = t.write("t_2.pkg", b"P22");
        let g = group_with(Some(root), &[(2, p2), (1, p1)]);

        let written = merge_group("t", &g).expect("merge ok");

        let out = std::fs::read(t.0.join("t-merged.pkg")).unwrap();
        assert_eq!(out, [PKG_MAGIC.as_slice(), b"ROOT", b"P1", b"P22"].concat());
        assert_eq!(written, out.len() as u64);
    }

    #[test]
    fn missing_root_is_error() {
        let t = TempDir::new("noroot");
        let p1 = t.write("t_1.pkg", b"P1");
        assert!(merge_group("t", &group_with(None, &[(1, p1)])).is_err());
    }

    #[test]
    fn missing_root_does_not_create_output() {
        let t = TempDir::new("noroot2");
        let p1 = t.write("t_1.pkg", b"P1");
        let _ = merge_group("t", &group_with(None, &[(1, p1)]));
        assert!(!t.0.join("t-merged.pkg").exists());
    }

    #[test]
    fn gap_in_part_numbers_is_error() {
        let t = TempDir::new("gap");
        let root = t.write("t_0.pkg", &[PKG_MAGIC.as_slice(), b"ROOT"].concat());
        let p1 = t.write("t_1.pkg", b"P1");
        let p3 = t.write("t_3.pkg", b"P3");
        // piece 2 missing -> merged pkg would be corrupt; must refuse.
        assert!(merge_group("t", &group_with(Some(root), &[(1, p1), (3, p3)])).is_err());
        assert!(!t.0.join("t-merged.pkg").exists());
    }

    #[test]
    fn existing_output_is_replaced() {
        let t = TempDir::new("replace");
        let root = t.write("t_0.pkg", &[PKG_MAGIC.as_slice(), b"NEW"].concat());
        t.write("t-merged.pkg", b"STALE-STALE-STALE");
        let g = group_with(Some(root), &[]);
        merge_group("t", &g).unwrap();
        assert_eq!(std::fs::read(t.0.join("t-merged.pkg")).unwrap(), [PKG_MAGIC.as_slice(), b"NEW"].concat());
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use std::path::Path;

    fn entry(title: &str, kind: PartKind, path: &str) -> Entry {
        Entry {
            title: title.to_string(),
            kind,
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn piece_before_root_is_buffered() {
        // Fix for C++ ordering bug: piece arriving before root was skipped.
        let entries = vec![
            entry("t", PartKind::Piece(1), "t_1.pkg"),
            entry("t", PartKind::RootCandidate, "t_0.pkg"),
        ];
        let groups = build_groups(entries, |p| {
            if p == &PathBuf::from("t_0.pkg") { Some(PKG_MAGIC) } else { None }
        });
        let g = groups.get("t").expect("group exists");
        assert_eq!(g.root.as_deref(), Some(Path::new("t_0.pkg")));
        assert!(g.parts.get(&1).unwrap().ends_with("t_1.pkg"));
    }

    #[test]
    fn candidate_without_magic_is_dropped() {
        // Full 4-byte check: 7F B9 8B E8 must NOT pass (C++ compared 1 byte).
        let entries = vec![entry("t", PartKind::RootCandidate, "x.pkg")];
        let groups = build_groups(entries, |_| Some([0x7F, 0xB9, 0x8B, 0xE8]));
        let g = groups.get("t").expect("group exists");
        assert!(g.root.is_none());
        assert!(g.parts.is_empty());
    }

    #[test]
    fn first_root_wins_extra_root_rejected() {
        // C++ silently appended the second root as a piece; we reject it.
        let entries = vec![
            entry("t", PartKind::RootCandidate, "a.pkg"),
            entry("t", PartKind::RootCandidate, "b.pkg"),
        ];
        let groups = build_groups(entries, |_| Some(PKG_MAGIC));
        let g = groups.get("t").expect("group exists");
        assert_eq!(g.root.as_deref(), Some(Path::new("a.pkg")));
        assert!(g.parts.is_empty());
    }

    #[test]
    fn piece_zero_with_magic_is_root() {
        // Real dump layout: the root itself is named _0.pkg and carries the header.
        let entries = vec![
            entry("t", PartKind::Piece(0), "t_0.pkg"),
            entry("t", PartKind::Piece(1), "t_1.pkg"),
        ];
        let groups = build_groups(entries, |p| {
            if p.ends_with("t_0.pkg") { Some(PKG_MAGIC) } else { Some([0x7F, 0xB9, 0x8B, 0xE8]) }
        });
        let g = groups.get("t").expect("group exists");
        assert_eq!(g.root.as_deref(), Some(Path::new("t_0.pkg")));
        assert!(g.parts.get(&1).is_some());
        assert!(g.parts.get(&0).is_none());
    }

    #[test]
    fn titles_are_grouped_independently() {
        let entries = vec![
            entry("t1", PartKind::Piece(1), "t1_1.pkg"),
            entry("t2", PartKind::Piece(2), "t2_2.pkg"),
        ];
        let groups = build_groups(entries, |_| None);
        assert_eq!(groups.len(), 2);
        assert!(groups["t1"].parts.contains_key(&1));
        assert!(groups["t2"].parts.contains_key(&2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PartKind::*;

    #[test]
    fn parses_numbered_piece() {
        let e = parse_pkg_name(
            "UP0002-CUSA13795_00-CRASHTEAMRACING1-A0121-V0101_1.pkg",
        )
        .expect("piece should parse");
        assert_eq!(
            e.title,
            "UP0002-CUSA13795_00-CRASHTEAMRACING1-A0121-V0101"
        );
        assert_eq!(e.kind, Piece(1));
    }

    #[test]
    fn parses_content_id_style_root_candidate() {
        // C++ quirk kept: suffix "00-ABC" is not a number -> root candidate.
        let e = parse_pkg_name("UP0002-CUSA13795_00-ABC.pkg").expect("root candidate should parse");
        assert_eq!(e.title, "UP0002-CUSA13795");
        assert_eq!(e.kind, RootCandidate);
    }

    #[test]
    fn parses_underscoreless_name_as_root_candidate() {
        let e = parse_pkg_name("game.pkg").expect("plain name should parse");
        assert_eq!(e.title, "game");
        assert_eq!(e.kind, RootCandidate);
    }

    #[test]
    fn rejects_non_pkg_extension() {
        assert!(parse_pkg_name("UP0002-CUSA13795_00-ABC.txt").is_none());
    }

    #[test]
    fn rejects_already_merged_output() {
        assert!(parse_pkg_name("UP0002-CUSA13795_00-ABC-merged.pkg").is_none());
    }
}

fn read_pkg_magic(p: &std::path::Path) -> Option<[u8; 4]> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut m = [0u8; 4];
    f.read_exact(&mut m).ok()?;
    Some(m)
}

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("No pkg directory supplied\nUsage: pkg-merge-rs <directory>");
        std::process::exit(1);
    };
    let dir = std::path::PathBuf::from(arg);
    if !dir.is_dir() {
        eprintln!("[error] argument '{}' is not a directory", dir.display());
        std::process::exit(1);
    }

    // Sorted for deterministic root detection regardless of FS order.
    let mut files: Vec<std::fs::DirEntry> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .collect();
    files.sort_by_key(|e| e.file_name());

    let mut entries = Vec::new();
    for f in files {
        let name = f.file_name().to_string_lossy().into_owned();
        match parse_pkg_name(&name) {
            None => println!("[warn] '{}' is not a PKG file. skipping...", name),
            Some(mut entry) => {
                entry.path = f.path();
                entries.push(entry);
            }
        }
    }

    let groups = build_groups(entries, |p| read_pkg_magic(p));

    let mut skipped = 0usize;
    for (title, group) in &groups {
        if let Err(e) = merge_group(title, group) {
            eprintln!("[error] merge of '{}' failed: {}", title, e);
            skipped += 1;
        }
    }
    if skipped == 0 {
        println!("[success] completed");
    } else {
        eprintln!("[error] completed with {} skipped package(s)", skipped);
        std::process::exit(1);
    }
}
