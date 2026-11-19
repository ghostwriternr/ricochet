{ lib, stdenv, rustPlatform, pkg-config, makeWrapper, copyDesktopItems, makeDesktopItem, sqlite
, libxkbcommon, wayland, vulkan-loader, libGL, libx11, libxcursor, libxi, libxrandr }:

let
  manifest = (lib.importTOML ./crates/ricochet/Cargo.toml).package;
  # Rune opens these at run time rather than linking them.
  runtimeLibs = [ vulkan-loader libxkbcommon wayland libGL libx11 libxcursor libxi libxrandr ];
in
rustPlatform.buildRustPackage {
  pname = "ricochet";
  inherit (manifest) version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./crates ];
  };

  # Not cargoLock: Rune's tiny-skia-path sits beside crates.io's under the same name and version,
  # which only this per-source vendoring keeps apart. Update the hash whenever Cargo.lock changes.
  cargoHash = "sha256-KAZuE+paImXUyJdSElQWIdjw5YYv6BDS+zuttRMxLEE=";

  nativeBuildInputs = [ pkg-config ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [ makeWrapper copyDesktopItems ];
  # nixpkgs' cargo hook makes rusqlite link this rather than compile its bundled SQLite.
  buildInputs = [ sqlite ] ++ lib.optionals stdenv.hostPlatform.isLinux [ libxkbcommon wayland ];

  desktopItems = lib.optionals stdenv.hostPlatform.isLinux [
    (makeDesktopItem {
      name = "ricochet";
      desktopName = "Ricochet";
      comment = "Copy photos from a camera to this laptop, then to an iPhone";
      exec = "ricochet";
      categories = [ "Graphics" "Photography" ];
    })
  ];

  postFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    wrapProgram $out/bin/ricochet --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  meta = {
    description = "Copy photos from a camera to a laptop, then to an iPhone";
    homepage = "https://github.com/ghostwriternr/ricochet";
    license = lib.licenses.mit;
    mainProgram = "ricochet";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
