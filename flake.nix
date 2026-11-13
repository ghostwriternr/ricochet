{
  description = "Ricochet: copy photos from a camera to a laptop, then to an iPhone";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer pkg-config libgphoto2 rustPlatform.bindgenHook ]
            ++ lib.optionals stdenv.hostPlatform.isLinux [ libxkbcommon wayland ];
          # Rune opens these at run time rather than linking them.
          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (pkgs.lib.makeLibraryPath (with pkgs; [
            vulkan-loader libxkbcommon wayland libGL libx11 libxcursor libxi libxrandr
          ]));
        };
      });
    };
}
