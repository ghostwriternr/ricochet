# Ricochet

A desktop app that copies photos from a camera to your laptop, then from the laptop to an
iPhone, one button each. Built with [Rune](https://github.com/anomalyco/rune).

![Ricochet importing from the camera](docs/importing.png)

- **Camera → Laptop**: lists the camera over PTP on USB and downloads what the library
  doesn't have yet. It only reads from the camera.
- **Laptop → iPhone**: sends what the phone doesn't have yet into an app's Documents folder over
  USB (usbmuxd and AFC, no mounting). It defaults to LocalSend's, which shows in Files under
  On My iPhone → LocalSend.

Both sides use the same flat layout, so selecting everything on the phone is one tap:

```
~/Pictures/GR IV/RAW/R0000185.DNG      LocalSend/GR IV/RAW/R0000185.DNG
~/Pictures/GR IV/JPG/R0000185.JPG      LocalSend/GR IV/JPG/R0000185.JPG
```

## Run

```sh
nix develop
cargo run --release -p ricochet
```

On Linux, `usbmuxd` must be running and the phone paired ("Trust this computer"). On NixOS
that's `services.usbmuxd.enable = true;`.

`ricochet-cli` runs the same steps without a window: `status`, `import`, `push`. `adopt DIR`
moves a folder of camera files into the library; it prints its plan and changes nothing unless
given `--go`.

## Layout

- `crates/ricochet-core`: devices, the library layout, plans and transfers. No UI.
- `crates/ricochet`: the window.

## License

MIT
