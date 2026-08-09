/// The picture processing unit, DMG and CGB modes. Draws 160x144 pixels, one
/// scanline at a time, 456 T-cycles per line, 154 lines per frame.
pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 144;

/// DMG green-ish palette, ARGB for minifb.
const DMG_COLORS: [u32; 4] = [0x00E0F8D0, 0x0088C070, 0x00346856, 0x00081820];

/// Convert CGB 15-bit BGR555 palette entries to ARGB.
fn rgb555(lo: u8, hi: u8) -> u32 {
    let c = u16::from_le_bytes([lo, hi]);
    let r = (c & 0x1F) as u32;
    let g = ((c >> 5) & 0x1F) as u32;
    let b = ((c >> 10) & 0x1F) as u32;
    // 5-bit to 8-bit: shift and fill low bits so white is pure white.
    (r << 19 | r >> 2 << 16) | (g << 11 | g >> 2 << 8) | (b << 3 | b >> 2)
}

#[derive(bincode::Encode, bincode::Decode)]
pub struct Ppu {
    pub cgb: bool,
    /// Two 8KB banks; DMG only uses the first.
    pub vram: [u8; 0x4000],
    pub vbk: u8, // FF4F: active VRAM bank
    pub oam: [u8; 0xA0],

    // Registers
    pub lcdc: u8, // FF40 control
    pub stat: u8, // FF41 status
    pub scy: u8,  // FF42 background scroll
    pub scx: u8,  // FF43
    pub ly: u8,   // FF44 current scanline
    pub lyc: u8,  // FF45 scanline compare
    pub bgp: u8,  // FF47 background palette (DMG)
    pub obp0: u8, // FF48 sprite palettes (DMG)
    pub obp1: u8, // FF49
    pub wy: u8,   // FF4A window position
    pub wx: u8,   // FF4B

    // CGB palette RAM: 8 palettes x 4 colors x 2 bytes, for BG and OBJ.
    bg_pal: [u8; 64],
    obj_pal: [u8; 64],
    bgpi: u8, // FF68: index | auto-increment bit
    obpi: u8, // FF6A

    line_cycles: u32,
    window_line: u8, // internal counter: window rendering position

    pub framebuffer: [u32; WIDTH * HEIGHT],
    pub frame_ready: bool,
    /// Interrupt requests for the bus to collect: bit 0 vblank, bit 1 stat.
    pub irq: u8,
}

impl Ppu {
    pub fn new(cgb: bool) -> Self {
        Self {
            cgb,
            vram: [0; 0x4000],
            vbk: 0,
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
            bg_pal: [0xFF; 64],
            obj_pal: [0xFF; 64],
            bgpi: 0,
            obpi: 0,
            line_cycles: 0,
            window_line: 0,
            framebuffer: [DMG_COLORS[0]; WIDTH * HEIGHT],
            frame_ready: false,
            irq: 0,
        }
    }

    pub fn read_vram(&self, addr: u16) -> u8 {
        self.vram[self.vbk as usize * 0x2000 + (addr - 0x8000) as usize]
    }

    pub fn write_vram(&mut self, addr: u16, val: u8) {
        self.vram[self.vbk as usize * 0x2000 + (addr - 0x8000) as usize] = val;
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

    /// Tile pixel lookup in a specific VRAM bank: color index 0-3.
    fn tile_pixel(&self, bank: usize, tile_addr: usize, x: u8, y: u8) -> u8 {
        let base = bank * 0x2000 + tile_addr;
        let lo = self.vram[base + y as usize * 2];
        let hi = self.vram[base + y as usize * 2 + 1];
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

    /// Draw one background/window pixel into the line buffers.
    /// map_x/map_y are coordinates within the 256x256 tilemap.
    fn bg_pixel(&self, map_base: usize, map_x: u8, map_y: u8) -> (u32, u8, bool) {
        let map_idx = map_base + (map_y / 8) as usize * 32 + (map_x / 8) as usize;
        let tile_idx = self.vram[map_idx];
        if self.cgb {
            // Bank 1 holds per-tile attributes at the same map offset.
            let attr = self.vram[0x2000 + map_idx];
            let bank = ((attr >> 3) & 1) as usize;
            let mut tx = map_x % 8;
            let mut ty = map_y % 8;
            if attr & 0x20 != 0 {
                tx = 7 - tx;
            }
            if attr & 0x40 != 0 {
                ty = 7 - ty;
            }
            let ci = self.tile_pixel(bank, self.tile_addr(tile_idx), tx, ty);
            let pal = (attr & 0x07) as usize;
            let p = pal * 8 + ci as usize * 2;
            (
                rgb555(self.bg_pal[p], self.bg_pal[p + 1]),
                ci,
                attr & 0x80 != 0,
            )
        } else {
            let ci = self.tile_pixel(0, self.tile_addr(tile_idx), map_x % 8, map_y % 8);
            let shade = (self.bgp >> (ci * 2)) & 0x03;
            (DMG_COLORS[shade as usize], ci, false)
        }
    }

    fn render_scanline(&mut self) {
        let y = self.ly;
        let mut bg_indices = [0u8; WIDTH]; // pre-palette color index, for sprite priority
        let mut bg_priority = [false; WIDTH]; // CGB per-tile "BG on top" attribute
        let mut colors = [DMG_COLORS[0]; WIDTH];

        // Background. On CGB, LCDC bit 0 changes meaning (BG priority master),
        // but treating it as enable is fine in practice.
        if self.lcdc & 0x01 != 0 || self.cgb {
            let map_base: usize = if self.lcdc & 0x08 != 0 {
                0x1C00
            } else {
                0x1800
            };
            let by = y.wrapping_add(self.scy);
            for x in 0..WIDTH as u8 {
                let bx = x.wrapping_add(self.scx);
                let (color, ci, prio) = self.bg_pixel(map_base, bx, by);
                colors[x as usize] = color;
                bg_indices[x as usize] = ci;
                bg_priority[x as usize] = prio;
            }
        }

        // Window: an opaque layer starting at (WX-7, WY), with its own line counter.
        let mut window_drawn = false;
        if self.lcdc & 0x20 != 0
            && (self.lcdc & 0x01 != 0 || self.cgb)
            && y >= self.wy
            && self.wx < 167
        {
            let map_base: usize = if self.lcdc & 0x40 != 0 {
                0x1C00
            } else {
                0x1800
            };
            let wy = self.window_line;
            let start_x = self.wx.saturating_sub(7);
            for x in start_x..WIDTH as u8 {
                let wx = x + 7 - self.wx;
                let (color, ci, prio) = self.bg_pixel(map_base, wx, wy);
                colors[x as usize] = color;
                bg_indices[x as usize] = ci;
                bg_priority[x as usize] = prio;
                window_drawn = true;
            }
        }
        if window_drawn {
            self.window_line += 1;
        }

        // Sprites (8x8 or 8x16). Hardware draws at most the first 10 sprites
        // on the line in OAM order. Priority: DMG lower X wins (OAM order
        // ties); CGB always OAM order. We draw lowest-priority first so
        // winners overwrite.
        if self.lcdc & 0x02 != 0 {
            let tall = self.lcdc & 0x04 != 0;
            let height = if tall { 16 } else { 8 };
            let mut line_sprites: Vec<usize> = (0..40)
                .filter(|i| y.wrapping_sub(self.oam[i * 4].wrapping_sub(16)) < height)
                .take(10)
                .collect();
            if !self.cgb {
                line_sprites.sort_by_key(|&i| (self.oam[i * 4 + 1], i));
            }
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
                let line = if attr & 0x40 != 0 {
                    height - 1 - line
                } else {
                    line
                }; // Y flip
                let bank = if self.cgb {
                    ((attr >> 3) & 1) as usize
                } else {
                    0
                };
                for px in 0..8u8 {
                    let x = sx.wrapping_add(px);
                    if x as usize >= WIDTH {
                        continue;
                    }
                    let tx = if attr & 0x20 != 0 { 7 - px } else { px }; // X flip
                    let ci = self.tile_pixel(bank, tile as usize * 16, tx, line);
                    if ci == 0 {
                        continue; // color 0 is transparent for sprites
                    }
                    let behind = (attr & 0x80 != 0 || bg_priority[x as usize])
                        && bg_indices[x as usize] != 0;
                    if behind {
                        continue;
                    }
                    colors[x as usize] = if self.cgb {
                        let p = (attr & 0x07) as usize * 8 + ci as usize * 2;
                        rgb555(self.obj_pal[p], self.obj_pal[p + 1])
                    } else {
                        let palette = if attr & 0x10 != 0 {
                            self.obp1
                        } else {
                            self.obp0
                        };
                        let shade = (palette >> (ci * 2)) & 0x03;
                        DMG_COLORS[shade as usize]
                    };
                }
            }
        }

        self.framebuffer[y as usize * WIDTH..(y as usize + 1) * WIDTH].copy_from_slice(&colors);
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
            0xFF4F => self.vbk | 0xFE,
            0xFF68 => self.bgpi,
            0xFF69 => self.bg_pal[(self.bgpi & 0x3F) as usize],
            0xFF6A => self.obpi,
            0xFF6B => self.obj_pal[(self.obpi & 0x3F) as usize],
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
            0xFF4F => self.vbk = val & if self.cgb { 1 } else { 0 },
            0xFF68 => self.bgpi = val & 0xBF,
            0xFF69 => {
                self.bg_pal[(self.bgpi & 0x3F) as usize] = val;
                if self.bgpi & 0x80 != 0 {
                    self.bgpi = 0x80 | (self.bgpi + 1) & 0x3F;
                }
            }
            0xFF6A => self.obpi = val & 0xBF,
            0xFF6B => {
                self.obj_pal[(self.obpi & 0x3F) as usize] = val;
                if self.obpi & 0x80 != 0 {
                    self.obpi = 0x80 | (self.obpi + 1) & 0x3F;
                }
            }
            _ => {}
        }
    }
}
