# Ricochet

Copies photos camera → laptop → iPhone. Rust, UI in Rune (pinned `dev` rev in `Cargo.toml`).

## Commands

- `nix develop -c cargo test`
- `nix develop -c cargo build`
- Screens without devices: `target/debug/ricochet --snap out.png --scene importing`
  (`--list-scenes`). A headless run never touches a camera or phone.
- Release: bump `version` in both crates' `Cargo.toml`, commit, then push a tag `vX.Y.Z`.
  `.github/workflows/release.yml` builds the downloads and publishes the GitHub release.
- `target/debug/ricochet-cli status` reads both devices and changes nothing.

## Rules

- The camera is read-only. Never call delete, upload or config on it.
- Never overwrite a photo on the laptop or the phone. Uploads go to `.part` and are renamed into
  place once their size checks out.
- Device work runs off the UI thread (`crates/ricochet/src/worker.rs`) and reaches the view
  through the inbox and `poll`.
- Keep the core free of UI so triggers (udev, launchd) can call it later.
- To bump Rune, change its rev everywhere in `Cargo.toml`, including `[patch.crates-io]`.
