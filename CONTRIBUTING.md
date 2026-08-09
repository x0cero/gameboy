# Contributing

This is a solo project I wrote from scratch against the Pan Docs and hardware test ROMs, so the code reflects one person's understanding of the hardware. If you know the hardware better than I do, I would genuinely like the correction.

Pull requests are welcome for accuracy fixes (timing, PPU behavior, mapper edge cases) and for plain bug fixes. If you have a bigger feature in mind, please open an issue first so we can agree on the shape of it before you spend time writing it.

For accuracy fixes, say which test ROM or documented hardware behavior the change is based on. "Pan Docs says X" or "this makes `instr_timing` pass" is exactly the kind of justification I am looking for.

## Before you open a PR

Your change needs to pass the same checks CI runs:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

The tests need Blargg's test ROMs, which are not vendored here. Clone them into `tests/roms`:

```sh
git clone --depth 1 https://github.com/retrio/gb-test-roms tests/roms
```

## Known trade-offs

The README has a "Known inaccuracies" section. Those are deliberate, documented choices rather than oversights, so a PR that fixes one of them is welcome but should explain the performance and complexity cost it brings with it.
