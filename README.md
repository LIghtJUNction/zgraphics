# zgraphics

Bounded Kitty Graphics Protocol and DEC Sixel processing for `no_std` terminals. The library
uses `alloc`, performs no operating-system calls, and does not select a window
system or renderer. All production Rust dependencies disable default features.

It accepts direct (`t=d`) RGB, RGBA and PNG image data, including zlib compression,
base64 transfers, capability queries, image IDs/numbers, source cropping,
placements, placement IDs, scaling, z-order and deletion. PNG decoding uses
`zune-png`; the container is checked before decoding and nonessential metadata is
removed. Kitty animation frames, frame composition and playback use a monotonic
clock supplied by the host; the library starts no timer. APNG remains unsupported.

File, temporary-file and shared-memory transfers use an optional host-supplied
`ExternalReader`. The host owns object validation, bounded reads and unlink policy.
Without that callback, external transfers are unavailable. Unicode placeholders
and relative placements resolve through the same scene: the host supplies decoded
`Placeholder` cells, and `draws_with_placeholders` returns the corresponding draws.

```rust
use zgraphics::{Context, Graphics};

let mut images = Graphics::default();
let context = Context {
    col: 0, row: 0, cols: 80, rows: 24, cell_width: 8, cell_height: 16,
};
let mut replies = Vec::new();
images.command(b"Ga=T,i=1,f=24,s=1,v=1,C=1;/wAA", context, &mut replies);
let draw = images.draws(0).next().unwrap();
assert_eq!(draw.image.pixels.as_slice(), [255, 0, 0, 255]);
assert_eq!(replies, b"\x1b_Gi=1;OK\x1b\\");
```

`Graphics::command` accepts one protocol-sized APC body starting with `G`, without
the surrounding `ESC _` and `ESC \\`. For a byte stream, use `CommandStream`:
it retains at most a 512-byte header and 4096 encoded bytes, decoding larger bodies
incrementally into the bounded transfer. This also accepts existing clients such
as `kitten icat` which can emit an entire image in one APC. Both padded and
unpadded final base64 are accepted; an explicit nonfinal `m=1` chunk must still be
a multiple of four without padding. The host owns ESC/ST framing and passes
cursor/grid context. Images become visible only when `finish` receives ST.
`Graphics::draws` returns sorted, borrowed straight-alpha RGBA images and source
and destination rectangles. The renderer honors `Draw::clip` while preserving the
full source-to-destination transform, additionally clips to its viewport, and
implements the documented z-order around text and explicit backgrounds.
Pixel buffers use `Arc<Vec<u8>>`; a native renderer can retain a buffer until its
graphics API has finished reading it. Renderers must release their retained
references and converted-image caches when no longer needed. The dependency on
`Arc` requires targets with pointer-width atomics.

Default limits per scene are 8192 pixels per dimension, 32 MiB per decoded image,
32 MiB total decoded storage, 16 MiB per pending transfer, 128 stored images and
512 placements. Limits are configurable. PNG decoding has additional temporary
buffers bounded by validated dimensions and compressed input; the decoded-store
quota is not a claim about peak process RSS. A terminal normally keeps independent
scenes for primary and alternate screens. No image storage is allocated while idle.
The decoded-storage accounting includes vector capacity, not only populated pixels.

`scroll`, `trim_history`, and `set_viewport` let the host keep placements attached
to terminal content. `revision` changes on scene mutations; `Image::generation`
distinguishes replacement pixel data under an existing ID for renderer caches.
Generation values are local to a scene; clear renderer caches when changing to a
different scene. Full-screen upward scrolls can retain images in history, while
partial-region scrolls move only placements whose anchors lie inside the region.

Protocol reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.

## Sixel

`begin_sixel`, `sixel_put`, `finish_sixel`, and `abort_sixel` decode a DCS body
incrementally into the same image scene used by Kitty images. The decoder handles
repeat counts, raster extents, integer pixel aspect ratios, graphics carriage
return/newline, 256 color registers, RGB percentages, DEC HLS colors, and opaque
or transparent backgrounds. Raster extents are minimum dimensions, so following
pixel data may extend them. Redefining a register recolors earlier pixels within
the image. The initial 16 registers use VT340 colors; remaining registers start
black. The host can retain the returned palette for shared-register mode.

The decoder creates no image storage before its first pixel or declared raster.
It counts encoded input against `max_transfer_bytes`, decoded RGBA capacity
against `max_image_bytes`, and expanded painting/copying work against eight times
`max_image_bytes` pixel operations. Dimensions include the vertical aspect scale;
aspect ratios are rounded to an integer of at least one and bounded at 64.
Malformed parameters or a quota violation immediately release the partial
decoder. A host must call `finish_sixel` only for ST; CAN, SUB, replacement escape
sequences and a truncated stream must cancel it. Invalid input cannot replace a
completed image or move the text cursor.

During decoding the indexed canvas uses at most half the decoded-image quota.
Final conversion can briefly hold that canvas and one RGBA buffer together
(48 MiB with the default 32 MiB image quota). Completed images remain subject to
the existing 32 MiB scene quota; renderer-owned uploads are separate. Palette
changes in a later image do not recolor already completed RGBA images.

The `zterm` host implements default scrolling, display mode (DECSDM 80), the
right-of-image cursor option (8452), and private/shared color registers (1070).
It accepts `ESC P` introducers and either `ESC \\` or the 8-bit ST byte as the
terminator. It preserves framing across arbitrary PTY read boundaries.

Sixel protocol and raster/color definitions:
<https://vt100.net/docs/vt3xx-gp/chapter14.html>.
Modern DECSDM polarity and cursor extensions:
<https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>.
The DEC manual's DECSDM set/reset prose conflicts with modern xterm behavior;
this implementation follows xterm: mode 80 set disables scrolling and uses the
top-left origin, while reset enables scrolling at the text cursor.

The host-level [Sixel reference test](https://github.com/LIghtJUNction/ztty/blob/f2db31d7ea35bf1a0554e76d69d2e44faafd0cab/tests/sixel_reference.py)
compares fixture pixels, libsixel's `sixel2png`, and the `zterm` parser at five input
chunk sizes. It remains in the host repository. This crate's tests exercise protocol
and pixel behavior; they do not establish native GUI presentation.


## Standalone crate and validation

This repository contains the independent `zgraphics 0.2.0` workspace. It was
extracted from [ztty at `f2db31d7`](https://github.com/LIghtJUNction/ztty/tree/f2db31d7ea35bf1a0554e76d69d2e44faafd0cab/crates/zgraphics);
[the extraction record](https://github.com/LIghtJUNction/zgraphics/blob/main/docs/extraction.json)
records the original file blobs. Public types remain under `zgraphics::`, and
the module split changes neither their paths nor the method bodies.

| Module | Responsibility |
| --- | --- |
| `lib.rs` | Public data, scene storage, quotas and content lifecycle |
| `kitty.rs` | Kitty command execution, transfer completion and replies |
| `sixel_scene.rs` | Sixel stream lifetime, scene commit and cell erasure |
| `sixel.rs`, `decode.rs` | Bounded raster and image decoding |
| `placement.rs` | Relative anchors and Unicode placeholder draw geometry |
| `animation.rs` | Frame storage, composition and host-clock advancement |
| `protocol.rs`, `stream.rs`, `transport.rs` | Command parsing, APC streaming and host I/O callback |

Run the library checks from this directory with Rust 1.100 or newer:

```sh
cargo test --locked --lib
cargo test --locked --doc
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
python3 tools/check_package.py
cargo package --locked
rustup target add wasm32-unknown-unknown x86_64-unknown-none
cargo check --locked --lib --target wasm32-unknown-unknown
cargo check --locked --lib --target x86_64-unknown-none
```

The manually invoked [candidate workflow](https://github.com/LIghtJUNction/zgraphics/blob/main/.github/workflows/ci.yml)
checks one fixed commit on Linux, macOS and Windows, plus WebAssembly and bare-metal
compilation, formatting, Clippy and the crate archive. It has read-only repository
permissions and does not publish a crate or create a release. Compilation on a
target does not establish renderer, filesystem callback or GUI behavior on that
platform.
