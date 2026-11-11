{
  # Linux build environment for Mailstream Flutter (`nix develop`): Flutter with its GTK desktop
  # toolchain, the native libraries the engine links (fontconfig for Blitz's system fonts, GLib
  # for the main-thread queue), and libclang for the Rust runner's bindgen. Rust comes from rustup
  # (`rustup default stable`).
  description = "Mailstream Flutter on Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      each = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = each (pkgs: {
        default = pkgs.mkShell {
          buildInputs = with pkgs; [ fontconfig glib gtk3 ];
          packages = with pkgs; [ flutter pkg-config cmake ninja python3 rustPlatform.bindgenHook ];
        };
      });
    };
}
