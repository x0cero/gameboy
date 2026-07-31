use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::ppu::Ppu;

/// The memory bus, DMG and CGB. One flat 64KB address space:
///   0000-7FFF  cartridge ROM (through the MBC)
///   8000-9FFF  VRAM (PPU, 2 banks on CGB)
///   A000-BFFF  cartridge RAM
///   C000-DFFF  work RAM (bank 0 + switchable bank 1-7 on CGB)
///   E000-FDFF  echo of work RAM
///   FE00-FE9F  OAM (PPU)
///   FF00-FF7F  I/O registers
///   FF80-FFFE  high RAM
///   FFFF       interrupt enable
#[derive(bincode::Encode, bincode::Decode)]
pub struct Bus {
    cart: Cartridge,
    pub ppu: Ppu,
    pub apu: Apu,
    pub cgb: bool,
    wram: [u8; 0x8000], // 8 banks of 4KB; DMG uses the first two
    svbk: u8,           // FF70: WRAM bank select (CGB)
    hram: [u8; 0x7F],
    io: [u8; 0x80],
    pub ie: u8,
    pub if_reg: u8,

    /// FF4D KEY1: bit 7 = current speed, bit 0 = switch armed. CPU handles STOP.
    pub key1: u8,
    hdma_src: u16,
    hdma_dst: u16,

    // Joypad: active-low button states, updated by the frontend.
    pub joy_buttons: u8, // start, select, B, A (bits 3-0)
    pub joy_dpad: u8,    // down, up, left, right (bits 3-0)
    joyp_select: u8,

    // Timer. div_counter runs on the CPU clock; DIV is its high byte.
    div_counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
}

impl Bus {
    pub fn new(cart: Cartridge) -> Self {
        let cgb = cart.cgb();
        Self {
            ppu: Ppu::new(cgb),
            apu: Apu::new(),
            cart,
            cgb,
            wram: [0; 0x8000],
            svbk: 1,
            hram: [0; 0x7F],
            io: [0; 0x80],
            ie: 0,
            if_reg: 0xE1,
            key1: 0,
            hdma_src: 0,
            hdma_dst: 0,
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

    fn wram_idx(&self, addr: u16) -> usize {
        let rel = (addr as usize - 0xC000) & 0x1FFF;
        if rel < 0x1000 {
            rel
        } else {
            let bank = if self.cgb { (self.svbk & 0x07).max(1) as usize } else { 1 };
            bank * 0x1000 + (rel - 0x1000)
        }
    }

    /// Advance timer and PPU by the given number of CPU T-cycles. In double
    /// speed mode the CPU clock doubles but the PPU doesn't, so it gets half.
    pub fn tick(&mut self, tcycles: u32) {
        let ppu_cycles = if self.key1 & 0x80 != 0 { tcycles / 2 } else { tcycles };
        self.ppu.tick(ppu_cycles);
        self.apu.tick(ppu_cycles);
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
            0x8000..=0x9FFF => self.ppu.read_vram(addr),
            0xA000..=0xBFFF => self.cart.read_ram(addr),
            0xC000..=0xDFFF | 0xE000..=0xFDFF => self.wram[self.wram_idx(addr)],
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0xFF, // unusable region
            0xFF00 => self.joyp(),
            0xFF04 => (self.div_counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => self.tac | 0xF8,
            0xFF0F => self.if_reg | 0xE0,
            0xFF10..=0xFF3F => self.apu.read(addr),
            0xFF4D if self.cgb => self.key1 | 0x7E,
            0xFF55 => 0xFF, // HDMA status: always "done" (transfers are instant)
            0xFF70 if self.cgb => self.svbk | 0xF8,
            0xFF40..=0xFF4B | 0xFF4F | 0xFF68..=0xFF6B => self.ppu.read(addr),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.ie,
        }
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x7FFF => self.cart.write(addr, val),
            0x8000..=0x9FFF => self.ppu.write_vram(addr, val),
            0xA000..=0xBFFF => self.cart.write_ram(addr, val),
            0xC000..=0xDFFF | 0xE000..=0xFDFF => {
                let i = self.wram_idx(addr);
                self.wram[i] = val;
            }
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize] = val,
            0xFEA0..=0xFEFF => {}
            0xFF00 => self.joyp_select = val & 0x30,
            // Serial transfer: Blargg's test ROMs print results here.
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
            0xFF10..=0xFF3F => self.apu.write(addr, val),
            // OAM DMA: copy 160 bytes from val<<8 into OAM. Instant.
            0xFF46 => {
                let src = (val as u16) << 8;
                for i in 0..0xA0 {
                    let b = self.read(src + i);
                    self.ppu.oam[i as usize] = b;
                }
            }
            0xFF4D if self.cgb => self.key1 = (self.key1 & 0x80) | (val & 0x01),
            // CGB VRAM DMA (FF51-FF55). Both general and hblank transfers are
            // performed instantly on the FF55 write; games poll FF55 and see
            // "done" immediately.
            0xFF51 => self.hdma_src = (self.hdma_src & 0x00FF) | ((val as u16) << 8),
            0xFF52 => self.hdma_src = (self.hdma_src & 0xFF00) | (val & 0xF0) as u16,
            0xFF53 => self.hdma_dst = (self.hdma_dst & 0x00FF) | (((val & 0x1F) as u16) << 8),
            0xFF54 => self.hdma_dst = (self.hdma_dst & 0xFF00) | (val & 0xF0) as u16,
            0xFF55 if self.cgb => {
                let len = ((val as u16 & 0x7F) + 1) * 0x10;
                for i in 0..len {
                    let b = self.read(self.hdma_src.wrapping_add(i));
                    self.ppu.write_vram(0x8000 + ((self.hdma_dst + i) & 0x1FFF), b);
                }
                self.hdma_src = self.hdma_src.wrapping_add(len);
                self.hdma_dst = (self.hdma_dst + len) & 0x1FFF;
            }
            0xFF70 if self.cgb => self.svbk = val & 0x07,
            0xFF40..=0xFF4B | 0xFF4F | 0xFF68..=0xFF6B => self.ppu.write(addr, val),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize] = val,
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = val,
            0xFFFF => self.ie = val,
        }
    }
}
