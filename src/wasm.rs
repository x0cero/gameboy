//! Browser frontend bindings.
//!
//! JavaScript owns the timing: it calls `step_frame` once per animation frame,
//! copies out `framebuffer` for the canvas, and drains `take_audio` into a
//! WebAudio queue. Nothing here touches the filesystem, so battery saves stay
//! in memory for the lifetime of the page.

use crate::cartridge::Cartridge;
use crate::cpu::Cpu;
use crate::ppu::{HEIGHT, WIDTH};
use crate::{CYCLES_PER_FRAME, bus::Bus};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Emulator {
    cpu: Cpu,
    rgba: Vec<u8>,
    title: String,
}

#[wasm_bindgen]
impl Emulator {
    /// Load a ROM from raw bytes. Returns an error string on a bad or
    /// unsupported cartridge header.
    #[wasm_bindgen(constructor)]
    pub fn new(rom: Vec<u8>) -> Result<Emulator, JsValue> {
        let cart = Cartridge::from_bytes(rom, String::new())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let title = cart.title();
        Ok(Emulator {
            cpu: Cpu::new(Bus::new(cart)),
            rgba: vec![0xFF; WIDTH * HEIGHT * 4],
            title,
        })
    }

    /// Cartridge title from the ROM header, for the page to display.
    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// Live machine state, for a frontend that wants to show the internals
    /// while the game runs: the register file as sixteen bit pairs, the
    /// interrupt master enable, the scanline the pixel pipeline is on, the PPU
    /// mode out of the low two bits of STAT, and the three bytes sitting at the
    /// program counter. Every read here takes `&self`, so sampling the machine
    /// cannot perturb the run.
    ///
    /// Layout: [AF, BC, DE, HL, SP, PC, IME, LY, MODE, op, op+1, op+2]
    pub fn state(&self) -> Vec<u16> {
        let c = &self.cpu;
        let ppu = &c.bus.ppu;
        vec![
            ((c.a as u16) << 8) | c.f as u16,
            ((c.b as u16) << 8) | c.c as u16,
            ((c.d as u16) << 8) | c.e as u16,
            ((c.h as u16) << 8) | c.l as u16,
            c.sp,
            c.pc,
            c.ime as u16,
            ppu.ly as u16,
            (ppu.stat & 0x03) as u16,
            c.bus.read(c.pc) as u16,
            c.bus.read(c.pc.wrapping_add(1)) as u16,
            c.bus.read(c.pc.wrapping_add(2)) as u16,
        ]
    }

    /// Run until one full frame of cycles has elapsed.
    pub fn step_frame(&mut self) {
        let mut cycles = 0u32;
        while cycles < CYCLES_PER_FRAME {
            let mcycles = self.cpu.step();
            cycles += mcycles * 4;
            self.cpu.bus.tick(mcycles * 4);
        }
        self.cpu.bus.ppu.frame_ready = false;
    }

    /// 160x144 RGBA bytes, ready for `ImageData`.
    pub fn framebuffer(&mut self) -> Vec<u8> {
        for (px, out) in self
            .cpu
            .bus
            .ppu
            .framebuffer
            .iter()
            .zip(self.rgba.chunks_exact_mut(4))
        {
            out[0] = (px >> 16) as u8;
            out[1] = (px >> 8) as u8;
            out[2] = *px as u8;
            out[3] = 0xFF;
        }
        self.rgba.clone()
    }

    /// Joypad state. Both arguments are bit sets of *pressed* buttons; the core
    /// wants active-low, so they are inverted here.
    /// buttons: bit0 A, bit1 B, bit2 Select, bit3 Start.
    /// dpad: bit0 Right, bit1 Left, bit2 Up, bit3 Down.
    pub fn set_joypad(&mut self, buttons: u8, dpad: u8) {
        self.cpu.bus.joy_buttons = !buttons & 0x0F;
        self.cpu.bus.joy_dpad = !dpad & 0x0F;
    }

    /// Drain queued audio: interleaved stereo f32 at `apu::SAMPLE_RATE`.
    pub fn take_audio(&mut self) -> Vec<f32> {
        self.cpu.bus.apu.samples.drain(..).collect()
    }

    /// Throw away queued audio, used when the page is not keeping up.
    pub fn clear_audio(&mut self) {
        self.cpu.bus.apu.samples.clear();
    }

    pub fn sample_rate(&self) -> u32 {
        crate::apu::SAMPLE_RATE
    }
}
