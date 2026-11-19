#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
#[cfg(test)]
use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;

mod animation;
mod decode;
mod kitty;
mod placement;
mod protocol;
mod sixel;
mod sixel_scene;
mod stream;
mod transport;

pub use placement::{MAX_PLACEHOLDER_RUNS, Placeholder};
pub use sixel::Palette as SixelPalette;
pub use stream::CommandStream;
pub use transport::{ExternalRead, ExternalReader, Medium, ReadError};

use protocol::Command;

/// Limits apply independently to each graphics scene (usually one screen).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_dimension: u32,
    pub max_image_bytes: usize,
    pub max_total_bytes: usize,
    pub max_transfer_bytes: usize,
    pub max_images: usize,
    pub max_placements: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_dimension: 8192,
            max_image_bytes: 32 * 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
            max_transfer_bytes: 16 * 1024 * 1024,
            max_images: 128,
            max_placements: 512,
        }
    }
}

/// Canonical sRGB, straight-alpha RGBA pixels. Row stride is `width * 4`.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
    /// Changes whenever image contents are replaced, including ID reuse.
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A renderer draws `source` scaled into `dest`, clipped to the text viewport.
/// Draws are sorted by z-index, then image ID. `z < 0` is behind text;
/// `z < -1073741824` is also behind explicitly colored cell backgrounds.
pub struct Draw<'a> {
    pub image: &'a Image,
    pub image_id: u32,
    pub placement_id: u32,
    pub source: Rect,
    pub dest: PixelRect,
    /// An optional viewport-relative clip, preserving the full source-to-dest
    /// transform. Unicode placeholder cells use this instead of rounding
    /// fractional source coordinates to whole pixels.
    pub clip: Option<PixelRect>,
    pub z: i32,
}

/// Cacheable geometry, without borrowing pixels or retaining image resources.
#[derive(Clone, Copy, Debug)]
pub struct DrawGeometry {
    pub image_id: u32,
    pub placement_id: u32,
    pub source: Rect,
    pub dest: PixelRect,
    pub clip: Option<PixelRect>,
    pub z: i32,
}

impl Draw<'_> {
    pub fn geometry(&self) -> DrawGeometry {
        DrawGeometry {
            image_id: self.image_id,
            placement_id: self.placement_id,
            source: self.source,
            dest: self.dest,
            clip: self.clip,
            z: self.z,
        }
    }
}

/// Grid context at the instant the final image chunk is received.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// A successful placement can request a cursor move after its rectangle.
#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    pub cursor_move: Option<(usize, usize)>,
}

/// Successfully stored sixel geometry and the resulting color registers.
pub struct SixelOutcome {
    pub width: u32,
    pub height: u32,
    pub palette: SixelPalette,
}

struct StoredImage {
    id: u32,
    number: u32,
    anonymous: bool,
    /// Remaining nontransparent source pixels; None identifies Kitty storage.
    sixel_pixels: Option<usize>,
    image: Image,
    animation: Option<Box<animation::Animation>>,
}

impl StoredImage {
    fn bytes(&self) -> usize {
        self.animation.as_ref().map_or_else(
            || self.image.pixels.capacity(),
            |animation| animation.frames.iter().map(|f| f.pixels.capacity()).sum(),
        )
    }
}

struct Placement {
    image_id: u32,
    id: u32,
    col: i32,
    row: i32,
    source: Rect,
    columns: u32,
    rows: u32,
    x_offset: u32,
    y_offset: u32,
    z: i32,
    pixel_scale_y: u32,
    virtual_placement: bool,
    parent: Option<(u32, u32)>,
    horizontal: i32,
    vertical: i32,
}

impl Placement {
    fn geometry(&self, cell_width: u32, cell_height: u32) -> (u32, u32, u32, u32) {
        let cw = cell_width.max(1);
        let ch = cell_height.max(1);
        let sw = u64::from(self.source.width);
        let sh = u64::from(self.source.height) * u64::from(self.pixel_scale_y);
        let target_w = u64::from(self.columns) * u64::from(cw);
        let target_h = u64::from(self.rows) * u64::from(ch);
        let (width, height) = match (self.columns, self.rows) {
            (0, 0) => (sw, sh),
            (_, 0) => (target_w, (sh * target_w / sw).max(1)),
            (0, _) => ((sw * target_h / sh).max(1), target_h),
            _ if sw * target_h > sh * target_w => (target_w, (sh * target_w / sw).max(1)),
            _ => ((sw * target_h / sh).max(1), target_h),
        };
        let box_w = if self.columns == 0 { width } else { target_w };
        let box_h = if self.rows == 0 { height } else { target_h };
        (
            width.min(u32::MAX as u64) as u32,
            height.min(u32::MAX as u64) as u32,
            box_w.saturating_sub(width).div_ceil(2) as u32,
            box_h.saturating_sub(height).div_ceil(2) as u32,
        )
    }

    fn rect(&self, cw: u32, ch: u32, history: usize) -> PixelRect {
        let (width, height, inset_x, inset_y) = self.geometry(cw, ch);
        PixelRect {
            x: clamp_i32(i64::from(self.col) * i64::from(cw) + i64::from(self.x_offset + inset_x)),
            y: clamp_i32(
                (i64::from(self.row) + history as i64) * i64::from(ch)
                    + i64::from(self.y_offset + inset_y),
            ),
            width,
            height,
        }
    }
}

fn clamp_i32(n: i64) -> i32 {
    n.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

struct Transfer {
    command: Command,
    bytes: Vec<u8>,
}

/// A bounded image store and Kitty image/animation command processor.
/// No platform calls, filesystem access, threads, or rendering backend.
pub struct Graphics {
    limits: Limits,
    images: Vec<StoredImage>,
    placements: Vec<Placement>,
    pending: Option<Transfer>,
    sixel: Option<Box<sixel::Decoder>>,
    used: usize,
    revision: u64,
    next_id: u32,
    cell_width: u32,
    cell_height: u32,
    visible_rows: usize,
    external_reader: Option<ExternalReader>,
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Graphics {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            images: Vec::new(),
            placements: Vec::new(),
            pending: None,
            sixel: None,
            used: 0,
            revision: 0,
            next_id: 1,
            cell_width: 8,
            cell_height: 16,
            visible_rows: 24,
            external_reader: None,
        }
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn image_bytes(&self) -> usize {
        self.used
    }
    pub fn pending_bytes(&self) -> usize {
        self.pending.as_ref().map_or(0, |t| t.bytes.len())
            + self
                .sixel
                .as_ref()
                .map_or(0, |decoder| decoder.allocated_bytes())
    }
    pub fn image_count(&self) -> usize {
        self.images.len()
    }
    pub fn placement_count(&self) -> usize {
        self.placements.len()
    }
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images
            .iter()
            .find(|image| image.id == id)
            .map(|image| &image.image)
    }

    pub fn draw_geometry(&self, geometry: &DrawGeometry) -> Option<Draw<'_>> {
        Some(Draw {
            image: self.image(geometry.image_id)?,
            image_id: geometry.image_id,
            placement_id: geometry.placement_id,
            source: geometry.source,
            dest: geometry.dest,
            clip: geometry.clip,
            z: geometry.z,
        })
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Force renderer cache invalidation, for example after swapping screens.
    pub fn invalidate(&mut self) {
        self.changed();
    }

    pub fn set_viewport(&mut self, cell_width: u32, cell_height: u32, rows: usize) {
        let cw = cell_width.clamp(1, 4096);
        let ch = cell_height.clamp(1, 4096);
        let rows = rows.clamp(1, 65535);
        if (self.cell_width, self.cell_height, self.visible_rows) != (cw, ch, rows) {
            self.cell_width = cw;
            self.cell_height = ch;
            self.visible_rows = rows;
            self.changed();
        }
    }

    pub fn draws(&self, history_offset: usize) -> impl Iterator<Item = Draw<'_>> {
        let history_offset = history_offset.min(i32::MAX as usize);
        self.placements.iter().filter_map(move |p| {
            let image = self.image(p.image_id)?;
            if p.virtual_placement {
                return None;
            }
            let dest = self.placement_rect(p, history_offset, &[])?;
            if i64::from(dest.y) + i64::from(dest.height) <= 0
                || i64::from(dest.y) >= (self.visible_rows as i64) * i64::from(self.cell_height)
            {
                return None;
            }
            Some(Draw {
                image,
                image_id: p.image_id,
                placement_id: p.id,
                source: p.source,
                dest,
                clip: None,
                z: p.z,
            })
        })
    }

    /// Cancel only a partial transmission. Completed images remain untouched.
    pub fn abort_transfer(&mut self) {
        self.pending = None;
    }

    /// Clear all stored images, placements and partial transfers.
    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.pending = None;
        self.sixel = None;
        self.used = 0;
        self.changed();
    }

    /// Follow a text-region scroll. A positive delta moves content down.
    /// Only a full-screen upward scroll may retain placements in scrollback.
    pub fn scroll(
        &mut self,
        top: usize,
        bottom: usize,
        delta: i32,
        history: usize,
        save_history: bool,
    ) {
        if delta == 0 || self.placements.is_empty() {
            return;
        }
        let history = history.min(i32::MAX as usize);
        let full = top == 0 && bottom.saturating_add(1) == self.visible_rows;
        let cw = self.cell_width;
        let ch = self.cell_height;
        self.placements.retain_mut(|p| {
            if p.parent.is_some() || p.virtual_placement {
                return true;
            }
            if full && save_history && delta < 0 {
                p.row = p.row.saturating_add(delta);
                let rect = p.rect(cw, ch, 0);
                i64::from(rect.y) + i64::from(rect.height) > -(history as i64) * i64::from(ch)
            } else if p.row >= top as i32 && p.row <= bottom as i32 {
                p.row = p.row.saturating_add(delta);
                p.row >= top as i32 && p.row <= bottom as i32
            } else {
                true
            }
        });
        self.prune_dependencies();
        self.collect_unreferenced_anonymous();
        self.changed();
    }

    /// Physical placement anchors in stable scene order, in text-cell units.
    /// Negative rows refer to retained primary-screen history.
    pub fn anchors(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.placements
            .iter()
            .filter(|placement| placement.parent.is_none() && !placement.virtual_placement)
            .map(|placement| (placement.col, placement.row))
    }

    /// Move anchors through a text-layout change, preserving image/source/size
    /// metadata. None drops a placement whose original text is no longer kept.
    pub fn remap_anchors(&mut self, mut map: impl FnMut(i32, i32) -> Option<(i32, i32)>) {
        let mut changed = false;
        self.placements.retain_mut(|placement| {
            if placement.parent.is_some() || placement.virtual_placement {
                return true;
            }
            if let Some((col, row)) = map(placement.col, placement.row) {
                changed |= (placement.col, placement.row) != (col, row);
                placement.col = col;
                placement.row = row;
                true
            } else {
                changed = true;
                false
            }
        });
        if changed {
            self.prune_dependencies();
            self.collect_unreferenced_anonymous();
            self.changed();
        }
    }

    /// Drop placements whose last pixel is older than retained text history.
    pub fn trim_history(&mut self, history: usize) {
        let history = history.min(i32::MAX as usize);
        let cw = self.cell_width;
        let ch = self.cell_height;
        let before = self.placements.len();
        self.placements.retain(|p| {
            if p.parent.is_some() || p.virtual_placement {
                return true;
            }
            let r = p.rect(cw, ch, 0);
            i64::from(r.y) + i64::from(r.height) > -(history as i64) * i64::from(ch)
        });
        self.prune_dependencies();
        if before != self.placements.len() {
            self.changed();
        }
        self.collect_unreferenced_anonymous();
    }

    fn collect_unreferenced_anonymous(&mut self) {
        // Identified images may intentionally be transmitted before placement.
        // Keep them until explicit deletion or quota eviction.
        let placements = &self.placements;
        let mut used = 0;
        self.images.retain(|image| {
            let keep = !image.anonymous || placements.iter().any(|p| p.image_id == image.id);
            if keep {
                used += image.bytes();
            }
            keep
        });
        self.used = used;
    }

    fn allocate_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if id != 0 && !self.images.iter().any(|image| image.id == id) {
                return id;
            }
        }
    }

    fn resolve(&self, c: &Command) -> Option<u32> {
        if c.id != 0 {
            return Some(c.id);
        }
        if c.number != 0 {
            return self
                .images
                .iter()
                .rev()
                .find(|i| i.number == c.number)
                .map(|i| i.id);
        }
        None
    }

    fn reserve_image(&mut self, id: u32, size: usize) -> Result<(), &'static str> {
        if size > self.limits.max_image_bytes || size > self.limits.max_total_bytes {
            return Err("ENOSPC:image exceeds memory limit");
        }
        loop {
            let replaced = self
                .images
                .iter()
                .find(|i| i.id == id)
                .map_or(0, StoredImage::bytes);
            let count = self.images.len() + usize::from(replaced == 0);
            if self.used - replaced + size <= self.limits.max_total_bytes
                && count <= self.limits.max_images
                && self.total_frames() + usize::from(replaced == 0) <= animation::MAX_FRAMES
            {
                return Ok(());
            }
            let Some(index) = self.images.iter().position(|image| {
                image.id != id && !self.placements.iter().any(|p| p.image_id == image.id)
            }) else {
                return Err("ENOSPC:image storage is full");
            };
            self.used -= self.images.remove(index).bytes();
            self.changed();
        }
    }
}

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod extension_tests;
#[cfg(test)]
mod sixel_tests;
#[cfg(test)]
mod tests;
