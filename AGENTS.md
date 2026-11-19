# Ricochet

Copies photos camera → laptop → iPhone. Rust, UI in Rune (pinned `dev` rev in `Cargo.toml`).

## Commands

- `nix develop -c cargo test`
- `nix develop -c cargo build`
- Screens without devices: `target/debug/ricochet --snap out.png --scene importing`
  (`--list-scenes`). A headless run never touches a camera or phone.
- After changing `Cargo.lock`, update `cargoHash` in `package.nix`: set it to `lib.fakeHash`,
  run `nix build`, and copy the hash it reports.
- Release: bump `version` in both crates' `Cargo.toml`, commit, then push a tag `vX.Y.Z`.
  `.github/workflows/release.yml` builds the downloads and publishes the GitHub release.
- `target/debug/ricochet-cli status` reads both devices and changes nothing on either. It does
  update the database (`ricochet.db` in the user's data folder), which only remembers.

## Rules

- The camera is read-only. Never call delete, upload or config on it.
- Never overwrite a photo on the laptop or the phone. Uploads go to `.part` and are renamed into
  place once their size checks out.
- The database only remembers: what exists is whatever the library folder and the phone hold.
  Deleting it must never lose or overwrite a photo.
- Device work runs off the UI thread (`crates/ricochet/src/worker.rs`) and reaches the view
  through the inbox and `poll`.
- Keep the core free of UI so triggers (udev, launchd) can call it later.
- To bump Rune, change its rev everywhere in `Cargo.toml`, including `[patch.crates-io]`.
