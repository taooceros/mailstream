# Linux build environment for Mailstream Flutter and GPUI (`devenv shell`): Flutter with its GTK
# desktop toolchain, the native libraries the engine links (fontconfig for Blitz's system fonts,
# GLib for the main-thread queue), libclang for the Flutter runner's bindgen, and what GPUI links
# or loads at run time (Wayland, X11/xcb, xkbcommon, the Vulkan loader). Rust comes from rustup
# (`rustup default stable`).
{ pkgs, lib, ... }:
let
  # GPUI opens these with dlopen (the Vulkan loader) or links them; NixOS has no global library
  # path, so the shell puts them on LD_LIBRARY_PATH for `GPUI/run.sh`.
  gpuiLibraries = with pkgs; [
    vulkan-loader wayland libxkbcommon libx11 libxcb libxcursor libxi libxrandr freetype
    fontconfig
  ];
in
{
  packages = with pkgs; [
    fontconfig glib gtk3 flutter pkg-config cmake ninja python3 rustPlatform.bindgenHook
  ] ++ gpuiLibraries;
  env.LD_LIBRARY_PATH = lib.makeLibraryPath gpuiLibraries;
  # nixpkgs' reproducible-builds hook adds `-frandom-seed=` from the shell's out path, which
  # differs per folder, to NIX_CFLAGS_COMPILE and so to bindgenHook's BINDGEN_EXTRA_CLANG_ARGS:
  # bindgen's build script reruns on that variable, so every checkout rebuilt bindgen and the
  # Turso crates above it instead of reusing the default checkout's warm build (MS-294).
  env.NIX_OUTPATH_USED_AS_RANDOM_SEED = "mailstream";
}
