use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[derive(PartialEq, Clone, Copy, bincode::Encode, bincode::Decode)]
enum Mapper {
    None,
    Mbc1,
    Mbc3,
    Mbc5,
}

#[derive(bincode::Encode, bincode::Decode)]
pub struct Cartridge {
    pub rom: Vec<u8>,
    ram: Vec<u8>,
    mapper: Mapper,
    has_battery: bool,
    save_path: String,
    rom_bank: usize,
    ram_bank: usize,
    ram_enabled: bool,
    pub ram_dirty: bool,

    // MBC3 real-time clock. rtc_base is the wall-clock unix time at which
    // the counter read zero; registers are derived from (now - rtc_base).
    // Not persisted across runs (starts from load time), which most games
    // treat as "time passed while you were away".
    rtc_base: u64,
    rtc_latched: [u8; 5],
    rtc_halt: bool,
}

impl Cartridge {
    pub fn load(path: &str) -> io::Result<Self> {
        let rom = fs::read(path)?;
        if rom.len() < 0x150 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ROM smaller than header"));
        }

        // Header 0x147: cartridge type (mapper + peripherals).
        let cart_type = rom[0x147];
        let mapper = match cart_type {
            0x00 | 0x08 | 0x09 => Mapper::None,
            0x01..=0x03 => Mapper::Mbc1,
            0x0F..=0x13 => Mapper::Mbc3,
            0x19..=0x1E => Mapper::Mbc5,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported cartridge type {other:#04X}"),
                ))
            }
        };
        let has_battery = matches!(cart_type, 0x03 | 0x06 | 0x09 | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E);

        // Header 0x149: RAM size.
        let ram_size = match rom[0x149] {
            0x02 => 0x2000,
            0x03 => 0x8000,
            0x04 => 0x20000,
            0x05 => 0x10000,
            _ => 0x2000, // none declared; keep a bank anyway for sloppy homebrew
        };

        let save_path = std::path::PathBuf::from(path).with_extension("sav");
        let ram = if has_battery {
            fs::read(&save_path).ok().filter(|d| d.len() == ram_size)
        } else {
            None
        }
        .unwrap_or_else(|| vec![0; ram_size]);

        Ok(Self {
            rom,
            ram,
            mapper,
            has_battery,
            save_path: save_path.to_string_lossy().into_owned(),
            rom_bank: 1,
            ram_bank: 0,
            ram_enabled: false,
            ram_dirty: false,
            rtc_base: unix_now(),
            rtc_latched: [0; 5],
            rtc_halt: false,
        })
    }

    /// Header 0x143 bit 7: cartridge supports (or requires) CGB mode.
    pub fn cgb(&self) -> bool {
        self.rom[0x143] & 0x80 != 0
    }

    pub fn title(&self) -> String {
        self.rom[0x134..0x144]
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect()
    }

    /// Persist battery-backed RAM next to the ROM as <rom>.sav.
    pub fn save(&mut self) {
        if self.has_battery && self.ram_dirty {
            if let Err(e) = fs::write(&self.save_path, &self.ram) {
                eprintln!("failed to write save file: {e}");
            }
            self.ram_dirty = false;
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        let idx = match addr {
            0x0000..=0x3FFF => addr as usize,
            _ => self.rom_bank * 0x4000 + (addr as usize - 0x4000),
        };
        *self.rom.get(idx).unwrap_or(&0xFF)
    }

    /// Current RTC counter in seconds.
    fn rtc_secs(&self) -> u64 {
        if self.rtc_halt {
            self.rtc_base // while halted, rtc_base stores the frozen counter
        } else {
            unix_now() - self.rtc_base
        }
    }

    fn rtc_regs(&self) -> [u8; 5] {
        let t = self.rtc_secs();
        let days = t / 86400;
        [
            (t % 60) as u8,
            (t / 60 % 60) as u8,
            (t / 3600 % 24) as u8,
            (days & 0xFF) as u8,
            (((days >> 8) & 1) as u8)
                | if self.rtc_halt { 0x40 } else { 0 }
                | if days > 511 { 0x80 } else { 0 },
        ]
    }

    pub fn read_ram(&self, addr: u16) -> u8 {
        if !self.ram_enabled {
            return 0xFF;
        }
        if self.mapper == Mapper::Mbc3 && (0x08..=0x0C).contains(&self.ram_bank) {
            return self.rtc_latched[self.ram_bank - 8];
        }
        if self.ram.is_empty() {
            return 0xFF;
        }
        let idx = (self.ram_bank * 0x2000 + (addr as usize - 0xA000)) % self.ram.len();
        self.ram[idx]
    }

    pub fn write_ram(&mut self, addr: u16, val: u8) {
        if self.ram_enabled && self.mapper == Mapper::Mbc3 && (0x08..=0x0C).contains(&self.ram_bank) {
            // Writing the clock: rebuild the counter with this register changed.
            let mut r = self.rtc_regs();
            r[self.ram_bank - 8] = val;
            let secs = r[0] as u64 % 60
                + r[1] as u64 % 60 * 60
                + r[2] as u64 % 24 * 3600
                + (r[3] as u64 + ((r[4] as u64 & 1) << 8)) * 86400;
            self.rtc_halt = r[4] & 0x40 != 0;
            self.rtc_base = if self.rtc_halt { secs } else { unix_now() - secs };
            return;
        }
        if self.ram_enabled && !self.ram.is_empty() {
            let idx = (self.ram_bank * 0x2000 + (addr as usize - 0xA000)) % self.ram.len();
            self.ram[idx] = val;
            self.ram_dirty = true;
        }
    }

    /// Writes into ROM space program the mapper's bank registers.
    pub fn write(&mut self, addr: u16, val: u8) {
        match self.mapper {
            Mapper::None => {}
            Mapper::Mbc1 => match addr {
                0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
                0x2000..=0x3FFF => {
                    // 5-bit ROM bank select; 0 is treated as 1.
                    let bank = (val & 0x1F).max(1) as usize;
                    self.rom_bank = (self.rom_bank & !0x1F) | bank;
                }
                0x4000..=0x5FFF => self.ram_bank = (val & 0x03) as usize,
                _ => {} // banking mode select: not needed for small carts
            },
            Mapper::Mbc3 => match addr {
                0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
                0x2000..=0x3FFF => self.rom_bank = (val & 0x7F).max(1) as usize,
                0x4000..=0x5FFF => self.ram_bank = (val & 0x0F) as usize,
                // Latch: snapshot the running clock into the readable registers.
                0x6000..=0x7FFF => {
                    if val & 1 != 0 {
                        self.rtc_latched = self.rtc_regs();
                    }
                }
                _ => {}
            },
            Mapper::Mbc5 => match addr {
                0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
                0x2000..=0x2FFF => self.rom_bank = (self.rom_bank & 0x100) | val as usize,
                0x3000..=0x3FFF => {
                    self.rom_bank = (self.rom_bank & 0xFF) | ((val as usize & 1) << 8)
                }
                0x4000..=0x5FFF => self.ram_bank = (val & 0x0F) as usize,
                _ => {}
            },
        }
    }
}
