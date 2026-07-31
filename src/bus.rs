use crate::cartridge::Cartridge;
use crate::ppu::Ppu;

/// The DMG memory bus. One flat 64KB address space:
///   0000-7FFF  cartridge ROM (through the MBC)
///   8000-9FFF  VRAM (PPU)
///   A000-BFFF  cartridge RAM
///   C000-DFFF  work RAM
///   E000-FDFF  echo of work RAM
///   FE00-FE9F  OAM (PPU)
///   FF00-FF7F  I/O registers
///   FF80-FFFE  high RAM
///   FFFF       interrupt enable
pub struct Bus {
    cart: Cartridge,
    pub ppu: Ppu,
    wram: [u8; 0x2000],
    hram: [u8; 0x7F],
    io: [u8; 0x80],
    pub ie: u8,
    pub if_reg: u8,

    // Joypad: active-low button states, updated by the frontend.
    // Bit layout matches FF00's low nibble for each select group.
    pub joy_buttons: u8, // start, select, B, A (bits 3-0)
    pub joy_dpad: u8,    // down, up, left, right (bits 3-0)
    joyp_select: u8,

    // Timer. div_counter runs at 4 MHz (T-cycles); DIV is its high byte.
    div_counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
}

impl Bus {
    pub fn new(cart: Cartridge) -> Self {
        Self {
            cart,
            ppu: Ppu::new(),
            wram: [0; 0x2000],
            hram: [0; 0x7F],
            io: [0; 0x80],
            ie: 0,
            if_reg: 0xE1,
            joy_buttons: 0x0F,
            joy_dpad: 0x0F,
            joyp_select: 0x30,
            div_counter: 0,
            tima: 0,
            tma: 0,
            tac: 0,
        }
    }

    pub fn save_cart(&mut self) {
        self.cart.save();
    }

    /// Advance timer and PPU by the given number of T-cycles.
    pub fn tick(&mut self, tcycles: u32) {
        self.ppu.tick(tcycles);
        if self.ppu.irq != 0 {
            self.if_reg |= self.ppu.irq;
            self.ppu.irq = 0;
        }

        for _ in 0..tcycles {
            let old = self.div_counter;
            self.div_counter = self.div_counter.wrapping_add(1);
            if self.tac & 0x04 != 0 {
                // Falling edge of the selected DIV bit increments TIMA.
                let bit = match self.tac & 0x03 {
                    0b00 => 9, // 4096 Hz
                    0b01 => 3, // 262144 Hz
                    0b10 => 5, // 65536 Hz
                    _ => 7,    // 16384 Hz
                };
                if old & (1 << bit) != 0 && self.div_counter & (1 << bit) == 0 {
                    let (t, overflow) = self.tima.overflowing_add(1);
                    self.tima = if overflow { self.tma } else { t };
                    if overflow {
                        self.if_reg |= 0x04; // timer interrupt
                    }
                }
            }
        }
    }

    fn joyp(&self) -> u8 {
        let mut v = 0xC0 | self.joyp_select | 0x0F;
        if self.joyp_select & 0x10 == 0 {
            v &= 0xF0 | self.joy_dpad;
        }
        if self.joyp_select & 0x20 == 0 {
            v &= 0xF0 | self.joy_buttons;
        }
        v
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cart.read(addr),
            0x8000..=0x9FFF => self.ppu.vram[(addr - 0x8000) as usize],
            0xA000..=0xBFFF => self.cart.read_ram(addr),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize],
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0xFF, // unusable region
            0xFF00 => self.joyp(),
            0xFF04 => (self.div_counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => self.tac | 0xF8,
            0xFF0F => self.if_reg | 0xE0,
            0xFF40..=0xFF4B => self.ppu.read(addr),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.ie,
        }
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x7FFF => self.cart.write(addr, val),
            0x8000..=0x9FFF => self.ppu.vram[(addr - 0x8000) as usize] = val,
            0xA000..=0xBFFF => self.cart.write_ram(addr, val),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize] = val,
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize] = val,
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize] = val,
            0xFEA0..=0xFEFF => {}
            0xFF00 => self.joyp_select = val & 0x30,
            // Serial transfer: Blargg's test ROMs print results here.
            // Writing 0x81 to SC (FF02) sends the byte in SB (FF01).
            0xFF02 if val == 0x81 => {
                print!("{}", self.io[0x01] as char);
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            0xFF04 => self.div_counter = 0,
            0xFF05 => self.tima = val,
            0xFF06 => self.tma = val,
            0xFF07 => self.tac = val & 0x07,
            0xFF0F => self.if_reg = val & 0x1F,
            // OAM DMA: copy 160 bytes from val<<8 into OAM. Real hardware
            // takes 160 cycles; instant is fine for games.
            0xFF46 => {
                let src = (val as u16) << 8;
                for i in 0..0xA0 {
                    let b = self.read(src + i);
                    self.ppu.oam[i as usize] = b;
                }
            }
            0xFF40..=0xFF4B => self.ppu.write(addr, val),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize] = val,
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = val,
            0xFFFF => self.ie = val,
        }
    }
}
