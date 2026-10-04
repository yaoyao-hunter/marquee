# marquee

Terminal marquee / scrolling text with correct CJK, emoji and mixed-width
Unicode handling.

Work in progress: the v1 core (unicode cells, CLI, terminal layer, scroll
engine) is under construction on `feat/marquee-core`; the big-font mode
(fusion-pixel 12px pixel glyphs scrolling over multiple terminal rows) is
designed in [docs/big-font-mode.md](docs/big-font-mode.md) and built on
`feat/big-font-mode`.

## Big-font assets

`assets/bigfont-12px-zh-hans.bin` is a compact glyph atlas generated at
development time from the official Fusion Pixel Font BDF release by
`tools/gen-bigfont` and committed to the repo, so builds stay offline and
`cargo install` needs no font downloads. The binary format, the recorded
provenance (release tag + sha256) and the regeneration procedure are in
[docs/bigfont-asset-format.md](docs/bigfont-asset-format.md).

## Acknowledgements

- [Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font) by TakWolf,
  licensed under the [SIL Open Font License 1.1](assets/fusion-pixel/LICENSE-OFL).
  It merges glyphs from Ark Pixel Font, Cubic 11 and Galmuri; the full
  copyright notices live in
  [assets/fusion-pixel/COPYRIGHT.md](assets/fusion-pixel/COPYRIGHT.md).
