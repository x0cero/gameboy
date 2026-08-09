//! Experimental "voxel diorama" renderer (--3d flag). Software-renders the
//! PPU's captured layers into a tilted oblique view: the BG+window composite
//! becomes a flat ground plane and each visible sprite pixel is extruded
//! upward into a short voxel column, so characters stand up off the map.
use gameboy::ppu::{self, Ppu};

/// Ground pixel footprint on screen: 3 wide, 2 tall. The 2/3 vertical squash
/// is the ~35 degree camera pitch.
const CELL_W: usize = 3;
const CELL_H: usize = 2;
/// Horizontal shear per ground row, for the oblique slant.
const SHEAR_DIV: usize = 4;
/// Sprite extrusion height, in cubes of CELL_H screen pixels each.
const COLUMN_H: usize = 5;
const BACKGROUND: u32 = 0x00101018;

const MARGIN_X: usize = 2;
const MARGIN_Y: usize = (HEIGHT - ppu::HEIGHT * CELL_H) / 2 + CELL_H * COLUMN_H / 2;

pub const WIDTH: usize = MARGIN_X * 2 + ppu::WIDTH * CELL_W + (ppu::HEIGHT - 1) / SHEAR_DIV + 1;
pub const HEIGHT: usize = 432;

/// Multiply each RGB channel by num/den, clamped: side faces darken, tops
/// brighten.
fn shade(color: u32, num: u32, den: u32) -> u32 {
    let ch = |c: u32| (c * num / den).min(255);
    ch(color >> 16 & 0xFF) << 16 | ch(color >> 8 & 0xFF) << 8 | ch(color & 0xFF)
}

/// Project a ground-plane pixel to its top-left screen position.
fn project(x: usize, y: usize) -> (usize, usize) {
    (
        MARGIN_X + x * CELL_W + (ppu::HEIGHT - 1 - y) / SHEAR_DIV,
        MARGIN_Y + y * CELL_H,
    )
}

pub struct Renderer {
    pub buffer: Vec<u32>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            buffer: vec![BACKGROUND; WIDTH * HEIGHT],
        }
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for row in y..(y + h).min(HEIGHT) {
            self.buffer[row * WIDTH + x..row * WIDTH + (x + w).min(WIDTH)].fill(color);
        }
    }

    /// Draw one diorama frame from the PPU's captured layers.
    pub fn render(&mut self, ppu: &Ppu) {
        self.buffer.fill(BACKGROUND);
        if ppu.bg_frame.len() < ppu::WIDTH * ppu::HEIGHT {
            return; // no capture yet (first frame)
        }
        // Painter's algorithm, far rows (small y) first: nearer ground rows
        // and the voxel columns standing on them overwrite what's behind.
        for y in 0..ppu::HEIGHT {
            for x in 0..ppu::WIDTH {
                let (sx, sy) = project(x, y);
                self.rect(sx, sy, CELL_W, CELL_H, ppu.bg_frame[y * ppu::WIDTH + x]);
            }
            let lift = COLUMN_H * CELL_H;
            for &(px, py, color) in ppu.sprite_pixels.iter() {
                if py as usize != y {
                    continue;
                }
                let (sx, sy) = project(px as usize, py as usize);
                // Front face of the column, darkened, then the brightened top.
                self.rect(sx, sy + CELL_H - lift, CELL_W, lift, shade(color, 5, 9));
                self.rect(sx, sy - lift, CELL_W, CELL_H, shade(color, 5, 4));
            }
        }
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}
