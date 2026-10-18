# pkg-merge-rs

A Rust rewrite of [Tustin/pkg-merge](https://github.com/Tustin/pkg-merge): merges split PS4 PKG parts into one whole PKG file. Std-only, zero dependencies, single binary.

## Why a rewrite

The original C++ tool works, but ships a few unfixed bugs. This rewrite replicates its behavior and fixes them:

| Original bug | Fix here |
|---|---|
| PKG magic checked with 1 byte (`sizeof(...) != 0` paren bug) | full 4-byte `7F 43 4E 54` check |
| Pieces arriving before the root file were dropped | pieces buffered until root shows up (any directory order works) |
| Dead `strtol` error check — garbage suffixes silently became piece 0 | piece suffix must be pure digits; anything else is a root candidate |
| Missing piece in the middle silently produced a corrupt PKG | gaps are refused with a loud error, no output written |
| `printf("%x", char*)` undefined behavior | proper hex formatting |
| `assert()` compiled out in release builds | plain error handling |

## How detection works

For every `*.pkg` file in the directory (the `-merged.pkg` output is ignored):

- Filename after the **last** `_` and before the first `.` is the piece suffix.
- Pure digits (`..._1.pkg`, `..._2.pkg`) → piece of that number for `title_id = name before the last _`.
- Any file that starts with the PKG magic `7F 43 4E 54` → root of its `title_id` group. Real dumps name the root `..._0.pkg` (magic + digit suffix) — handled.
- Pieces are appended to the root in ascending order; piece numbers must be contiguous `1..N`, otherwise the package is skipped with an error.

## Build

```
cargo build --release
```

## Usage

```
pkg-merge-rs <directory>
```

Put the root PKG and all its pieces in one directory and point the tool at it:

```
pkg-merge-rs D:\pkgs\crash-team-racing
```

Output is written next to the root file as `<title_id>-merged.pkg`, e.g.:

```
UP0002-CUSA13795_00-CRASHTEAMRACING1-A0121-V0101-merged.pkg
```

Free disk space equal to the combined PKG size is required. Source files are never modified.

Progress is printed per 512 KB chunk while each piece is appended; a `9.1 GB / 3-piece` merge takes ~3 minutes on a SATA SSD.

## Verification

- 15 unit tests (`cargo test`) covering parsing, grouping and merge semantics.
- Real-world check: 3-piece `UP0002-CUSA13795` (Crash Team Racing) merge, byte-identical boundaries at 4 GiB / 8 GiB, exact total size 9,116,712,960 bytes.
