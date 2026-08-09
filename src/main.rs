use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use gameboy::{CYCLES_PER_FRAME, apu, bus, cartridge, cpu, ppu};
use minifb::{Key, Scale, Window, WindowOptions};
use std::collections::VecDeque;
use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

mod voxel;

fn main() -> ExitCode {
    let Some(rom_path) = env::args().nth(1) else {
        eprintln!("usage: gameboy <rom.gb> [--headless] [--3d]");
        return ExitCode::FAILURE;
    };
    let headless = env::args().any(|a| a == "--headless");
    let mode3d = env::args().any(|a| a == "--3d");

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
    cpu.bus.ppu.capture_layers = mode3d;
    let mut voxel = mode3d.then(voxel::Renderer::new);

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
                    let (buf, w, h): (&[u32], usize, usize) = match &mut voxel {
                        Some(v) => {
                            v.render(&cpu.bus.ppu);
                            (&v.buffer, voxel::WIDTH, voxel::HEIGHT)
                        }
                        None => (&cpu.bus.ppu.framebuffer, ppu::WIDTH, ppu::HEIGHT),
                    };
                    let mut out = format!("P3\n{w} {h}\n255\n");
                    for px in buf.iter() {
                        out +=
                            &format!("{} {} {}\n", (px >> 16) & 0xFF, (px >> 8) & 0xFF, px & 0xFF);
                    }
                    std::fs::write("frame.ppm", out).unwrap();
                    // Dump captured audio as raw f32le stereo for inspection.
                    let raw: Vec<u8> = cpu
                        .bus
                        .apu
                        .samples
                        .iter()
                        .flat_map(|s| s.to_le_bytes())
                        .collect();
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
            sample_rate: apu::SAMPLE_RATE,
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

    // 3D mode renders at 3x internally, so scale the window down to match.
    let (win_w, win_h, scale) = if mode3d {
        (voxel::WIDTH, voxel::HEIGHT, Scale::X2)
    } else {
        (ppu::WIDTH, ppu::HEIGHT, Scale::X4)
    };
    let mut window = Window::new(
        &title,
        win_w,
        win_h,
        WindowOptions {
            scale,
            ..Default::default()
        },
    )
    .expect("failed to open window");
    window.set_target_fps(60);

    let state_path = format!("{rom_path}.state");
    let mut frame_count = 0u64;
    let mut paused = false;
    while window.is_open() && !window.is_key_down(Key::Escape) {
        // F5 = save state, F7 = load state, P = pause, hold Tab = fast-forward.
        if window.is_key_pressed(Key::F5, minifb::KeyRepeat::No) {
            match bincode::encode_to_vec(&cpu, bincode::config::standard()) {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&state_path, bytes) {
                        eprintln!("save state failed: {e}");
                    } else {
                        eprintln!("state saved");
                    }
                }
                Err(e) => eprintln!("save state failed: {e}"),
            }
        }
        if window.is_key_pressed(Key::F7, minifb::KeyRepeat::No) {
            match std::fs::read(&state_path)
                .map_err(|e| e.to_string())
                .and_then(|b| {
                    bincode::decode_from_slice::<cpu::Cpu, _>(&b, bincode::config::standard())
                        .map_err(|e| e.to_string())
                }) {
                Ok((loaded, _)) => {
                    cpu = loaded;
                    cpu.bus.ppu.capture_layers = mode3d;
                    eprintln!("state loaded");
                }
                Err(e) => eprintln!("load state failed: {e}"),
            }
        }
        if window.is_key_pressed(Key::P, minifb::KeyRepeat::No) {
            paused = !paused;
        }
        let turbo = window.is_key_down(Key::Tab);

        if !paused {
            let frames = if turbo { 4 } else { 1 };
            let mut cycles = 0;
            while cycles < CYCLES_PER_FRAME * frames {
                let mcycles = cpu.step();
                cycles += mcycles * 4;
                cpu.bus.tick(mcycles * 4);
            }
        }
        if turbo || paused {
            cpu.bus.apu.samples.clear(); // keep audio in sync with real time
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
        match &mut voxel {
            Some(v) => {
                v.render(&cpu.bus.ppu);
                window.update_with_buffer(&v.buffer, voxel::WIDTH, voxel::HEIGHT)
            }
            None => window.update_with_buffer(&cpu.bus.ppu.framebuffer, ppu::WIDTH, ppu::HEIGHT),
        }
        .expect("window update failed");

        // Flush battery saves about once a second.
        frame_count += 1;
        if frame_count.is_multiple_of(60) {
            cpu.bus.save_cart();
        }
    }
    cpu.bus.save_cart();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save-state round trip: run a while, snapshot, run on, restore, and
    /// confirm the restored machine replays identically.
    #[test]
    fn save_state_round_trip() {
        // The emulator state is a large by-value struct; the default test
        // stack overflows in debug builds, so run on a roomier thread.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(save_state_round_trip_body)
            .unwrap()
            .join()
            .unwrap();
    }

    fn save_state_round_trip_body() {
        let rom = "tests/roms/cpu_instrs/cpu_instrs.gb";
        if !std::path::Path::new(rom).exists() {
            eprintln!(
                "skipping: {rom} not found (clone https://github.com/retrio/gb-test-roms into tests/roms)"
            );
            return;
        }
        let cart = cartridge::Cartridge::load(rom).unwrap();
        let mut cpu = cpu::Cpu::new(bus::Bus::new(cart));
        for _ in 0..500_000 {
            let m = cpu.step();
            cpu.bus.tick(m * 4);
        }
        let snap = bincode::encode_to_vec(&cpu, bincode::config::standard()).unwrap();

        // Advance the live machine, then restore and advance the copy equally.
        for _ in 0..100_000 {
            let m = cpu.step();
            cpu.bus.tick(m * 4);
        }
        let (mut restored, _): (cpu::Cpu, usize) =
            bincode::decode_from_slice(&snap, bincode::config::standard()).unwrap();
        for _ in 0..100_000 {
            let m = restored.step();
            restored.bus.tick(m * 4);
        }
        assert_eq!(cpu.pc, restored.pc);
        assert_eq!(cpu.sp, restored.sp);
        assert_eq!(cpu.af(), restored.af());
        assert_eq!(cpu.hl(), restored.hl());
    }
}
