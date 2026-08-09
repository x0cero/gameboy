/// The audio processing unit: two square channels (ch1 has a frequency
/// sweep), a 32-sample wavetable channel, and an LFSR noise channel. A frame
/// sequencer at 512 Hz clocks length counters, envelopes, and the sweep.
/// Output is stereo f32 at SAMPLE_RATE, resampled by simple decimation.
pub const SAMPLE_RATE: u32 = 44100;
const CPU_HZ: u32 = 4_194_304;

const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

#[derive(Default, bincode::Encode, bincode::Decode)]
struct Square {
    enabled: bool,
    dac: bool,
    duty: u8,
    duty_pos: u8,
    freq: u16, // 11-bit period value from NRx3/NRx4
    timer: i32,
    length: u16,
    length_enable: bool,
    volume: u8,
    env_vol: u8,
    env_add: bool,
    env_period: u8,
    env_timer: u8,
    // Channel 1 sweep
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    sweep_shadow: u16,
}

impl Square {
    fn trigger(&mut self, has_sweep: bool) {
        self.enabled = self.dac;
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = (2048 - self.freq as i32) * 4;
        self.env_vol = self.volume;
        self.env_timer = self.env_period;
        if has_sweep {
            self.sweep_shadow = self.freq;
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            if self.sweep_shift != 0 && self.sweep_next() > 2047 {
                self.enabled = false;
            }
        }
    }

    fn sweep_next(&self) -> u32 {
        let d = self.sweep_shadow >> self.sweep_shift;
        if self.sweep_negate {
            (self.sweep_shadow - d) as u32
        } else {
            self.sweep_shadow as u32 + d as u32
        }
    }

    fn clock_sweep(&mut self) {
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            if self.sweep_enabled && self.sweep_period != 0 {
                let next = self.sweep_next();
                if next > 2047 {
                    self.enabled = false;
                } else if self.sweep_shift != 0 {
                    self.sweep_shadow = next as u16;
                    self.freq = next as u16;
                    if self.sweep_next() > 2047 {
                        self.enabled = false;
                    }
                }
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_env(&mut self) {
        if self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
        }
        if self.env_timer == 0 {
            self.env_timer = self.env_period;
            if self.env_add && self.env_vol < 15 {
                self.env_vol += 1;
            } else if !self.env_add && self.env_vol > 0 {
                self.env_vol -= 1;
            }
        }
    }

    fn tick(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += (2048 - self.freq as i32) * 4;
            self.duty_pos = (self.duty_pos + 1) & 7;
        }
    }

    fn output(&self) -> u8 {
        if self.enabled && self.dac {
            DUTY[self.duty as usize][self.duty_pos as usize] * self.env_vol
        } else {
            0
        }
    }
}

#[derive(Default, bincode::Encode, bincode::Decode)]
struct Wave {
    enabled: bool,
    dac: bool,
    freq: u16,
    timer: i32,
    length: u16,
    length_enable: bool,
    volume_code: u8,
    pos: u8,
    sample: u8,
}

#[derive(Default, bincode::Encode, bincode::Decode)]
struct Noise {
    enabled: bool,
    dac: bool,
    length: u16,
    length_enable: bool,
    volume: u8,
    env_vol: u8,
    env_add: bool,
    env_period: u8,
    env_timer: u8,
    divisor_code: u8,
    width7: bool,
    shift: u8,
    timer: i32,
    lfsr: u16,
}

impl Noise {
    fn period(&self) -> i32 {
        let d = if self.divisor_code == 0 {
            8
        } else {
            self.divisor_code as i32 * 16
        };
        d << self.shift
    }

    fn tick(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period();
            let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 14);
            if self.width7 {
                self.lfsr = (self.lfsr & !(1 << 6)) | (bit << 6);
            }
        }
    }

    fn output(&self) -> u8 {
        if self.enabled && self.dac && self.lfsr & 1 == 0 {
            self.env_vol
        } else {
            0
        }
    }
}

#[derive(bincode::Encode, bincode::Decode)]
pub struct Apu {
    ch1: Square,
    ch2: Square,
    ch3: Wave,
    ch4: Noise,
    wave_ram: [u8; 16],
    power: bool,
    nr50: u8,
    nr51: u8,

    frame_timer: u32,
    frame_step: u8,
    sample_timer: u32,
    /// Stereo interleaved samples for the frontend to drain.
    pub samples: Vec<f32>,
    regs: [u8; 0x30], // raw register bytes for reads
}

impl Apu {
    pub fn new() -> Self {
        Self {
            ch1: Square::default(),
            ch2: Square::default(),
            ch3: Wave::default(),
            ch4: Noise {
                lfsr: 0x7FFF,
                ..Default::default()
            },
            wave_ram: [0; 16],
            power: true,
            nr50: 0x77,
            nr51: 0xF3,
            frame_timer: 0,
            frame_step: 0,
            sample_timer: 0,
            samples: Vec::new(),
            regs: [0; 0x30],
        }
    }

    pub fn tick(&mut self, tcycles: u32) {
        if !self.power {
            // Still produce silence so the audio stream doesn't starve.
            self.sample_timer += tcycles * SAMPLE_RATE;
            while self.sample_timer >= CPU_HZ {
                self.sample_timer -= CPU_HZ;
                self.samples.push(0.0);
                self.samples.push(0.0);
            }
            return;
        }
        let c = tcycles as i32;
        self.ch1.tick(c);
        self.ch2.tick(c);
        self.ch4.tick(c);

        // Wave channel: period (2048-f)*2, steps through 32 4-bit samples.
        self.ch3.timer -= c;
        while self.ch3.timer <= 0 {
            self.ch3.timer += (2048 - self.ch3.freq as i32) * 2;
            self.ch3.pos = (self.ch3.pos + 1) & 31;
            let byte = self.wave_ram[(self.ch3.pos / 2) as usize];
            self.ch3.sample = if self.ch3.pos & 1 == 0 {
                byte >> 4
            } else {
                byte & 0x0F
            };
        }

        // Frame sequencer: 512 Hz.
        self.frame_timer += tcycles;
        while self.frame_timer >= 8192 {
            self.frame_timer -= 8192;
            match self.frame_step {
                0 | 4 => self.clock_lengths(),
                2 | 6 => {
                    self.clock_lengths();
                    self.ch1.clock_sweep();
                }
                7 => {
                    self.ch1.clock_env();
                    self.ch2.clock_env();
                    let n = &mut self.ch4;
                    if n.env_period > 0 {
                        if n.env_timer > 0 {
                            n.env_timer -= 1;
                        }
                        if n.env_timer == 0 {
                            n.env_timer = n.env_period;
                            if n.env_add && n.env_vol < 15 {
                                n.env_vol += 1;
                            } else if !n.env_add && n.env_vol > 0 {
                                n.env_vol -= 1;
                            }
                        }
                    }
                }
                _ => {}
            }
            self.frame_step = (self.frame_step + 1) & 7;
        }

        // Decimating resampler: emit one stereo frame every CPU_HZ/SAMPLE_RATE cycles.
        self.sample_timer += tcycles * SAMPLE_RATE;
        while self.sample_timer >= CPU_HZ {
            self.sample_timer -= CPU_HZ;
            let (l, r) = self.mix();
            self.samples.push(l);
            self.samples.push(r);
        }
    }

    fn clock_lengths(&mut self) {
        self.ch1.clock_length();
        self.ch2.clock_length();
        if self.ch3.length_enable && self.ch3.length > 0 {
            self.ch3.length -= 1;
            if self.ch3.length == 0 {
                self.ch3.enabled = false;
            }
        }
        if self.ch4.length_enable && self.ch4.length > 0 {
            self.ch4.length -= 1;
            if self.ch4.length == 0 {
                self.ch4.enabled = false;
            }
        }
    }

    fn mix(&self) -> (f32, f32) {
        let wave_out = if self.ch3.enabled && self.ch3.dac {
            match self.ch3.volume_code {
                0 => 0,
                1 => self.ch3.sample,
                2 => self.ch3.sample >> 1,
                _ => self.ch3.sample >> 2,
            }
        } else {
            0
        };
        let outs = [
            self.ch1.output(),
            self.ch2.output(),
            wave_out,
            self.ch4.output(),
        ];
        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for (i, &o) in outs.iter().enumerate() {
            // DAC maps 0-15 to +1..-1; approximate with centered scale.
            let v = o as f32 / 15.0 * 2.0 - 1.0;
            let v = if o == 0 { 0.0 } else { v * 0.25 };
            if self.nr51 & (1 << (4 + i)) != 0 {
                l += v;
            }
            if self.nr51 & (1 << i) != 0 {
                r += v;
            }
        }
        let lv = ((self.nr50 >> 4) & 7) as f32 + 1.0;
        let rv = (self.nr50 & 7) as f32 + 1.0;
        (l * lv / 8.0, r * rv / 8.0)
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF26 => {
                let mut v = 0x70;
                if self.power {
                    v |= 0x80;
                }
                v |= self.ch1.enabled as u8;
                v |= (self.ch2.enabled as u8) << 1;
                v |= (self.ch3.enabled as u8) << 2;
                v |= (self.ch4.enabled as u8) << 3;
                v
            }
            0xFF30..=0xFF3F => self.wave_ram[(addr - 0xFF30) as usize],
            0xFF10..=0xFF2F => self.regs[(addr - 0xFF10) as usize],
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        if addr == 0xFF26 {
            self.power = val & 0x80 != 0;
            if !self.power {
                *self = Apu {
                    samples: std::mem::take(&mut self.samples),
                    ..Apu::new()
                };
                self.power = false;
            }
            return;
        }
        if let 0xFF30..=0xFF3F = addr {
            self.wave_ram[(addr - 0xFF30) as usize] = val;
            return;
        }
        if !self.power {
            return;
        }
        self.regs[(addr - 0xFF10) as usize] = val;
        match addr {
            // Channel 1: sweep square
            0xFF10 => {
                self.ch1.sweep_period = (val >> 4) & 7;
                self.ch1.sweep_negate = val & 0x08 != 0;
                self.ch1.sweep_shift = val & 7;
            }
            0xFF11 => {
                self.ch1.duty = val >> 6;
                self.ch1.length = 64 - (val & 0x3F) as u16;
            }
            0xFF12 => {
                self.ch1.volume = val >> 4;
                self.ch1.env_add = val & 0x08 != 0;
                self.ch1.env_period = val & 7;
                self.ch1.dac = val & 0xF8 != 0;
                if !self.ch1.dac {
                    self.ch1.enabled = false;
                }
            }
            0xFF13 => self.ch1.freq = (self.ch1.freq & 0x700) | val as u16,
            0xFF14 => {
                self.ch1.freq = (self.ch1.freq & 0xFF) | ((val as u16 & 7) << 8);
                self.ch1.length_enable = val & 0x40 != 0;
                if val & 0x80 != 0 {
                    self.ch1.trigger(true);
                }
            }
            // Channel 2: square
            0xFF16 => {
                self.ch2.duty = val >> 6;
                self.ch2.length = 64 - (val & 0x3F) as u16;
            }
            0xFF17 => {
                self.ch2.volume = val >> 4;
                self.ch2.env_add = val & 0x08 != 0;
                self.ch2.env_period = val & 7;
                self.ch2.dac = val & 0xF8 != 0;
                if !self.ch2.dac {
                    self.ch2.enabled = false;
                }
            }
            0xFF18 => self.ch2.freq = (self.ch2.freq & 0x700) | val as u16,
            0xFF19 => {
                self.ch2.freq = (self.ch2.freq & 0xFF) | ((val as u16 & 7) << 8);
                self.ch2.length_enable = val & 0x40 != 0;
                if val & 0x80 != 0 {
                    self.ch2.trigger(false);
                }
            }
            // Channel 3: wave
            0xFF1A => {
                self.ch3.dac = val & 0x80 != 0;
                if !self.ch3.dac {
                    self.ch3.enabled = false;
                }
            }
            0xFF1B => self.ch3.length = 256 - val as u16,
            0xFF1C => self.ch3.volume_code = (val >> 5) & 3,
            0xFF1D => self.ch3.freq = (self.ch3.freq & 0x700) | val as u16,
            0xFF1E => {
                self.ch3.freq = (self.ch3.freq & 0xFF) | ((val as u16 & 7) << 8);
                self.ch3.length_enable = val & 0x40 != 0;
                if val & 0x80 != 0 {
                    self.ch3.enabled = self.ch3.dac;
                    if self.ch3.length == 0 {
                        self.ch3.length = 256;
                    }
                    self.ch3.timer = (2048 - self.ch3.freq as i32) * 2;
                    self.ch3.pos = 0;
                }
            }
            // Channel 4: noise
            0xFF20 => self.ch4.length = 64 - (val & 0x3F) as u16,
            0xFF21 => {
                self.ch4.volume = val >> 4;
                self.ch4.env_add = val & 0x08 != 0;
                self.ch4.env_period = val & 7;
                self.ch4.dac = val & 0xF8 != 0;
                if !self.ch4.dac {
                    self.ch4.enabled = false;
                }
            }
            0xFF22 => {
                self.ch4.shift = val >> 4;
                self.ch4.width7 = val & 0x08 != 0;
                self.ch4.divisor_code = val & 7;
            }
            0xFF23 => {
                self.ch4.length_enable = val & 0x40 != 0;
                if val & 0x80 != 0 {
                    self.ch4.enabled = self.ch4.dac;
                    if self.ch4.length == 0 {
                        self.ch4.length = 64;
                    }
                    self.ch4.timer = self.ch4.period();
                    self.ch4.lfsr = 0x7FFF;
                    self.ch4.env_vol = self.ch4.volume;
                    self.ch4.env_timer = self.ch4.env_period;
                }
            }
            0xFF24 => self.nr50 = val,
            0xFF25 => self.nr51 = val,
            _ => {}
        }
    }
}
