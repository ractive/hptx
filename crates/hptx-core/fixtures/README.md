# Recorded calculator replies

Recorded on 2026-10-05 from the emulated 48SX (ROM J), 48GX (ROM R) and
49G (ROM 2.15) in `emulator/`, with
`cargo run -p kermit-proto --example record` (see `emulator/README.md`).
File names are `<model>-<what>`. All files hold raw bytes in the HP
character set.

Text replies (`.txt`):

| File | Request |
|------|---------|
| `dir`, `dir-sub` | `G D` in HOME and in the subdirectory `D1` |
| `path`, `path-sub` | `C PATH` in HOME and in `D1` |
| `mem`, `version`, `iopar`, `vars`, `flag` | `C MEM`, `VERSION`, `IOPAR`, `VARS`, `-35 FS?` |
| `stack` | `C "a\nb" 1.5 { 1 2 }` (a multi-line value) |
| `empty` | `C CLEAR` |
| `error`, `error-undefined`, `error-syntax` | `1 0 /`, `'NOSUCH' RCL`, `\->LIST` |

Binary objects (`.hp`, GET with flag -35 set): `R` real 1.5, `S` string
"AB", `L` list { 1 2 }, `P` program « 1 + », `A` algebraic 'X+Y', `C`
complex (1,2), `B` binary integer #FFh, `TG` list { :T:5 }, `G` the LCD
(`LCD→`, 131x64 GROB), `D1` a directory holding Z = 3.
