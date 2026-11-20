//! Commit and erase Sixel images in the shared graphics scene.
use alloc::{boxed::Box, sync::Arc};

use crate::{
    Context, Graphics, Image, Placement, Rect, SixelOutcome, SixelPalette, StoredImage, sixel,
};

impl Graphics {
    /// Erase Sixel pixels intersecting a rectangle in the live cell grid.
    ///
    /// Kitty images and history outside this rectangle are unchanged. Erasure
    /// uses source-pixel granularity: a scaled pixel touching the rectangle is
    /// cleared completely. At a nonaligned vertical boundary this can clear up
    /// to pixel_scale_y - 1 additional display pixels. Unscaled images retain
    /// every pixel outside the requested rectangle exactly.
    pub fn erase_sixel_cells(&mut self, col: usize, row: usize, cols: usize, rows: usize) {
        if self.placements.is_empty() || cols == 0 || rows == 0 {
            return;
        }
        let left = col.min(65535) as i64 * i64::from(self.cell_width);
        let top = row.min(65535) as i64 * i64::from(self.cell_height);
        let right = col.saturating_add(cols).min(65535) as i64 * i64::from(self.cell_width);
        let bottom = row.saturating_add(rows).min(65535) as i64 * i64::from(self.cell_height);
        let generation = self.revision.wrapping_add(1);
        let mut changed = false;
        for stored in &mut self.images {
            let Some(remaining) = &mut stored.sixel_pixels else {
                continue;
            };
            for placement in self.placements.iter().filter(|p| p.image_id == stored.id) {
                let rect = placement.rect(self.cell_width, self.cell_height, 0);
                let x = i64::from(rect.x);
                let y = i64::from(rect.y);
                let x0 = left.max(x);
                let y0 = top.max(y);
                let x1 = right.min(x + i64::from(rect.width));
                let y1 = bottom.min(y + i64::from(rect.height));
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                let source = placement.source;
                let sx0 = source.x as usize
                    + ((x0 - x) as u64 * u64::from(source.width) / u64::from(rect.width)) as usize;
                let sx1 = source.x as usize
                    + ((x1 - x) as u64 * u64::from(source.width)).div_ceil(u64::from(rect.width))
                        as usize;
                let sy0 = source.y as usize
                    + ((y0 - y) as u64 * u64::from(source.height) / u64::from(rect.height))
                        as usize;
                let sy1 = source.y as usize
                    + ((y1 - y) as u64 * u64::from(source.height)).div_ceil(u64::from(rect.height))
                        as usize;
                let width = stored.image.width as usize;
                let previous_capacity = stored.image.pixels.capacity();
                let pixels = Arc::make_mut(&mut stored.image.pixels);
                // A retained frame can make this clone a Vec with less spare capacity.
                self.used = self.used - previous_capacity + pixels.capacity();
                for source_row in sy0..sy1 {
                    let begin = (source_row * width + sx0) * 4;
                    let end = (source_row * width + sx1) * 4;
                    for pixel in pixels[begin..end].as_chunks_mut::<4>().0 {
                        if pixel[3] != 0 {
                            pixel.fill(0);
                            *remaining -= 1;
                            stored.image.generation = generation;
                            changed = true;
                        }
                    }
                }
                // Also reclaim a transparent-only Sixel when its area is erased.
                changed |= *remaining == 0;
            }
        }
        if changed {
            let images = &self.images;
            self.placements.retain(|placement| {
                images
                    .iter()
                    .any(|image| image.id == placement.image_id && image.sixel_pixels != Some(0))
            });
            let used = &mut self.used;
            self.images.retain(|image| {
                if image.sixel_pixels == Some(0) {
                    *used -= image.image.pixels.capacity();
                    false
                } else {
                    true
                }
            });
            self.changed();
        }
    }

    /// Begin a sixel body after a validated DCS `q` header. Color registers and
    /// decoding storage are allocated only while a sixel stream is active.
    pub fn begin_sixel(
        &mut self,
        aspect: u16,
        transparent: bool,
        background: [u8; 3],
        palette: Option<&SixelPalette>,
    ) {
        self.query_only = false;
        self.abort_transfer();
        self.sixel = Some(Box::new(sixel::Decoder::new(
            self.limits,
            aspect,
            transparent,
            background,
            palette,
        )));
    }

    /// Feed one body byte. A malformed or excessive stream drops its partial
    /// allocation immediately; later bytes remain ignored until the next DCS.
    pub fn sixel_put(&mut self, byte: u8) {
        if self
            .sixel
            .as_mut()
            .is_some_and(|decoder| decoder.push(byte).is_err())
        {
            self.sixel = None;
        }
    }

    pub fn abort_sixel(&mut self) {
        self.sixel = None;
    }

    /// Commit only after an actual ST. No reply is sent by the sixel protocol.
    /// The terminal, rather than the scene, implements scrolling/cursor policy.
    pub fn finish_sixel(&mut self, context: Context) -> Option<SixelOutcome> {
        let decoded = self.sixel.take()?.finish().ok()?;
        if decoded.width == 0 || decoded.height == 0 {
            return Some(SixelOutcome {
                width: 0,
                height: 0,
                palette: decoded.palette,
            });
        }
        if self.placements.len() >= self.limits.max_placements {
            return None;
        }
        let id = self.allocate_id();
        self.reserve_image(id, decoded.pixels.capacity()).ok()?;
        self.set_viewport(context.cell_width, context.cell_height, context.rows);
        self.changed();
        self.used += decoded.pixels.capacity();
        let result = SixelOutcome {
            width: decoded.width,
            height: decoded.height * decoded.scale_y,
            palette: decoded.palette,
        };
        self.images.push(StoredImage {
            id,
            number: 0,
            anonymous: true,
            sixel_pixels: Some(
                decoded
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[3] != 0)
                    .count(),
            ),
            image: Image {
                width: decoded.width,
                height: decoded.height,
                pixels: Arc::new(decoded.pixels),
                generation: self.revision,
            },
            animation: None,
        });
        self.placements.push(Placement {
            image_id: id,
            id: 0,
            col: context.col.min(context.cols.saturating_sub(1)).min(65534) as i32,
            row: context.row.min(context.rows.saturating_sub(1)).min(65534) as i32,
            source: Rect {
                x: 0,
                y: 0,
                width: result.width,
                height: result.height / decoded.scale_y,
            },
            columns: 0,
            rows: 0,
            x_offset: 0,
            y_offset: 0,
            z: -1,
            pixel_scale_y: decoded.scale_y,
            virtual_placement: false,
            parent: None,
            horizontal: 0,
            vertical: 0,
        });
        self.placements.sort_by_key(|p| (p.z, p.image_id));
        Some(result)
    }
}
