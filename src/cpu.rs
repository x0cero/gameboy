use crate::bus::Bus;

/// Flag bits in register F. Lower nibble of F is always zero.
const FLAG_Z: u8 = 0x80; // zero
const FLAG_N: u8 = 0x40; // subtract
const FLAG_H: u8 = 0x20; // half-carry
const FLAG_C: u8 = 0x10; // carry

/// The SM83 CPU. Registers pair up as AF, BC, DE, HL.
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
    pub ime: bool,
    ime_pending: bool, // EI takes effect after the following instruction
    halted: bool,
    pub bus: Bus,
}

impl Cpu {
    /// Register state after the boot ROM hands off to the cartridge at 0x0100.
    /// A distinguishes hardware: 0x01 = DMG, 0x11 = CGB (games check this).
    pub fn new(bus: Bus) -> Self {
        Self {
            a: if bus.cgb { 0x11 } else { 0x01 },
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc: 0x0100,
            ime: false,
            ime_pending: false,
            halted: false,
            bus,
        }
    }

    pub fn hl(&self) -> u16 { u16::from_be_bytes([self.h, self.l]) }
    pub fn set_hl(&mut self, v: u16) { [self.h, self.l] = v.to_be_bytes(); }
    pub fn bc(&self) -> u16 { u16::from_be_bytes([self.b, self.c]) }
    pub fn set_bc(&mut self, v: u16) { [self.b, self.c] = v.to_be_bytes(); }
    pub fn de(&self) -> u16 { u16::from_be_bytes([self.d, self.e]) }
    pub fn set_de(&mut self, v: u16) { [self.d, self.e] = v.to_be_bytes(); }
    pub fn af(&self) -> u16 { u16::from_be_bytes([self.a, self.f]) }
    pub fn set_af(&mut self, v: u16) {
        let [a, f] = v.to_be_bytes();
        self.a = a;
        self.f = f & 0xF0;
    }

    fn set_flag(&mut self, flag: u8, on: bool) {
        if on { self.f |= flag } else { self.f &= !flag }
    }

    fn flag(&self, flag: u8) -> bool {
        self.f & flag != 0
    }

    fn fetch8(&mut self) -> u8 {
        let v = self.bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    fn fetch16(&mut self) -> u16 {
        let lo = self.fetch8();
        let hi = self.fetch8();
        u16::from_le_bytes([lo, hi])
    }

    fn push16(&mut self, v: u16) {
        let [hi, lo] = v.to_be_bytes();
        self.sp = self.sp.wrapping_sub(1);
        self.bus.write(self.sp, hi);
        self.sp = self.sp.wrapping_sub(1);
        self.bus.write(self.sp, lo);
    }

    fn pop16(&mut self) -> u16 {
        let lo = self.bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        let hi = self.bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_be_bytes([hi, lo])
    }

    /// Read the 8-bit operand encoded by index 0-7: B,C,D,E,H,L,(HL),A.
    fn get_r(&self, idx: u8) -> u8 {
        match idx {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => self.bus.read(self.hl()),
            _ => self.a,
        }
    }

    fn set_r(&mut self, idx: u8, v: u8) {
        match idx {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => self.bus.write(self.hl(), v),
            _ => self.a = v,
        }
    }

    // ALU helpers. Each sets flags per the SM83 spec.

    fn alu_add(&mut self, n: u8, with_carry: bool) {
        let c = (with_carry && self.flag(FLAG_C)) as u8;
        let a = self.a;
        let res = a.wrapping_add(n).wrapping_add(c);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (a & 0x0F) + (n & 0x0F) + c > 0x0F);
        self.set_flag(FLAG_C, (a as u16) + (n as u16) + (c as u16) > 0xFF);
        self.a = res;
    }

    fn alu_sub(&mut self, n: u8, with_carry: bool, store: bool) {
        let c = (with_carry && self.flag(FLAG_C)) as u8;
        let a = self.a;
        let res = a.wrapping_sub(n).wrapping_sub(c);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_H, (a & 0x0F) < (n & 0x0F) + c);
        self.set_flag(FLAG_C, (a as u16) < (n as u16) + (c as u16));
        if store {
            self.a = res;
        }
    }

    fn alu_and(&mut self, n: u8) {
        self.a &= n;
        self.f = if self.a == 0 { FLAG_Z | FLAG_H } else { FLAG_H };
    }

    fn alu_or(&mut self, n: u8) {
        self.a |= n;
        self.f = if self.a == 0 { FLAG_Z } else { 0 };
    }

    fn alu_xor(&mut self, n: u8) {
        self.a ^= n;
        self.f = if self.a == 0 { FLAG_Z } else { 0 };
    }

    /// Dispatch ALU operation 0-7 (ADD,ADC,SUB,SBC,AND,XOR,OR,CP) as laid out
    /// in opcode rows 0x80-0xBF and the d8 immediates.
    fn alu_op(&mut self, op: u8, n: u8) {
        match op {
            0 => self.alu_add(n, false),
            1 => self.alu_add(n, true),
            2 => self.alu_sub(n, false, true),
            3 => self.alu_sub(n, true, true),
            4 => self.alu_and(n),
            5 => self.alu_xor(n),
            6 => self.alu_or(n),
            _ => self.alu_sub(n, false, false), // CP
        }
    }

    fn inc8(&mut self, v: u8) -> u8 {
        let res = v.wrapping_add(1);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, v & 0x0F == 0x0F);
        res
    }

    fn dec8(&mut self, v: u8) -> u8 {
        let res = v.wrapping_sub(1);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_H, v & 0x0F == 0);
        res
    }

    fn add_hl(&mut self, rr: u16) {
        let hl = self.hl();
        let res = hl.wrapping_add(rr);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (hl & 0x0FFF) + (rr & 0x0FFF) > 0x0FFF);
        self.set_flag(FLAG_C, (hl as u32) + (rr as u32) > 0xFFFF);
        self.set_hl(res);
    }

    /// ADD SP,r8 and LD HL,SP+r8 share this: signed add with flags from the
    /// unsigned low-byte addition.
    fn sp_add_signed(&mut self, off: i8) -> u16 {
        let sp = self.sp;
        let n = off as u16; // sign-extended
        self.set_flag(FLAG_Z, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (sp & 0x0F) + (n & 0x0F) > 0x0F);
        self.set_flag(FLAG_C, (sp & 0xFF) + (n & 0xFF) > 0xFF);
        sp.wrapping_add(n)
    }

    // Rotate/shift helpers used by both the CB block and the A-register
    // shortcuts (RLCA etc., which additionally force Z=0).

    fn rlc(&mut self, v: u8) -> u8 {
        let res = v.rotate_left(1);
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn rrc(&mut self, v: u8) -> u8 {
        let res = v.rotate_right(1);
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x01 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn rl(&mut self, v: u8) -> u8 {
        let res = (v << 1) | self.flag(FLAG_C) as u8;
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn rr(&mut self, v: u8) -> u8 {
        let res = (v >> 1) | ((self.flag(FLAG_C) as u8) << 7);
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x01 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn sla(&mut self, v: u8) -> u8 {
        let res = v << 1;
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn sra(&mut self, v: u8) -> u8 {
        let res = (v >> 1) | (v & 0x80);
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x01 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn swap(&mut self, v: u8) -> u8 {
        let res = v.rotate_left(4);
        self.f = if res == 0 { FLAG_Z } else { 0 };
        res
    }

    fn srl(&mut self, v: u8) -> u8 {
        let res = v >> 1;
        self.f = 0;
        self.set_flag(FLAG_C, v & 0x01 != 0);
        self.set_flag(FLAG_Z, res == 0);
        res
    }

    fn daa(&mut self) {
        let mut a = self.a;
        let mut carry = self.flag(FLAG_C);
        if !self.flag(FLAG_N) {
            if carry || a > 0x99 {
                a = a.wrapping_add(0x60);
                carry = true;
            }
            if self.flag(FLAG_H) || a & 0x0F > 0x09 {
                a = a.wrapping_add(0x06);
            }
        } else {
            if carry {
                a = a.wrapping_sub(0x60);
            }
            if self.flag(FLAG_H) {
                a = a.wrapping_sub(0x06);
            }
        }
        self.a = a;
        self.set_flag(FLAG_Z, a == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, carry);
    }

    /// Condition codes NZ,Z,NC,C as encoded in conditional jumps/calls/rets.
    fn cond(&self, idx: u8) -> bool {
        match idx {
            0 => !self.flag(FLAG_Z),
            1 => self.flag(FLAG_Z),
            2 => !self.flag(FLAG_C),
            _ => self.flag(FLAG_C),
        }
    }

    /// Service the highest-priority pending interrupt, if any.
    fn handle_interrupts(&mut self) -> u32 {
        let pending = self.bus.ie & self.bus.if_reg & 0x1F;
        if pending == 0 {
            return 0;
        }
        self.halted = false;
        if !self.ime {
            return 0;
        }
        self.ime = false;
        let bit = pending.trailing_zeros() as u8;
        self.bus.if_reg &= !(1 << bit);
        self.push16(self.pc);
        self.pc = 0x0040 + 8 * bit as u16;
        5
    }

    /// Execute one instruction (or service an interrupt), returning machine cycles.
    pub fn step(&mut self) -> u32 {
        let ic = self.handle_interrupts();
        if ic > 0 {
            return ic;
        }
        if self.halted {
            return 1;
        }
        if self.ime_pending {
            self.ime_pending = false;
            self.ime = true;
        }

        let opcode = self.fetch8();
        match opcode {
            0x00 => 1, // NOP
            // STOP: on CGB with a speed switch armed (KEY1 bit 0), toggles
            // between normal and double speed. Otherwise a 2-byte NOP.
            0x10 => {
                self.fetch8();
                if self.bus.key1 & 0x01 != 0 {
                    self.bus.key1 = !self.bus.key1 & 0x80;
                }
                1
            }

            // LD r, r' block. 0x76 in the middle is HALT.
            0x76 => { self.halted = true; 1 }
            0x40..=0x7F => {
                let src = opcode & 7;
                let dst = (opcode >> 3) & 7;
                let v = self.get_r(src);
                self.set_r(dst, v);
                if src == 6 || dst == 6 { 2 } else { 1 }
            }

            // ALU A, r block
            0x80..=0xBF => {
                let v = self.get_r(opcode & 7);
                self.alu_op((opcode >> 3) & 7, v);
                if opcode & 7 == 6 { 2 } else { 1 }
            }

            // ALU A, d8
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let n = self.fetch8();
                self.alu_op((opcode >> 3) & 7, n);
                2
            }

            // LD r, d8
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let n = self.fetch8();
                let dst = (opcode >> 3) & 7;
                self.set_r(dst, n);
                if dst == 6 { 3 } else { 2 }
            }

            // INC r / DEC r
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let idx = (opcode >> 3) & 7;
                let v = self.get_r(idx);
                let res = self.inc8(v);
                self.set_r(idx, res);
                if idx == 6 { 3 } else { 1 }
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let idx = (opcode >> 3) & 7;
                let v = self.get_r(idx);
                let res = self.dec8(v);
                self.set_r(idx, res);
                if idx == 6 { 3 } else { 1 }
            }

            // 16-bit loads
            0x01 => { let v = self.fetch16(); self.set_bc(v); 3 }
            0x11 => { let v = self.fetch16(); self.set_de(v); 3 }
            0x21 => { let v = self.fetch16(); self.set_hl(v); 3 }
            0x31 => { self.sp = self.fetch16(); 3 }
            0x08 => {
                // LD (a16), SP
                let addr = self.fetch16();
                self.bus.write(addr, self.sp as u8);
                self.bus.write(addr.wrapping_add(1), (self.sp >> 8) as u8);
                5
            }
            0xF9 => { self.sp = self.hl(); 2 } // LD SP, HL
            0xF8 => {
                // LD HL, SP+r8
                let off = self.fetch8() as i8;
                let v = self.sp_add_signed(off);
                self.set_hl(v);
                3
            }
            0xE8 => {
                // ADD SP, r8
                let off = self.fetch8() as i8;
                self.sp = self.sp_add_signed(off);
                4
            }

            // 16-bit INC/DEC (no flags)
            0x03 => { let v = self.bc().wrapping_add(1); self.set_bc(v); 2 }
            0x13 => { let v = self.de().wrapping_add(1); self.set_de(v); 2 }
            0x23 => { let v = self.hl().wrapping_add(1); self.set_hl(v); 2 }
            0x33 => { self.sp = self.sp.wrapping_add(1); 2 }
            0x0B => { let v = self.bc().wrapping_sub(1); self.set_bc(v); 2 }
            0x1B => { let v = self.de().wrapping_sub(1); self.set_de(v); 2 }
            0x2B => { let v = self.hl().wrapping_sub(1); self.set_hl(v); 2 }
            0x3B => { self.sp = self.sp.wrapping_sub(1); 2 }

            // ADD HL, rr
            0x09 => { let v = self.bc(); self.add_hl(v); 2 }
            0x19 => { let v = self.de(); self.add_hl(v); 2 }
            0x29 => { let v = self.hl(); self.add_hl(v); 2 }
            0x39 => { let v = self.sp; self.add_hl(v); 2 }

            // Loads through register-pair addresses
            0x02 => { self.bus.write(self.bc(), self.a); 2 }
            0x12 => { self.bus.write(self.de(), self.a); 2 }
            0x0A => { self.a = self.bus.read(self.bc()); 2 }
            0x1A => { self.a = self.bus.read(self.de()); 2 }
            0x22 => { let hl = self.hl(); self.bus.write(hl, self.a); self.set_hl(hl.wrapping_add(1)); 2 }
            0x32 => { let hl = self.hl(); self.bus.write(hl, self.a); self.set_hl(hl.wrapping_sub(1)); 2 }
            0x2A => { let hl = self.hl(); self.a = self.bus.read(hl); self.set_hl(hl.wrapping_add(1)); 2 }
            0x3A => { let hl = self.hl(); self.a = self.bus.read(hl); self.set_hl(hl.wrapping_sub(1)); 2 }

            // Absolute and high-page loads
            0xEA => { let addr = self.fetch16(); self.bus.write(addr, self.a); 4 }
            0xFA => { let addr = self.fetch16(); self.a = self.bus.read(addr); 4 }
            0xE0 => { let off = self.fetch8(); self.bus.write(0xFF00 + off as u16, self.a); 3 }
            0xF0 => { let off = self.fetch8(); self.a = self.bus.read(0xFF00 + off as u16); 3 }
            0xE2 => { self.bus.write(0xFF00 + self.c as u16, self.a); 2 }
            0xF2 => { self.a = self.bus.read(0xFF00 + self.c as u16); 2 }

            // Rotates on A (always clear Z, unlike the CB versions)
            0x07 => { let v = self.a; self.a = self.rlc(v); self.set_flag(FLAG_Z, false); 1 }
            0x0F => { let v = self.a; self.a = self.rrc(v); self.set_flag(FLAG_Z, false); 1 }
            0x17 => { let v = self.a; self.a = self.rl(v); self.set_flag(FLAG_Z, false); 1 }
            0x1F => { let v = self.a; self.a = self.rr(v); self.set_flag(FLAG_Z, false); 1 }

            0x27 => { self.daa(); 1 }
            0x2F => {
                // CPL
                self.a = !self.a;
                self.set_flag(FLAG_N, true);
                self.set_flag(FLAG_H, true);
                1
            }
            0x37 => {
                // SCF
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_C, true);
                1
            }
            0x3F => {
                // CCF
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                let c = self.flag(FLAG_C);
                self.set_flag(FLAG_C, !c);
                1
            }

            // Jumps
            0xC3 => { self.pc = self.fetch16(); 4 }
            0xE9 => { self.pc = self.hl(); 1 }
            0x18 => {
                let off = self.fetch8() as i8;
                self.pc = self.pc.wrapping_add_signed(off as i16);
                3
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                let off = self.fetch8() as i8;
                if self.cond((opcode >> 3) & 3) {
                    self.pc = self.pc.wrapping_add_signed(off as i16);
                    3
                } else {
                    2
                }
            }
            0xC2 | 0xCA | 0xD2 | 0xDA => {
                let addr = self.fetch16();
                if self.cond((opcode >> 3) & 3) {
                    self.pc = addr;
                    4
                } else {
                    3
                }
            }

            // Calls and returns
            0xCD => {
                let addr = self.fetch16();
                self.push16(self.pc);
                self.pc = addr;
                6
            }
            0xC4 | 0xCC | 0xD4 | 0xDC => {
                let addr = self.fetch16();
                if self.cond((opcode >> 3) & 3) {
                    self.push16(self.pc);
                    self.pc = addr;
                    6
                } else {
                    3
                }
            }
            0xC9 => { self.pc = self.pop16(); 4 }
            0xD9 => {
                // RETI: return and enable interrupts immediately
                self.pc = self.pop16();
                self.ime = true;
                4
            }
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                if self.cond((opcode >> 3) & 3) {
                    self.pc = self.pop16();
                    5
                } else {
                    2
                }
            }

            // RST: call to a fixed vector
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                self.push16(self.pc);
                self.pc = (opcode & 0x38) as u16;
                4
            }

            // Stack
            0xC5 => { let v = self.bc(); self.push16(v); 4 }
            0xD5 => { let v = self.de(); self.push16(v); 4 }
            0xE5 => { let v = self.hl(); self.push16(v); 4 }
            0xF5 => { let v = self.af(); self.push16(v); 4 }
            0xC1 => { let v = self.pop16(); self.set_bc(v); 3 }
            0xD1 => { let v = self.pop16(); self.set_de(v); 3 }
            0xE1 => { let v = self.pop16(); self.set_hl(v); 3 }
            0xF1 => { let v = self.pop16(); self.set_af(v); 3 }

            0xF3 => { self.ime = false; self.ime_pending = false; 1 }
            0xFB => { self.ime_pending = true; 1 }

            // CB prefix: rotates/shifts, BIT, RES, SET on any r
            0xCB => {
                let cb = self.fetch8();
                let idx = cb & 7;
                let v = self.get_r(idx);
                match cb >> 6 {
                    0 => {
                        let res = match (cb >> 3) & 7 {
                            0 => self.rlc(v),
                            1 => self.rrc(v),
                            2 => self.rl(v),
                            3 => self.rr(v),
                            4 => self.sla(v),
                            5 => self.sra(v),
                            6 => self.swap(v),
                            _ => self.srl(v),
                        };
                        self.set_r(idx, res);
                    }
                    1 => {
                        // BIT b, r: test only, C unchanged
                        let bit = (cb >> 3) & 7;
                        self.set_flag(FLAG_Z, v & (1 << bit) == 0);
                        self.set_flag(FLAG_N, false);
                        self.set_flag(FLAG_H, true);
                    }
                    2 => self.set_r(idx, v & !(1 << ((cb >> 3) & 7))), // RES
                    _ => self.set_r(idx, v | (1 << ((cb >> 3) & 7))),  // SET
                }
                match (idx, cb >> 6) {
                    (6, 1) => 3, // BIT (HL)
                    (6, _) => 4, // read-modify-write on (HL)
                    _ => 2,
                }
            }

            // Unused opcodes on the SM83: D3,DB,DD,E3,E4,EB,EC,ED,F4,FC,FD
            _ => panic!(
                "illegal opcode {opcode:#04X} at {:#06X}",
                self.pc.wrapping_sub(1)
            ),
        }
    }
}
