/// The DMG picture processing unit. Draws 160x144 pixels, one scanline at a
/// time, 456 T-cycles per line, 154 lines per frame (144 visible + 10 vblank).
pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 144;

/// DMG green-ish palette, ARGB for minifb.
const COLORS: [u32; 4] = [0x00E0F8D0, 0x0088C070, 0x00346856, 0x00081820];

pub struct Ppu {
    pub vram: [u8; 0x2000],
    pub oam: [u8; 0xA0],

    // Registers
    pub lcdc: u8, // FF40 control
    pub stat: u8, // FF41 status
    pub scy: u8,  // FF42 background scroll
    pub scx: u8,  // FF43
    pub ly: u8,   // FF44 current scanline
    pub lyc: u8,  // FF45 scanline compare
    pub bgp: u8,  // FF47 background palette
    pub obp0: u8, // FF48 sprite palettes
    pub obp1: u8, // FF49
    pub wy: u8,   // FF4A window position
    pub wx: u8,   // FF4B

    line_cycles: u32,
    window_line: u8, // internal counter: window rendering position

    pub framebuffer: [u32; WIDTH * HEIGHT],
    pub frame_ready: bool,
    /// Interrupt requests for the bus to collect: bit 0 vblank, bit 1 stat.
    pub irq: u8,
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            vram: [0; 0x2000],
            oam: [0; 0xA0],
            lcdc: 0x91,
            stat: 0x85,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            line_cycles: 0,
            window_line: 0,
            framebuffer: [COLORS[0]; WIDTH * HEIGHT],
            frame_ready: false,
            irq: 0,
        }
    }

    fn set_mode(&mut self, mode: u8) {
        self.stat = (self.stat & !0x03) | mode;
        // STAT interrupt on entering a mode whose enable bit is set.
        let enable_bit = match mode {
            0 => 0x08,
            1 => 0x10,
            2 => 0x20,
            _ => return,
        };
        if self.stat & enable_bit != 0 {
            self.irq |= 0x02;
        }
    }

    fn check_lyc(&mut self) {
        if self.ly == self.lyc {
            self.stat |= 0x04;
            if self.stat & 0x40 != 0 {
                self.irq |= 0x02;
            }
        } else {
            self.stat &= !0x04;
        }
    }

    /// Advance the PPU by the given number of T-cycles.
    pub fn tick(&mut self, tcycles: u32) {
        if self.lcdc & 0x80 == 0 {
            // LCD off: LY pinned to 0, mode 0.
            self.ly = 0;
            self.line_cycles = 0;
            self.stat &= !0x03;
            return;
        }

        self.line_cycles += tcycles;
        while self.line_cycles >= 456 {
            self.line_cycles -= 456;
            if self.ly < 144 {
                self.render_scanline();
            }
            self.ly += 1;
            match self.ly {
                144 => {
                    self.irq |= 0x01; // vblank interrupt
                    self.set_mode(1);
                    self.frame_ready = true;
                }
                154.. => {
                    self.ly = 0;
                    self.window_line = 0;
                    self.set_mode(2);
                }
                _ if self.ly < 144 => self.set_mode(2),
                _ => {}
            }
            self.check_lyc();
        }

        // Mode within a visible line: 2 (OAM scan) 0-80, 3 (draw) 80-252, 0 (hblank) rest.
        if self.ly < 144 {
            let mode = match self.line_cycles {
                0..=79 => 2,
                80..=251 => 3,
                _ => 0,
            };
            if self.stat & 0x03 != mode {
                self.set_mode(mode);
            }
        }
    }

    /// Tile pixel lookup: returns color index 0-3 for a tile row byte pair.
    fn tile_pixel(&self, tile_addr: usize, x: u8, y: u8) -> u8 {
        let lo = self.vram[tile_addr + y as usize * 2];
        let hi = self.vram[tile_addr + y as usize * 2 + 1];
        let bit = 7 - x;
        ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1)
    }

    /// Resolve a tile index from a tilemap into a VRAM address, honoring the
    /// LCDC bit 4 addressing mode (0x8000 unsigned vs 0x8800 signed).
    fn tile_addr(&self, index: u8) -> usize {
        if self.lcdc & 0x10 != 0 {
            index as usize * 16
        } else {
            (0x1000i32 + (index as i8 as i32) * 16) as usize
        }
    }

    fn render_scanline(&mut self) {
        let y = self.ly;
        let mut bg_indices = [0u8; WIDTH]; // color indices before palette, for sprite priority

        // Background
        if self.lcdc & 0x01 != 0 {
            let map_base: usize = if self.lcdc & 0x08 != 0 { 0x1C00 } else { 0x1800 };
            let by = y.wrapping_add(self.scy);
            for x in 0..WIDTH as u8 {
                let bx = x.wrapping_add(self.scx);
                let tile_idx = self.vram[map_base + (by / 8) as usize * 32 + (bx / 8) as usize];
                let ci = self.tile_pixel(self.tile_addr(tile_idx), bx % 8, by % 8);
                bg_indices[x as usize] = ci;
            }
        }

        // Window: an opaque layer starting at (WX-7, WY), using its own line counter.
        let mut window_drawn = false;
        if self.lcdc & 0x21 == 0x21 && y >= self.wy && self.wx < 167 {
            let map_base: usize = if self.lcdc & 0x40 != 0 { 0x1C00 } else { 0x1800 };
            let wy = self.window_line;
            let start_x = self.wx.saturating_sub(7);
            for x in start_x..WIDTH as u8 {
                let wx = x + 7 - self.wx;
                let tile_idx = self.vram[map_base + (wy / 8) as usize * 32 + (wx / 8) as usize];
                let ci = self.tile_pixel(self.tile_addr(tile_idx), wx % 8, wy % 8);
                bg_indices[x as usize] = ci;
                window_drawn = true;
            }
        }
        if window_drawn {
            self.window_line += 1;
        }

        // Apply background palette
        let row = &mut self.framebuffer[y as usize * WIDTH..(y as usize + 1) * WIDTH];
        for (x, px) in row.iter_mut().enumerate() {
            let shade = (self.bgp >> (bg_indices[x] * 2)) & 0x03;
            *px = COLORS[shade as usize];
        }

        // Sprites (8x8 or 8x16). Hardware draws at most the first 10 sprites
        // on the line in OAM order; among those, lower X wins priority (OAM
        // order breaks ties). We draw lowest-priority first so winners
        // overwrite.
        if self.lcdc & 0x02 != 0 {
            let tall = self.lcdc & 0x04 != 0;
            let height = if tall { 16 } else { 8 };
            let mut line_sprites: Vec<usize> = (0..40)
                .filter(|i| y.wrapping_sub(self.oam[i * 4].wrapping_sub(16)) < height)
                .take(10)
                .collect();
            line_sprites.sort_by_key(|&i| (self.oam[i * 4 + 1], i));
            for &i in line_sprites.iter().rev() {
                let e = i * 4;
                let sy = self.oam[e].wrapping_sub(16);
                let sx = self.oam[e + 1].wrapping_sub(8);
                let mut tile = self.oam[e + 2];
                let attr = self.oam[e + 3];
                if tall {
                    tile &= 0xFE;
                }
                let line = y.wrapping_sub(sy);
                let line = if attr & 0x40 != 0 { height - 1 - line } else { line }; // Y flip
                let palette = if attr & 0x10 != 0 { self.obp1 } else { self.obp0 };
                for px in 0..8u8 {
                    let x = sx.wrapping_add(px);
                    if x as usize >= WIDTH {
                        continue;
                    }
                    let tx = if attr & 0x20 != 0 { 7 - px } else { px }; // X flip
                    let ci = self.tile_pixel(tile as usize * 16, tx, line);
                    if ci == 0 {
                        continue; // color 0 is transparent for sprites
                    }
                    if attr & 0x80 != 0 && bg_indices[x as usize] != 0 {
                        continue; // behind non-zero background
                    }
                    let shade = (palette >> (ci * 2)) & 0x03;
                    self.framebuffer[y as usize * WIDTH + x as usize] = COLORS[shade as usize];
                }
            }
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => self.stat | 0x80,
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF40 => self.lcdc = val,
            0xFF41 => self.stat = (self.stat & 0x07) | (val & 0x78),
            0xFF42 => self.scy = val,
            0xFF43 => self.scx = val,
            0xFF44 => {} // LY is read-only
            0xFF45 => self.lyc = val,
            0xFF47 => self.bgp = val,
            0xFF48 => self.obp0 = val,
            0xFF49 => self.obp1 = val,
            0xFF4A => self.wy = val,
            0xFF4B => self.wx = val,
            _ => {}
        }
    }
}
