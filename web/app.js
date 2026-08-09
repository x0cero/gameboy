import init, { Emulator } from "./pkg/gameboy.js";

const canvas = document.getElementById("screen");
const ctx = canvas.getContext("2d");
const statusEl = document.getElementById("status");
const overlay = document.getElementById("overlay");
const startBtn = document.getElementById("start");
const romInput = document.getElementById("rom");

const image = ctx.createImageData(160, 144);
let emu = null;
let romName = "";
let audioCtx = null;
let sink = null;
let backlog = 0;
let running = false;

function status(text) {
  statusEl.textContent = text;
}

// Keyboard, matching the desktop build: Z=A, X=B, Enter=Start, Shift=Select.
const BUTTONS = { KeyZ: 0, KeyX: 1, ShiftLeft: 2, ShiftRight: 2, Enter: 3 };
const DPAD = { ArrowRight: 0, ArrowLeft: 1, ArrowUp: 2, ArrowDown: 3 };
let buttons = 0;
let dpad = 0;

function key(e, down) {
  let hit = false;
  if (e.code in BUTTONS) {
    const bit = 1 << BUTTONS[e.code];
    buttons = down ? buttons | bit : buttons & ~bit;
    hit = true;
  }
  if (e.code in DPAD) {
    const bit = 1 << DPAD[e.code];
    dpad = down ? dpad | bit : dpad & ~bit;
    hit = true;
  }
  if (hit) e.preventDefault();
}

window.addEventListener("keydown", (e) => key(e, true));
window.addEventListener("keyup", (e) => key(e, false));
window.addEventListener("blur", () => {
  buttons = 0;
  dpad = 0;
});

async function startAudio(sampleRate) {
  if (audioCtx) {
    await audioCtx.resume();
    return;
  }
  audioCtx = new AudioContext({ sampleRate });
  await audioCtx.audioWorklet.addModule("./audio-worklet.js");
  sink = new AudioWorkletNode(audioCtx, "gb-sink", {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [2],
  });
  sink.port.onmessage = (e) => {
    backlog = e.data;
  };
  sink.connect(audioCtx.destination);
}

function loadRom(bytes, name) {
  try {
    emu = new Emulator(bytes);
  } catch (err) {
    status(`could not load ${name}: ${err}`);
    return;
  }
  romName = emu.title().trim() || name;
  if (sink) sink.port.postMessage("flush");
  status(`running ${romName}`);
}

function frame() {
  requestAnimationFrame(frame);
  if (!emu || !running) return;

  emu.step_frame();
  image.data.set(emu.framebuffer());
  ctx.putImageData(image, 0, 0);

  if (sink) {
    // Keep at most a quarter second of stereo audio buffered; beyond that the
    // page has drifted ahead and the extra samples would only add latency.
    const samples = emu.take_audio();
    if (backlog > audioCtx.sampleRate / 2) {
      sink.port.postMessage("flush");
    } else if (samples.length > 0) {
      sink.port.postMessage(samples, [samples.buffer]);
    }
  } else {
    emu.clear_audio();
  }

  emu.set_joypad(buttons, dpad);
}

async function main() {
  await init();
  status("loading libbet.gb");
  const res = await fetch("./libbet.gb");
  if (res.ok) {
    loadRom(new Uint8Array(await res.arrayBuffer()), "libbet.gb");
  } else {
    status("no ROM loaded");
  }
  requestAnimationFrame(frame);

  startBtn.addEventListener("click", async () => {
    if (emu) await startAudio(emu.sample_rate());
    overlay.classList.add("hidden");
    running = true;
    canvas.focus();
    status(romName ? `running ${romName}` : "no ROM loaded");
  });

  romInput.addEventListener("change", async () => {
    const file = romInput.files[0];
    if (!file) return;
    loadRom(new Uint8Array(await file.arrayBuffer()), file.name);
  });
}

main().catch((err) => status(`failed to start: ${err}`));
