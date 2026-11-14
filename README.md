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

## Install

Download the latest build from [Releases](https://github.com/ghostwriternr/ricochet/releases):

- **Linux (x86_64)**: `ricochet-*-x86_64-linux.tar.gz` holds `ricochet` and `ricochet-cli`.
  It needs `libxkbcommon` and a Vulkan or GL driver. On NixOS, build from source instead.
- **Mac (Apple silicon)**: `ricochet-*-aarch64-macos.zip` holds `Ricochet.app` and
  `ricochet-cli`. The app isn't notarized, so clear the download flag once before opening it:
  `xattr -dr com.apple.quarantine Ricochet.app`.

## Build from source

```sh
nix develop
cargo run --release -p ricochet
```

On Linux, `usbmuxd` must be running and the phone paired ("Trust this computer"). On NixOS
that's `services.usbmuxd.enable = true;`.

`ricochet-cli` runs the same steps without a window: `status`, `import`, `push`. The library
is `GR IV` in your Pictures folder; `RICOCHET_LIBRARY` overrides it for the CLI.

## Layout

- `crates/ricochet-core`: devices, the library layout, plans and transfers. No UI.
- `crates/ricochet`: the window.

## License

MIT
