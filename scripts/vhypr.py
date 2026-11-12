#!/usr/bin/env python3
"""A private Hyprland desktop for testing the Linux front ends without the user's screen (MS-204).

For hand-over screenshots of GPUI use scripts/shots.py instead (MS-243): it draws the app headless
in its own process, light and dark, from a script, with no desktop, on the Mac too. Use this
desktop for what that can't show: Flutter, the compositor's blur behind the window, real input
and windowing, motion, the installed copy's launch check.

Usage: python3 scripts/vhypr.py start NAME [--size WxH] [--scale S] [--hz HZ] [--dark] [--blur] [--wallpaper]
       python3 scripts/vhypr.py run NAME -- COMMAND [ARG...]
       python3 scripts/vhypr.py shot NAME FILE [--class CLASS]
       python3 scripts/vhypr.py clients NAME
       python3 scripts/vhypr.py click NAME X Y [--button left|right|middle] [--double]
       python3 scripts/vhypr.py move NAME X Y
       python3 scripts/vhypr.py drag NAME X1 Y1 X2 Y2 [--button B] [--steps N]
       python3 scripts/vhypr.py scroll NAME X Y DY [DX]
       python3 scripts/vhypr.py type NAME TEXT
       python3 scripts/vhypr.py key NAME COMBO [COMBO...]
       python3 scripts/vhypr.py appearance NAME dark|light
       python3 scripts/vhypr.py env NAME
       python3 scripts/vhypr.py stop NAME
       python3 scripts/vhypr.py ls

`start` runs a second Hyprland nested in the user's session, with its own window on the user's
desktop turned off (output WAYLAND-1 disabled) and one headless output VHYPR-1 of WxH logical
pixels (default 1600x1000) at scale S (default 1) and HZ (default 60) instead. The user sees
nothing of it. Hyprland can't run on the headless backend alone: aquamarine takes its buffer
allocator from a DRM or Wayland backend, the user's session holds the seat, and other compositors
(weston, sway, cage) offer too old an xdg_wm_base (5; Hyprland 0.57 needs 6) to nest it in.

Everything in it is separate from the user's session:
- `run` starts COMMAND detached inside it (Wayland only: DISPLAY unset, GDK_BACKEND=wayland),
  with XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_CACHE_HOME and XDG_STATE_HOME under the desktop's
  folder, so an app's data (`~/.local/share/Mailstream`: mail.db, sign-ins) is a fresh empty
  one; put synthetic data there (`env NAME` prints the paths). Its output goes to a log
  beside them; `run` prints its pid and log.
- A private D-Bus session bus carries a stand-in for the desktop portal's Settings, which is
  where GPUI and Flutter read light or dark (org.freedesktop.appearance color-scheme):
  `--dark` starts dark, `appearance` switches a running desktop and tells the apps at once.
  The user's own theme never changes.
- Input goes only to this desktop: `click`, `move`, `drag` and `scroll` through a virtual
  pointer (zwlr_virtual_pointer_v1, X Y in logical pixels of VHYPR-1, origin top left), `type`
  and `key` through wtype (virtual keyboard; any Unicode text). A COMBO is an xkb key name
  with optional modifiers: `Return`, `ctrl+k`, `ctrl+shift+Tab`, `alt+Left`, `super+q`.
- `shot` saves a PNG of the whole output, or of the window whose class is CLASS (grim).
  Windows tile with no gaps, borders or rounding; one window fills the output.
- Window blur (Hyprland's `decoration:blur`) is off unless `--blur` turns it on, to see a
  translucent window as on a desktop that blurs (GPUI's frosted window, MS-249) and, without it,
  its fallback. Nothing lies behind the windows but a flat color unless `--wallpaper` (implied by
  `--blur`) shows Hyprland's own wallpaper, the anime mascot, which shows clearly through blur.

`env NAME` prints the variables to reach the desktop yourself (WAYLAND_DISPLAY,
HYPRLAND_INSTANCE_SIGNATURE for hyprctl, DBUS_SESSION_BUS_ADDRESS, the XDG folders). `stop`
ends the apps, the desktop, the bus and the portal and deletes the folder; `ls` lists running
desktops. Name each desktop after your agent (`ms200-gpui-styles`); never stop one you didn't
start. Needs the user's Hyprland session (it nests in it) and nix (grim, wtype, dbus-daemon and
a Python with pywayland and dbus-next come from nixpkgs, built once and kept by nix).
State: $XDG_RUNTIME_DIR/vhypr/NAME/."""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}") / "vhypr"
OUTPUT = "VHYPR-1"
PYTHON = ('(builtins.getFlake "nixpkgs").legacyPackages.${builtins.currentSystem}'
          '.python3.withPackages (p: [ p.pywayland p.dbus-next ])')
TOOLS = {"grim": "nixpkgs#grim", "wtype": "nixpkgs#wtype", "dbus": "nixpkgs#dbus",
         "scanner": "nixpkgs#wayland-scanner.out", "wlr": "nixpkgs#wlr-protocols"}
BUTTONS = {"left": 0x110, "right": 0x111, "middle": 0x112}
MODIFIERS = {"ctrl": "ctrl", "control": "ctrl", "shift": "shift", "alt": "alt", "super": "logo",
             "logo": "logo", "meta": "logo", "win": "logo", "altgr": "altgr"}


def fail(message):
    sys.exit(f"vhypr: {message}")


def nix_build(*args):
    done = subprocess.run(["nix", "build", "--no-link", "--print-out-paths", *args],
                          capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"nix build {' '.join(args)} failed:\n{done.stderr.strip()}")
    return done.stdout.split()[-1]


def folder(name):
    return ROOT / name


def load(name):
    path = folder(name) / "info.json"
    if not path.exists():
        fail(f"no desktop {name} (python3 scripts/vhypr.py start {name})")
    return json.loads(path.read_text())


def save(name, info):
    (folder(name) / "info.json").write_text(json.dumps(info, indent=1))


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def kill_group(pid, grace=3.0):
    """Ends the process group led by pid: SIGTERM, then SIGKILL after grace seconds (a crashed
    Hyprland ignores SIGTERM)."""
    for sig, wait in ((signal.SIGTERM, grace), (signal.SIGKILL, 1.0)):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            return
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            if not alive(pid):
                return
            time.sleep(0.1)


def xdg(name):
    home = folder(name) / "home"
    return {"XDG_DATA_HOME": str(home / "share"), "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_CACHE_HOME": str(home / "cache"), "XDG_STATE_HOME": str(home / "state")}


def environment(info, name):
    """What an app inside the desktop runs with."""
    env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_SOCKET")}
    env.update(WAYLAND_DISPLAY=info["socket"], HYPRLAND_INSTANCE_SIGNATURE=info["instance"],
               DBUS_SESSION_BUS_ADDRESS=info["bus"], GDK_BACKEND="wayland", XDG_SESSION_TYPE="wayland",
               **xdg(name))
    return env


def hyprctl(info, *args):
    env = dict(os.environ, HYPRLAND_INSTANCE_SIGNATURE=info["instance"])
    return subprocess.run(["hyprctl", *args], env=env, capture_output=True, text=True)


def ours():
    """The Wayland sockets of the desktops this script runs."""
    return {json.loads(p.read_text()).get("socket") for p in ROOT.glob("*/info.json")}


def parent_socket():
    """The user's own Hyprland, to nest in: a live instance not of ours, $WAYLAND_DISPLAY's if
    it is one."""
    mine = ours()
    done = subprocess.run(["hyprctl", "instances", "-j"], capture_output=True, text=True)
    sockets = [i["wl_socket"] for i in json.loads(done.stdout or "[]") if i["wl_socket"] not in mine]
    if not sockets:
        fail("no Hyprland session of the user's to nest in")
    own = os.environ.get("WAYLAND_DISPLAY", "")
    return own if own in sockets else sockets[0]



CONFIG = """hl.monitor({{ output = "WAYLAND-1", disabled = true }})
hl.monitor({{ output = "", mode = "{size}@{hz}", position = "0x0", scale = {scale} }})
hl.config({{
    general = {{ gaps_in = 0, gaps_out = 0, border_size = 0 }},
    decoration = {{ rounding = 0, blur = {{ enabled = {blur} }}, shadow = {{ enabled = false }} }},
    animations = {{ enabled = false }},
    cursor = {{ inactive_timeout = 1 }},
    misc = {{ disable_hyprland_logo = {no_wallpaper}, force_default_wallpaper = 2, disable_splash_rendering = true,
             focus_on_activate = true, disable_watchdog_warning = true }},
    debug = {{ enable_stdout_logs = true }},
}})
"""


def start(args):
    name = args.name
    if not name.replace("-", "").replace("_", "").isalnum():
        fail("NAME takes letters, digits, - and _")
    here = folder(name)
    if here.exists():
        info = json.loads((here / "info.json").read_text()) if (here / "info.json").exists() else {}
        if info and alive(info.get("pid", 0)):
            fail(f"desktop {name} runs already (stop it first)")
        shutil.rmtree(here)
    here.mkdir(parents=True)
    for path in xdg(name).values():
        Path(path).mkdir(parents=True)
    tools = {key: nix_build(installable) for key, installable in TOOLS.items()}
    tools["python"] = nix_build("--impure", "--expr", PYTHON)
    info = {"tools": tools, "apps": [], "size": args.size, "scale": args.scale, "hz": args.hz}
    (here / "color-scheme").write_text("dark\n" if args.dark else "light\n")

    bus = here / "bus"
    dbus = subprocess.Popen([f"{tools['dbus']}/bin/dbus-daemon", "--session", "--nofork", "--nopidfile",
                             f"--address=unix:path={bus}"], stdout=subprocess.DEVNULL,
                            stderr=open(here / "dbus.log", "w"), start_new_session=True)
    info.update(bus=f"unix:path={bus}", dbus=dbus.pid)
    save(name, info)
    for _ in range(50):
        if bus.exists():
            break
        time.sleep(0.1)
    else:
        stop_all(name, info, keep=True)
        fail(f"the private D-Bus session did not come up; its log: {here / 'dbus.log'}")
    portal = subprocess.Popen([f"{tools['python']}/bin/python", __file__, "_portal", name],
                              env=dict(os.environ, DBUS_SESSION_BUS_ADDRESS=info["bus"]),
                              stdout=subprocess.DEVNULL, stderr=open(here / "portal.log", "w"),
                              start_new_session=True)
    info["portal"] = portal.pid

    config = here / "hyprland.lua"
    wallpaper = args.wallpaper or args.blur
    config.write_text(CONFIG.format(size=args.size, scale=args.scale, hz=args.hz, blur=str(args.blur).lower(),
                                    no_wallpaper=str(not wallpaper).lower()))
    # LIBSEAT_BACKEND=seatd (absent here) keeps it off the GPU's DRM device, which the user's
    # session holds: libseat's fallback would open /dev/dri/card* directly.
    env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE")}
    env.update(WAYLAND_DISPLAY=parent_socket(), LIBSEAT_BACKEND="seatd", HYPRLAND_NO_SD_NOTIFY="1",
               HYPRLAND_NO_SD_VARS="1", HYPRLAND_NO_CRASHREPORTER="1", DBUS_SESSION_BUS_ADDRESS=info["bus"])
    hypr = subprocess.Popen(["Hyprland", "--config", str(config)],
                            env=env, cwd=here, stdout=open(here / "hyprland.log", "w"),
                            stderr=subprocess.STDOUT, start_new_session=True)
    info["pid"] = hypr.pid
    save(name, info)
    found = None
    for _ in range(100):
        done = subprocess.run(["hyprctl", "instances", "-j"], capture_output=True, text=True)
        found = next((i for i in json.loads(done.stdout or "[]") if i["pid"] == hypr.pid), None)
        if found or hypr.poll() is not None:
            break
        time.sleep(0.1)
    if not found:
        stop_all(name, info, keep=True)
        fail(f"Hyprland did not come up; its log: {here / 'hyprland.log'}")
    info.update(instance=found["instance"], socket=found["wl_socket"])
    save(name, info)
    created = hyprctl(info, "output", "create", "headless", OUTPUT)
    for _ in range(50):
        monitors = json.loads(hyprctl(info, "monitors", "-j").stdout or "[]")
        if any(m["name"] == OUTPUT for m in monitors):
            break
        time.sleep(0.1)
    else:
        stop_all(name, info, keep=True)
        fail(f"the headless output did not come up ({created.stdout.strip()} {created.stderr.strip()}); "
             f"Hyprland's log: {here / 'hyprland.log'}")
    print(f"desktop {name}: {args.size}@{args.hz} at scale {args.scale}, {'dark' if args.dark else 'light'}"
          f"{', blur' if args.blur else ''}{', wallpaper' if wallpaper else ''}; "
          f"stop it with: python3 scripts/vhypr.py stop {name}")
    print_env(info, name)


def print_env(info, name):
    for key in ("WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE", "DBUS_SESSION_BUS_ADDRESS",
                "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        print(f"export {key}={environment(info, name)[key]}")


def stop_all(name, info, keep=False):
    """Ends the desktop's apps, Hyprland, portal and bus; deletes its folder unless keep (a
    failed start leaves its logs there for `stop` to clear)."""
    for pid in info.get("apps", []):
        kill_group(pid, grace=2.0)
    if info.get("instance") and alive(info.get("pid", 0)):
        hyprctl(info, "dispatch", "hl.dsp.exit()")
    for key in ("pid", "portal", "dbus"):
        if info.get(key):
            kill_group(info[key])
    if not keep:
        shutil.rmtree(folder(name), ignore_errors=True)


def run(args):
    info = load(args.name)
    if not args.command:
        fail("run needs a command after --")
    here = folder(args.name)
    log = here / f"app-{len(info['apps']) + 1}.log"
    app = subprocess.Popen(args.command, env=environment(info, args.name), stdout=open(log, "w"),
                           stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    info["apps"].append(app.pid)
    save(args.name, info)
    print(f"pid {app.pid}, log {log}")


def clients(info):
    return json.loads(hyprctl(info, "clients", "-j").stdout or "[]")


def shot(args):
    info = load(args.name)
    command = [f"{info['tools']['grim']}/bin/grim", "-o", OUTPUT]
    if args.cls:
        window = next((c for c in clients(info) if c["class"] == args.cls and c["mapped"]), None)
        if window is None:
            fail(f"no window of class {args.cls}; vhypr.py clients {args.name} lists them")
        (x, y), (w, h) = window["at"], window["size"]
        command = [f"{info['tools']['grim']}/bin/grim", "-g", f"{x},{y} {w}x{h}"]
    done = subprocess.run([*command, str(Path(args.file).resolve())], env=environment(info, args.name),
                          capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"grim failed: {done.stderr.strip()}")
    print(Path(args.file).resolve())


def list_clients(args):
    for c in clients(load(args.name)):
        print(f"{c['class']}\t{c['title']}\tpid {c['pid']}\tat {c['at'][0]},{c['at'][1]}\t"
              f"size {c['size'][0]}x{c['size'][1]}")


def pointer(args, actions):
    info = load(args.name)
    done = subprocess.run([f"{info['tools']['python']}/bin/python", __file__, "_pointer", args.name,
                           json.dumps(actions)], env=environment(info, args.name), capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"pointer failed: {done.stderr.strip()}")


def click(args):
    button = BUTTONS[args.button]
    actions = [["move", args.x, args.y]] + [["button", button]] * (2 if args.double else 1)
    pointer(args, actions)


def move(args):
    pointer(args, [["move", args.x, args.y]])


def drag(args):
    steps = max(1, args.steps)
    path = [["move", args.x1 + (args.x2 - args.x1) * i / steps, args.y1 + (args.y2 - args.y1) * i / steps]
            for i in range(1, steps + 1)]
    pointer(args, [["move", args.x1, args.y1], ["press", BUTTONS[args.button]], *path,
                   ["release", BUTTONS[args.button]]])


def scroll(args):
    pointer(args, [["move", args.x, args.y], ["scroll", args.dy, args.dx]])


def wtype(name, *argv):
    info = load(name)
    done = subprocess.run([f"{info['tools']['wtype']}/bin/wtype", *argv], env=environment(info, name),
                          capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"wtype failed: {done.stderr.strip()}")


def keys(args):
    for combo in args.combos:
        *mods, key = combo.split("+")
        unknown = [m for m in mods if m.lower() not in MODIFIERS]
        if unknown or not key:
            fail(f"bad key combo {combo!r} (modifiers: ctrl, shift, alt, super, altgr)")
        argv = []
        for m in mods:
            argv += ["-M", MODIFIERS[m.lower()]]
        argv += ["-k", key]
        for m in reversed(mods):
            argv += ["-m", MODIFIERS[m.lower()]]
        wtype(args.name, *argv)


def appearance(args):
    load(args.name)
    (folder(args.name) / "color-scheme").write_text(args.mode + "\n")
    time.sleep(0.5)  # the portal polls every 0.2 s and tells the apps


def stop(args):
    path = folder(args.name) / "info.json"
    if not path.exists():
        print(f"no desktop {args.name}")
        return
    stop_all(args.name, json.loads(path.read_text()))
    print(f"stopped {args.name}")


def list_desktops(_args):
    for here in sorted(ROOT.glob("*/info.json")):
        info = json.loads(here.read_text())
        state = "running" if alive(info.get("pid", 0)) else "dead (stop it to clean up)"
        print(f"{here.parent.name}\t{info.get('size')}\t{state}")


# Helpers run under nix's Python with pywayland and dbus-next.

def pointer_helper(name, actions):
    """Plays pointer actions through zwlr_virtual_pointer_v1 on the desktop's seat."""
    import importlib
    info = load(name)
    cache = folder(name) / "protocols"
    if not (cache / "wlr_virtual_pointer_unstable_v1").exists():
        subprocess.run([sys.executable, "-m", "pywayland.scanner", "-o", str(cache), "-i",
                        f"{info['tools']['scanner']}/share/wayland/wayland.xml",
                        f"{info['tools']['wlr']}/share/wlr-protocols/unstable/wlr-virtual-pointer-unstable-v1.xml"],
                       check=True, capture_output=True)
    sys.path.insert(0, str(cache.parent))
    from pywayland.client import Display
    wayland = importlib.import_module("protocols.wayland")
    virtual = importlib.import_module("protocols.wlr_virtual_pointer_unstable_v1")
    display = Display(info["socket"])
    display.connect()
    registry = display.get_registry()
    found = {}
    registry.dispatcher["global"] = lambda _r, gid, iface, version: found.setdefault(iface, (gid, version))
    display.roundtrip()
    seat = registry.bind(found["wl_seat"][0], wayland.WlSeat, 1)
    manager = registry.bind(found["zwlr_virtual_pointer_manager_v1"][0],
                            virtual.ZwlrVirtualPointerManagerV1, 1)
    device = manager.create_virtual_pointer(seat)
    monitors = json.loads(hyprctl(info, "monitors", "-j").stdout)
    output = next(m for m in monitors if m["name"] == OUTPUT)
    width, height = round(output["width"] / output["scale"]), round(output["height"] / output["scale"])

    def now():
        return int(time.monotonic() * 1000) & 0xFFFFFFFF

    def sync(pause):
        device.frame()
        display.flush()
        display.roundtrip()
        time.sleep(pause)

    for action in actions:
        kind = action[0]
        if kind == "move":
            x, y = min(max(action[1], 0), width - 1), min(max(action[2], 0), height - 1)
            device.motion_absolute(now(), round(x), round(y), width, height)
            sync(0.02)
        elif kind in ("button", "press"):
            device.button(now(), action[1], 1)
            sync(0.05)
            if kind == "button":
                device.button(now(), action[1], 0)
                sync(0.05)
        elif kind == "release":
            device.button(now(), action[1], 0)
            sync(0.05)
        elif kind == "scroll":
            device.axis_source(0)  # wheel
            for axis, amount in ((0, action[1]), (1, action[2])):
                if amount:
                    device.axis_discrete(now(), axis, float(amount) * 15, int(amount))
            sync(0.05)
    device.destroy()
    display.flush()
    display.disconnect()


def portal_helper(name):
    """org.freedesktop.portal.Settings with only org.freedesktop.appearance color-scheme
    (1 dark, 2 light), from the desktop's color-scheme file; signals each change."""
    import asyncio
    from dbus_next import Variant
    from dbus_next.aio import MessageBus
    from dbus_next.errors import DBusError
    from dbus_next.service import PropertyAccess, ServiceInterface, dbus_property, method, signal as dbus_signal

    path = folder(name) / "color-scheme"

    def scheme():
        try:
            return 1 if path.read_text().strip() == "dark" else 2
        except OSError:
            return 2

    class Settings(ServiceInterface):
        def __init__(self):
            super().__init__("org.freedesktop.portal.Settings")

        @method()
        def ReadAll(self, namespaces: "as") -> "a{sa{sv}}":
            if namespaces and not any(n in ("org.freedesktop.appearance", "org.freedesktop.*", "*", "")
                                      for n in namespaces):
                return {}
            return {"org.freedesktop.appearance": {"color-scheme": Variant("u", scheme())}}

        @method()
        def Read(self, namespace: "s", key: "s") -> "v":
            self.check(namespace, key)
            return Variant("v", Variant("u", scheme()))

        @method()
        def ReadOne(self, namespace: "s", key: "s") -> "v":
            self.check(namespace, key)
            return Variant("u", scheme())

        @staticmethod
        def check(namespace, key):
            if (namespace, key) != ("org.freedesktop.appearance", "color-scheme"):
                raise DBusError("org.freedesktop.portal.Error.NotFound", "Requested setting not found")

        @dbus_signal()
        def SettingChanged(self, namespace, key, value) -> "ssv":
            return [namespace, key, value]

        @dbus_property(access=PropertyAccess.READ)
        def version(self) -> "u":
            return 2

    async def main():
        bus = await MessageBus().connect()
        settings = Settings()
        bus.export("/org/freedesktop/portal/desktop", settings)
        await bus.request_name("org.freedesktop.portal.Desktop")
        last = scheme()
        while True:
            await asyncio.sleep(0.2)
            current = scheme()
            if current != last:
                last = current
                settings.SettingChanged("org.freedesktop.appearance", "color-scheme", Variant("u", current))

    asyncio.run(main())


def main():
    if sys.argv[1:2] == ["_pointer"]:
        return pointer_helper(sys.argv[2], json.loads(sys.argv[3]))
    if sys.argv[1:2] == ["_portal"]:
        return portal_helper(sys.argv[2])
    parser = argparse.ArgumentParser(prog="vhypr.py", description=__doc__.split("\n\n")[0])
    verbs = parser.add_subparsers(dest="verb", required=True)
    p = verbs.add_parser("start")
    p.add_argument("name")
    p.add_argument("--size", default="1600x1000")
    p.add_argument("--scale", default="1")
    p.add_argument("--hz", default="60")
    p.add_argument("--dark", action="store_true")
    p.add_argument("--blur", action="store_true")
    p.add_argument("--wallpaper", action="store_true")
    p.set_defaults(fn=start)
    p = verbs.add_parser("run")
    p.add_argument("name")
    p.add_argument("command", nargs=argparse.REMAINDER)
    p.set_defaults(fn=run)
    p = verbs.add_parser("shot")
    p.add_argument("name")
    p.add_argument("file")
    p.add_argument("--class", dest="cls")
    p.set_defaults(fn=shot)
    p = verbs.add_parser("clients")
    p.add_argument("name")
    p.set_defaults(fn=list_clients)
    p = verbs.add_parser("click")
    p.add_argument("name")
    p.add_argument("x", type=float)
    p.add_argument("y", type=float)
    p.add_argument("--button", choices=BUTTONS, default="left")
    p.add_argument("--double", action="store_true")
    p.set_defaults(fn=click)
    p = verbs.add_parser("move")
    p.add_argument("name")
    p.add_argument("x", type=float)
    p.add_argument("y", type=float)
    p.set_defaults(fn=move)
    p = verbs.add_parser("drag")
    p.add_argument("name")
    for coordinate in ("x1", "y1", "x2", "y2"):
        p.add_argument(coordinate, type=float)
    p.add_argument("--button", choices=BUTTONS, default="left")
    p.add_argument("--steps", type=int, default=12)
    p.set_defaults(fn=drag)
    p = verbs.add_parser("scroll")
    p.add_argument("name")
    p.add_argument("x", type=float)
    p.add_argument("y", type=float)
    p.add_argument("dy", type=int)
    p.add_argument("dx", type=int, nargs="?", default=0)
    p.set_defaults(fn=scroll)
    p = verbs.add_parser("type")
    p.add_argument("name")
    p.add_argument("text")
    p.set_defaults(fn=lambda a: wtype(a.name, a.text))
    p = verbs.add_parser("key")
    p.add_argument("name")
    p.add_argument("combos", nargs="+")
    p.set_defaults(fn=keys)
    p = verbs.add_parser("appearance")
    p.add_argument("name")
    p.add_argument("mode", choices=("dark", "light"))
    p.set_defaults(fn=appearance)
    p = verbs.add_parser("env")
    p.add_argument("name")
    p.set_defaults(fn=lambda a: print_env(load(a.name), a.name))
    p = verbs.add_parser("stop")
    p.add_argument("name")
    p.set_defaults(fn=stop)
    p = verbs.add_parser("ls")
    p.set_defaults(fn=list_desktops)
    args = parser.parse_args()
    if getattr(args, "command", None) and args.command[:1] == ["--"]:
        args.command = args.command[1:]
    args.fn(args)


if __name__ == "__main__":
    main()
