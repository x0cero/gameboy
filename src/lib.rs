//! Game Boy / Game Boy Color emulator core.
//!
//! The modules here are platform independent; frontends (the desktop binary in
//! `main.rs`, the browser frontend in `wasm.rs`) drive them the same way: call
//! `Cpu::step`, feed the returned M-cycles to `Bus::tick`, then read
//! `bus.ppu.framebuffer` and drain `bus.apu.samples`.

pub mod apu;
pub mod bus;
pub mod cartridge;
pub mod cpu;
pub mod ppu;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// T-cycles per frame: 154 scanlines x 456 cycles.
pub const CYCLES_PER_FRAME: u32 = 70224;
