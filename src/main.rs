mod apu;
mod bus;
mod cartridge;
mod cpu;
mod ppu;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use minifb::{Key, Scale, Window, WindowOptions};
use std::collections::VecDeque;
use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// T-cycles per frame: 154 scanlines x 456 cycles.
const CYCLES_PER_FRAME: u32 = 70224;

fn main() -> ExitCode {
    let Some(rom_path) = env::args().nth(1) else {
        eprintln!("usage: gameboy <rom.gb> [--headless]");
        return ExitCode::FAILURE;
    };
    let headless = env::args().any(|a| a == "--headless");

    let cart = match cartridge::Cartridge::load(&rom_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load {rom_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let title = format!("gameboy - {}", cart.title());
    eprintln!("loaded: {} ({} KB)", cart.title(), cart.rom.len() / 1024);

    let mut cpu = cpu::Cpu::new(bus::Bus::new(cart));

    if headless {
        // Test-ROM mode: no window, serial output goes to stdout. If GB_DUMP
        // is set, render that many frames then write the screen as a PPM.
        let dump_frames: Option<u32> = env::var("GB_DUMP").ok().and_then(|v| v.parse().ok());
        let mut frames = 0u32;
        loop {
            let mcycles = cpu.step();
            cpu.bus.tick(mcycles * 4);
            if cpu.bus.ppu.frame_ready {
                cpu.bus.ppu.frame_ready = false;
                frames += 1;
                if Some(frames) == dump_frames {
                    let mut out = format!("P3\n{} {}\n255\n", ppu::WIDTH, ppu::HEIGHT);
                    for px in cpu.bus.ppu.framebuffer.iter() {
                        out += &format!("{} {} {}\n", (px >> 16) & 0xFF, (px >> 8) & 0xFF, px & 0xFF);
                    }
                    std::fs::write("frame.ppm", out).unwrap();
                    // Dump captured audio as raw f32le stereo for inspection.
                    let raw: Vec<u8> =
                        cpu.bus.apu.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    std::fs::write("samples.raw", raw).unwrap();
                    cpu.bus.save_cart();
                    return ExitCode::SUCCESS;
                }
            }
        }
    }

    // Audio: cpal pulls from a shared queue; the emulator pushes into it.
    // Underrun plays silence, overrun (queue > ~0.25s) drops the oldest.
    let audio_queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let stream = cpal::default_host().default_output_device().map(|dev| {
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: apu::SAMPLE_RATE.into(),
            buffer_size: cpal::BufferSize::Default,
        };
        let q = audio_queue.clone();
        dev.build_output_stream(
            config,
            move |out: &mut [f32], _| {
                let mut q = q.lock().unwrap();
                for s in out.iter_mut() {
                    *s = q.pop_front().unwrap_or(0.0);
                }
            },
            |e| eprintln!("audio error: {e}"),
            None,
        )
        .and_then(|s| {
            s.play()?;
            Ok(s)
        })
    });
    if let Some(Err(e)) = &stream {
        eprintln!("audio unavailable: {e}");
    }

    let mut window = Window::new(
        &title,
        ppu::WIDTH,
        ppu::HEIGHT,
        WindowOptions { scale: Scale::X4, ..Default::default() },
    )
    .expect("failed to open window");
    window.set_target_fps(60);

    let mut frame_count = 0u64;
    while window.is_open() && !window.is_key_down(Key::Escape) {
        let mut cycles = 0;
        while cycles < CYCLES_PER_FRAME {
            let mcycles = cpu.step();
            cycles += mcycles * 4;
            cpu.bus.tick(mcycles * 4);
        }

        // Joypad: active-low. Z=A, X=B, Enter=Start, RightShift=Select, arrows=dpad.
        let btn = |k| !window.is_key_down(k) as u8;
        cpu.bus.joy_buttons =
            btn(Key::Z) | btn(Key::X) << 1 | btn(Key::RightShift) << 2 | btn(Key::Enter) << 3;
        cpu.bus.joy_dpad =
            btn(Key::Right) | btn(Key::Left) << 1 | btn(Key::Up) << 2 | btn(Key::Down) << 3;

        {
            let mut q = audio_queue.lock().unwrap();
            q.extend(cpu.bus.apu.samples.drain(..));
            let cap = apu::SAMPLE_RATE as usize / 2; // 0.25s of stereo
            while q.len() > cap {
                q.pop_front();
            }
        }

        cpu.bus.ppu.frame_ready = false;
        window
            .update_with_buffer(&cpu.bus.ppu.framebuffer, ppu::WIDTH, ppu::HEIGHT)
            .expect("window update failed");

        // Flush battery saves about once a second.
        frame_count += 1;
        if frame_count % 60 == 0 {
            cpu.bus.save_cart();
        }
    }
    cpu.bus.save_cart();
    ExitCode::SUCCESS
}
