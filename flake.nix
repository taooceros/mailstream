{
  # Linux build environment for Mailstream Flutter and GPUI (`nix develop`): Flutter with its GTK
  # desktop toolchain, the native libraries the engine links (fontconfig for Blitz's system fonts,
  # GLib for the main-thread queue), libclang for the Flutter runner's bindgen, and what GPUI
  # links or loads at run time (Wayland, X11/xcb, xkbcommon, the Vulkan loader). Rust comes from
  # rustup (`rustup default stable`).
  description = "Mailstream Flutter and GPUI on Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      each = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = each (pkgs:
        let
          # GPUI opens these with dlopen (the Vulkan loader) or links them; NixOS has no global
          # library path, so the shell puts them on LD_LIBRARY_PATH for `GPUI/run.sh`.
          gpuiLibraries = with pkgs; [
            vulkan-loader wayland libxkbcommon libx11 libxcb libxcursor libxi
            libxrandr freetype fontconfig
          ];
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [ fontconfig glib gtk3 ] ++ gpuiLibraries;
            packages = with pkgs; [ flutter pkg-config cmake ninja python3 rustPlatform.bindgenHook ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath gpuiLibraries;
          };
        });
    };
}
