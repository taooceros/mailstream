#!/usr/bin/env python3
"""Mac compile budget (MS-260): every agent's compiles on the Mac together use at most 8 GB of
memory and 6 CPU cores.

Each compile job asks for its share before it starts and gives it back when it ends:
- Cargo: every rustc, in every checkout and project, through the rustc wrapper `install` puts in
  ~/.cargo/config.toml (the wrapper there before it, rcache, still runs after it; this repo's
  .cargo/config.toml turns rcache off with RCACHE_DISABLE). A crate's rustc, with the cc and ld
  it runs to link, is one job.
- `swift build`/`swift test`, xcodebuild and `flutter build` through `run`, which
  scripts/capped runs on the Mac; the whole command is one job. Anything else `run` is given
  (cargo, scripts that build) runs at once, its rustc in the budget through the wrapper.
Memory: a job's share is the peak its key (crate, kind, opt level; the compiler and config for
`run`) reached in its last 3 successful runs plus 10 %, learned in
~/.local/state/build-budget/peaks.json (a key not seen yet takes the same crate's largest peak,
else 1 GB for rustc, 3 GB for `run`). A job starts once the running jobs, each counted at the
larger of its share and its actual RSS, plus the actual RSS of every compile process outside a
job (cargo, build scripts, clang, ld, …), plus its own share fit in 90 % of the budget (the rest
is headroom for jobs growing before the next ones see it). A job too big for that starts alone.
CPU: 6 slots. A rustc starts with one; the threads it runs to generate code take more one at a
time through a jobserver of its own (CARGO_MAKEFLAGS fifo) that the wrapper fills from free slots
and empties as soon as tokens sit unused (without a jobserver rustc runs up to 32 threads). A
`run` job holds 4 slots and passes that on (`swift build -j 4`, `xcodebuild -jobs 4`, and for
flutter's own xcodebuild the IDEBuildOperationMaxNumberOfConcurrentCompileTasks default). While
a job runs, its wrapper measures the cores its process tree (and itself) uses and holds a slot
for each whole core beyond its tokens (rustc's helper threads, ld's threads while linking), so
the slots held follow what is actually used; when none is free it counts as waiting, and no
codegen thread gets another token until it has them. A job also starts only when the slots in
use, or the cores jobs use when that is more, plus the cores compile processes outside jobs use
(cargo, build scripts), leave room for its slots.
Waiting jobs start in arrival order: one waits at the door, the rest queue behind it.

Nothing deadlocks: the door, each running job's record and each CPU slot are flock(2) locks,
which the kernel drops when their process ends however it ends; records whose lock is gone are
ignored and deleted. Inside a job (BUILD_BUDGET_JOB set) `run` and the wrapper start everything
at once, so nested calls (install.sh → bundle.sh → capped swift build) never wait on themselves;
cargo under a swift or flutter job would run outside the CPU slots, so scripts don't nest it.

Usage:
  build-budget.py install          (Mac) put the wrapper in ~/.cargo/config.toml; warm-build.py
                                   runs it, and `run` when it is missing
  build-budget.py uninstall        put the previous wrapper back
  build-budget.py run -- CMD...    run CMD in the budget (scripts/capped CMD on the Mac)
  build-budget.py status           running jobs, waiting jobs, slots, outside compile processes
  build-budget.py sample [SECONDS] every 0.5 s: time, compile RSS sum (MB), compile CPU (cores),
                                   jobs, waiting, CPU slots held
BUILD_BUDGET_GB and BUILD_BUDGET_CPUS override 8 and 6. Off the Mac, `run` and the wrapper only
run their command: the rule is the Mac's, and only the Mac installs the wrapper."""
import ctypes
import fcntl
import json
import os
import re
import resource
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

GB = 1000 ** 3
MB = 1000 ** 2
BUDGET = float(os.environ.get("BUILD_BUDGET_GB", "8")) * GB
LIMIT = 0.9 * BUDGET
CPUS = int(os.environ.get("BUILD_BUDGET_CPUS", "6"))
DEFAULT_RUSTC = 1 * GB
DEFAULT_RUN = 3 * GB
STATE = Path.home() / ".local/state/build-budget"
HOLDERS = STATE / "holders"
WAITING = STATE / "waiting"
SLOTS = STATE / "cpu"
FIFOS = STATE / "jobserver"
PEAKS = STATE / "peaks.json"
METER = STATE / "meter.json"
INSTALLED = Path.home() / ".local/libexec/build-budget"
WRAPPER_NAME = "rustc-wrapper"
NEXT = INSTALLED / "next-wrapper"
CARGO_CONFIG = Path.home() / ".cargo/config.toml"
# Set for a job's command: rustc and `run` under it share that job.
HELD = "BUILD_BUDGET_JOB"
ON_MAC = sys.platform == "darwin"

# Compile processes, counted wherever they run; everything under a driver counts too.
COMPILERS = {
    "rustc", "cargo", "build-script-build", "cc", "c++", "clang", "clang++", "ld", "ld64",
    "ld-classic", "lld", "ld64.lld", "swift", "swiftc", "swift-frontend", "swift-driver",
    "swift-build", "xcodebuild", "gen_snapshot", "gen_snapshot_arm64",
}
DRIVERS = {"cargo", "swift-build", "xcodebuild"}


# --- processes -------------------------------------------------------------------------------

def processes():
    """{pid: (ppid, rss bytes, name)} for every process, from one `ps`."""
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,rss=,comm="], capture_output=True, text=True).stdout
    table = {}
    for line in out.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4 and parts[2].isdigit():
            table[int(parts[0])] = (int(parts[1]), int(parts[2]) * 1024, parts[3].rsplit("/", 1)[-1])
    return table


def children_of(table):
    children = {}
    for pid, (ppid, _, _) in table.items():
        children.setdefault(ppid, []).append(pid)
    return children


def tree(pid, children):
    found, stack = [], [pid]
    while stack:
        p = stack.pop()
        found.append(p)
        stack.extend(children.get(p, ()))
    return found


def compile_pids(table, children, roots=()):
    """Every compile process: COMPILERS by name, and everything under a driver or a root."""
    found = set()
    for pid, (_, _, name) in table.items():
        if name in COMPILERS:
            found.add(pid)
            if name in DRIVERS:
                found.update(tree(pid, children))
    for pid in roots:
        if pid in table:
            found.update(tree(pid, children))
    return found


class _Rusage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in (
        "user", "system", "idle_wakeups", "interrupt_wakeups", "pageins", "wired", "resident",
        "footprint", "start", "exit")]


class _Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


if ON_MAC:
    _libc = ctypes.CDLL("/usr/lib/libSystem.dylib")
    _timebase = _Timebase()
    _libc.mach_timebase_info(ctypes.byref(_timebase))
    _TICK = _timebase.numer / _timebase.denom / 1e9  # proc_pid_rusage counts mach ticks


def cpu_seconds(pid):
    """CPU time pid has used so far (`ps` has only whole seconds), or None once it is gone."""
    usage = _Rusage()
    if _libc.proc_pid_rusage(pid, 0, ctypes.byref(usage)) != 0:
        return None
    return (usage.user + usage.system) * _TICK


def cpu_rates(pids, previous):
    """({pid: cores used since previous}, snapshot): previous is an earlier snapshot
    {"t": time, "cpu": {pid: seconds}}; pids it lacks count 0 (they started since)."""
    now = time.time()
    seconds = {p: s for p in pids if (s := cpu_seconds(p)) is not None}
    rates = {}
    if previous and 0.05 < now - previous["t"] < 5:
        span = now - previous["t"]
        before = previous["cpu"]
        rates = {p: max(0.0, (s - before[str(p)]) / span) for p, s in seconds.items() if str(p) in before}
    return rates, {"t": now, "cpu": {str(p): s for p, s in seconds.items()}}


# --- locks and records -----------------------------------------------------------------------

def lock_nb(path):
    """Opens path and takes its exclusive lock without waiting; None when another process has it."""
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return fd
    except BlockingIOError:
        os.close(fd)
        return None


def live(folder, clean):
    """The records in folder whose owner still holds their lock. With clean (only the door's
    holder, which alone creates holders), records whose owner is gone are deleted."""
    records = []
    for path in folder.glob("*"):
        fd = lock_nb(path)
        if fd is not None:
            if clean:
                path.unlink(missing_ok=True)
            os.close(fd)
            continue
        try:
            records.append(json.loads(path.read_text()))
        except (OSError, ValueError):
            pass  # being written or just removed
    return records


def someone_waiting():
    for path in WAITING.glob("*"):
        fd = lock_nb(path)
        if fd is None:
            return True
        os.close(fd)
    return False


def publish(folder, record):
    """A record locked by this process for its lifetime (the kernel drops the lock with it)."""
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / str(os.getpid())
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    fcntl.flock(fd, fcntl.LOCK_EX)
    os.write(fd, json.dumps(record).encode())
    return path, fd


def withdraw(record):
    record[0].unlink(missing_ok=True)
    os.close(record[1])


def slot_paths():
    SLOTS.mkdir(parents=True, exist_ok=True)
    return [SLOTS / str(i) for i in range(CPUS)]


def take_slots(count):
    """count CPU slots (lock fds), or None when fewer are free."""
    taken = []
    for path in slot_paths():
        if len(taken) == count:
            break
        if (fd := lock_nb(path)) is not None:
            taken.append(fd)
    if len(taken) < count:
        for fd in taken:
            os.close(fd)
        return None
    return taken


def slots_held():
    held = 0
    for path in slot_paths():
        fd = lock_nb(path)
        if fd is None:
            held += 1
        else:
            os.close(fd)
    return held


# --- admission -------------------------------------------------------------------------------

def measure(clean=False, previous=None):
    """The budget's view of the machine: running jobs (each with its tree's RSS and cores),
    memory and cores counted against the budget, and the CPU snapshot for the next call."""
    jobs = live(HOLDERS, clean)
    if clean:
        live(WAITING, clean)
    table = processes()
    children = children_of(table)
    counted = compile_pids(table, children, [j["pid"] for j in jobs])
    rates, snapshot = cpu_rates(counted, previous)
    in_jobs = set()
    memory = job_cores = 0.0
    for job in jobs:
        pids = [p for p in tree(job["pid"], children) if p in table]
        in_jobs.update(pids)
        job["rss"] = sum(table[p][1] for p in pids)
        job["cores"] = sum(rates.get(p, 0.0) for p in pids)
        memory += max(job["share"], job["rss"])
        job_cores += job["cores"]
    outside = counted - in_jobs
    view = {
        "jobs": jobs,
        "outside_rss": sum(table[p][1] for p in outside),
        "outside_cores": sum(rates.get(p, 0.0) for p in outside),
        "held": slots_held(),
        "job_cores": job_cores,
    }
    view["memory"] = memory + view["outside_rss"]
    view["cores"] = max(view["held"], job_cores) + view["outside_cores"]
    return view, snapshot


def load_meter():
    try:
        return json.loads(METER.read_text())
    except (OSError, ValueError):
        return None


def save_meter(snapshot, view):
    snapshot["outside_cores"] = view["outside_cores"]
    tmp = METER.with_suffix(f".{os.getpid()}")
    tmp.write_text(json.dumps(snapshot))
    os.replace(tmp, METER)


def admit(key, share, cpus, label):
    """Waits at the door for room for share and cpus, then holds them until this process ends.
    Returns (record, slot fds)."""
    STATE.mkdir(parents=True, exist_ok=True)
    need = min(share, LIMIT)
    cpus = min(cpus, CPUS)
    waiting = publish(WAITING, {"pid": os.getpid(), "key": key, "share": share, "since": time.time()})
    door = os.open(STATE / "door.lock", os.O_RDWR | os.O_CREAT, 0o644)
    fcntl.flock(door, fcntl.LOCK_EX)
    started, told = time.monotonic(), False
    previous = load_meter()
    while True:
        if CPUS - slots_held() < cpus:  # the cheap check first: no `ps` while every slot is taken
            time.sleep(0.05)
            continue
        view, previous = measure(clean=True, previous=previous)
        save_meter(previous, view)
        jobs = view["jobs"]
        memory_fits = view["memory"] + need <= LIMIT or (not jobs and view["outside_rss"] + need <= BUDGET)
        cores_fit = view["cores"] + cpus <= CPUS or (not jobs and view["outside_cores"] < 1)
        if memory_fits and cores_fit and (slots := take_slots(cpus)) is not None:
            break
        if not told and time.monotonic() - started > 60:
            told = True
            print(f"build-budget: {label} waits for {share / GB:.1f} GB and {cpus} CPU; "
                  f"{view['memory'] / GB:.1f} of {LIMIT / GB:.1f} GB and {view['cores']:.1f} of {CPUS} "
                  f"cores in use ({len(jobs)} jobs)", file=sys.stderr, flush=True)
        time.sleep(0.1)
    holder = publish(HOLDERS, {"pid": os.getpid(), "key": key, "share": share, "cpus": cpus,
                               "since": time.time()})
    os.close(door)
    withdraw(waiting)
    return holder, slots


# --- learned peaks ---------------------------------------------------------------------------

def peaks():
    try:
        return json.loads(PEAKS.read_text())
    except (OSError, ValueError):
        return {}


def estimate(key, default):
    """The key's recent peak plus 10 %; a key not seen yet takes the same crate's (or command's)
    largest peak under another profile, else default."""
    table = peaks()
    seen = table.get(key)
    if not seen and key.startswith("rustc:"):
        prefix = key.rsplit(":", 2)[0] + ":"
        seen = [m for k, v in table.items() if k.startswith(prefix) for m in v]
    return max(seen) * MB * 1.1 if seen else default


def learn(key, peak):
    STATE.mkdir(parents=True, exist_ok=True)
    with open(STATE / "peaks.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        table = peaks()
        table[key] = (table.get(key, []) + [round(peak / MB)])[-3:]
        tmp = PEAKS.with_suffix(f".{os.getpid()}")
        tmp.write_text(json.dumps(table, sort_keys=True, indent=0))
        os.replace(tmp, PEAKS)


# --- jobserver for rustc's codegen threads ---------------------------------------------------

class Jobserver:
    """A fifo jobserver for one rustc, holding no tokens until rustc wants them: the feeder puts a
    token in only for a free CPU slot it takes, and takes the token and slot back as soon as the
    token sits unused (rustc returns tokens when a thread ends)."""

    def __init__(self):
        FIFOS.mkdir(parents=True, exist_ok=True)
        self.path = FIFOS / str(os.getpid())
        self.path.unlink(missing_ok=True)
        os.mkfifo(self.path, 0o600)
        self.fd = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)  # rustc opens its own, blocking
        self.extras = []
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.feed, daemon=True)

    def env(self, env):
        env = {k: v for k, v in env.items() if k not in ("MAKEFLAGS", "MFLAGS")}
        env["CARGO_MAKEFLAGS"] = f"-j --jobserver-auth=fifo:{self.path}"
        return env

    def unused(self):
        buf = ctypes.c_int(0)
        fcntl.ioctl(self.fd, 0x4004667F, buf)  # FIONREAD
        return buf.value

    def feed(self):
        delay, probed = 0.02, False
        while not self.done.wait(delay):
            unused = self.unused()
            if unused and self.extras:
                try:
                    returned = len(os.read(self.fd, min(unused, len(self.extras))))
                except BlockingIOError:
                    returned = 0  # rustc took them meanwhile
                for _ in range(returned):
                    os.close(self.extras.pop())
                if probed:
                    delay = min(delay * 2, 0.5)  # no thread wanted it: ask less often
                probed = False
            elif not unused and len(self.extras) < CPUS - 1:
                if probed:
                    delay = 0.02  # the last token went to a thread at once: offer another
                probed = False
                if not someone_waiting() and (slot := take_slots(1)):
                    self.extras.append(slot[0])
                    os.write(self.fd, b"+")
                    probed = True

    def close(self):
        self.done.set()
        self.thread.join()
        for fd in self.extras:
            os.close(fd)
        os.close(self.fd)
        self.path.unlink(missing_ok=True)


# --- running a job ---------------------------------------------------------------------------

def descendants(pid):
    """pid and every process under it, from the kernel's child lists (no `ps`)."""
    found, stack = [], [pid]
    buf = (ctypes.c_int * 512)()
    while stack:
        p = stack.pop()
        found.append(p)
        count = _libc.proc_listchildpids(p, buf, ctypes.sizeof(buf))
        stack.extend(buf[:count] if count > 0 else ())
    return found


class Governor:
    """Keeps a running job's CPU slots at what it actually uses. A job may use more cores than its
    slots for a moment (rustc's helper threads beside its codegen workers, ld's threads during a
    link, the wrapper itself): every TICK it measures its process tree and takes a further slot
    for each whole core over, giving them back as use drops. When no slot is free it posts a
    waiting record, so no codegen thread anywhere gets another token until the slots it needs
    come free."""

    TICK = 0.25

    def __init__(self, pid, slots, server, key):
        self.pid, self.slots, self.server, self.key = pid, slots, server, key
        self.shadows, self.debt = [], None
        self.before, self.at = {}, time.monotonic()
        self.peak = 0

    def tick(self):
        cpu, rss = {}, 0
        for p in descendants(self.pid):
            usage = _Rusage()
            if _libc.proc_pid_rusage(p, 0, ctypes.byref(usage)) == 0:
                cpu[p] = (usage.user + usage.system) * _TICK
                rss += usage.resident
        own = resource.getrusage(resource.RUSAGE_SELF)
        cpu[-1] = own.ru_utime + own.ru_stime
        self.peak = max(self.peak, rss)
        now = time.monotonic()
        cores = sum(s - self.before[p] for p, s in cpu.items() if p in self.before) / (now - self.at)
        self.before, self.at = cpu, now
        tokens = len(self.slots) + (len(self.server.extras) if self.server else 0)
        want = max(0, int(cores + 0.9) - tokens)  # whole cores over its tokens, 0.1 of grace
        while len(self.shadows) > want:
            os.close(self.shadows.pop())
        while len(self.shadows) < want and (slot := take_slots(1)):
            self.shadows.append(slot[0])
        if len(self.shadows) < want and not self.debt:
            self.debt = publish(WAITING, {"pid": os.getpid(), "key": f"over:{self.key}", "share": 0,
                                          "since": time.time()})
        elif len(self.shadows) >= want and self.debt:
            withdraw(self.debt)
            self.debt = None

    def close(self):
        for fd in self.shadows:
            os.close(fd)
        if self.debt:
            withdraw(self.debt)


def run_job(command, key, default, cpus, label, env=None, jobserver=False):
    """Runs command as one job and returns its exit status."""
    env = dict(env or os.environ)
    if not ON_MAC or env.get(HELD):
        return subprocess.run(command, env=env).returncode
    holder, slots = admit(key, estimate(key, default), cpus, label)
    env[HELD] = str(os.getpid())
    server = Jobserver() if jobserver else None
    child = subprocess.Popen(command, env=server.env(env) if server else env)
    if server:
        server.thread.start()
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, lambda s, _f: child.send_signal(s))
    governor = Governor(child.pid, slots, server, key)
    while True:
        try:
            child.wait(timeout=Governor.TICK)
            break
        except subprocess.TimeoutExpired:
            governor.tick()
    governor.close()
    if server:
        server.close()
    for fd in slots:
        os.close(fd)
    withdraw(holder)
    peak = max(governor.peak, resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)  # bytes on macOS
    if child.returncode == 0:
        learn(key, peak)
    return child.returncode


def exit_like(status):
    if status < 0:
        signal.signal(-status, signal.SIG_DFL)
        os.kill(os.getpid(), -status)
    sys.exit(status)


def arg_value(argv, flag):
    for i, arg in enumerate(argv):
        if arg == flag and i + 1 < len(argv):
            return argv[i + 1]
        if arg.startswith(flag + "="):
            return arg.split("=", 1)[1]
    return None


def rustc_wrapper(argv):
    """argv: rustc and its arguments, as cargo passes them to a rustc wrapper."""
    nxt = NEXT.read_text().strip() if NEXT.exists() else ""
    command = ([nxt] if nxt else []) + argv
    crate = arg_value(argv, "--crate-name")
    if not ON_MAC or crate in (None, "___") or os.environ.get(HELD):
        os.execvp(command[0], command)  # version and target probes, other machines, nested jobs
    opt = next((a.split("=", 1)[1] for a in argv if a.removeprefix("-C").startswith("opt-level=")), "0")
    kind = "test" if "--test" in argv else arg_value(argv, "--crate-type") or "bin"
    key = f"rustc:{crate}:{kind}:O{opt}"
    exit_like(run_job(command, key, DEFAULT_RUSTC, 1, f"rustc {crate}", jobserver=True))


JOB_CPUS = 4  # slots a `run` job holds, and the parallelism it passes on


def compile_job(command):
    """The job key when command is a compiler `run` holds slots for itself (swift build/test,
    xcodebuild, flutter build), with its parallelism set; else (None, command)."""
    name = Path(command[0]).name
    sub = command[1] if len(command) > 1 else ""
    config = "release" if "release" in " ".join(command) else "debug"
    if name == "swift" and sub in ("build", "test"):
        if "--show-bin-path" in command:
            return None, command
        if not any(a in ("-j", "--jobs") or a.startswith("--jobs=") for a in command):
            command = command[:2] + ["-j", str(JOB_CPUS)] + command[2:]
        return f"swift-{sub}-{config}", command
    if name == "xcodebuild":
        if "-jobs" not in command:
            command = command[:1] + ["-jobs", str(JOB_CPUS)] + command[1:]
        return f"xcodebuild-{config}", command
    if name == "flutter" and sub == "build":
        # flutter runs xcodebuild itself; this default caps its parallel compile tasks.
        subprocess.run(["defaults", "write", "com.apple.dt.xcodebuild",
                        "IDEBuildOperationMaxNumberOfConcurrentCompileTasks", "-int", str(JOB_CPUS)],
                       check=True)
        return f"flutter-build-{config}", command
    return None, command


def run_command(command):
    """`run -- CMD` (scripts/capped on the Mac): a compiler becomes one job; anything else (cargo,
    scripts that build) runs at once, every rustc under it in the budget through the wrapper,
    named in the environment as well so a checkout's own .cargo/config.toml can't turn it off.
    Inside a job everything runs at once, so nested calls never wait on themselves."""
    if not ON_MAC or os.environ.get(HELD):
        os.execvp(command[0], command)
    wrapper = INSTALLED / WRAPPER_NAME
    if not wrapper.exists():
        install()
    key, command = compile_job(command)
    if key is None:
        os.environ["CARGO_BUILD_RUSTC_WRAPPER"] = str(wrapper)
        os.execvp(command[0], command)
    exit_like(run_job(command, f"run:{key}", DEFAULT_RUN, JOB_CPUS, key))


# --- install ---------------------------------------------------------------------------------

WRAPPER_LINE = re.compile(r'^(\s*rustc-wrapper\s*=\s*)"([^"]*)"(.*)$', re.M)


def install():
    if not ON_MAC:
        return  # the budget is the Mac's
    INSTALLED.mkdir(parents=True, exist_ok=True)
    script = INSTALLED / "build-budget.py"
    source = Path(__file__).resolve().read_bytes()
    changed = not script.exists() or script.read_bytes() != source
    if changed:
        tmp = script.with_suffix(".new")
        tmp.write_bytes(source)
        tmp.chmod(0o755)
        os.replace(tmp, script)
    wrapper = INSTALLED / WRAPPER_NAME
    if not wrapper.is_symlink():
        wrapper.symlink_to("build-budget.py")
    text = CARGO_CONFIG.read_text() if CARGO_CONFIG.exists() else ""
    current = WRAPPER_LINE.search(text)
    if current and current.group(2) == str(wrapper):
        if changed:
            print(f"build-budget: updated {script}")
        return
    previous = current.group(2) if current else ""
    NEXT.write_text(previous + "\n" if previous else "")
    line = f'rustc-wrapper = "{wrapper}"'
    if current:
        text = WRAPPER_LINE.sub(lambda m: f'{m.group(1)}"{wrapper}"{m.group(3)}', text, count=1)
    elif re.search(r"^\[build\]\s*$", text, re.M):
        text = re.sub(r"^\[build\]\s*$", "[build]\n" + line, text, count=1, flags=re.M)
    else:
        text += ("\n" if text and not text.endswith("\n") else "") + "[build]\n" + line + "\n"
    CARGO_CONFIG.parent.mkdir(parents=True, exist_ok=True)
    CARGO_CONFIG.write_text(text)
    print(f"build-budget: {CARGO_CONFIG} now runs every rustc through {wrapper}"
          + (f", then {previous}" if previous else ""))


def uninstall():
    text = CARGO_CONFIG.read_text() if CARGO_CONFIG.exists() else ""
    current = WRAPPER_LINE.search(text)
    if current and current.group(2) == str(INSTALLED / WRAPPER_NAME):
        previous = NEXT.read_text().strip() if NEXT.exists() else ""
        if previous:
            text = WRAPPER_LINE.sub(lambda m: f'{m.group(1)}"{previous}"{m.group(3)}', text, count=1)
        else:
            text = WRAPPER_LINE.sub("", text, count=1)
        CARGO_CONFIG.write_text(text)
    shutil.rmtree(INSTALLED, ignore_errors=True)
    print(f"build-budget: removed from {CARGO_CONFIG}")


# --- reports ---------------------------------------------------------------------------------

def status():
    view, snapshot = measure()
    time.sleep(0.5)
    view, _ = measure(previous=snapshot)
    waiting = live(WAITING, clean=False)
    print(f"budget {BUDGET / GB:.1f} GB and {CPUS} CPU; memory in use {view['memory'] / GB:.2f} GB "
          f"(starts jobs up to {LIMIT / GB:.1f} GB), cores {view['cores']:.1f} ({view['held']} slots held, "
          f"{view['outside_cores']:.1f} cores and {view['outside_rss'] / GB:.2f} GB outside jobs), "
          f"{len(view['jobs'])} running, {len(waiting)} waiting")
    now = time.time()
    for job in sorted(view["jobs"], key=lambda j: j["since"]):
        print(f"  running {job['key']}: share {job['share'] / GB:.2f} GB, RSS {job['rss'] / GB:.2f} GB, "
              f"{job['cores']:.1f} cores, {now - job['since']:.0f} s")
    for job in sorted(waiting, key=lambda j: j["since"]):
        print(f"  waiting {job['key']}: share {job['share'] / GB:.2f} GB, {now - job['since']:.0f} s")
    installed = WRAPPER_LINE.search(CARGO_CONFIG.read_text()) if CARGO_CONFIG.exists() else None
    if not installed or installed.group(2) != str(INSTALLED / WRAPPER_NAME):
        print(f"  not installed in {CARGO_CONFIG} (build-budget.py install)")


def sample(seconds):
    """Every compile process's RSS and CPU, summed, with nothing left out: to measure the budget."""
    end = time.monotonic() + seconds
    print("time,compile_rss_mb,compile_cores,jobs,waiting,slots", flush=True)
    previous = None
    while time.monotonic() < end:
        jobs = live(HOLDERS, clean=False)
        waiting = live(WAITING, clean=False)
        table = processes()
        children = children_of(table)
        pids = compile_pids(table, children, [j["pid"] for j in jobs + waiting])
        rates, previous = cpu_rates(pids, previous)
        rss = sum(table[p][1] for p in pids)
        print(f"{time.time():.1f},{rss / MB:.0f},{sum(rates.values()):.2f},{len(jobs)},{len(waiting)},"
              f"{slots_held()}", flush=True)
        time.sleep(0.5)


def main():
    if Path(sys.argv[0]).name == WRAPPER_NAME:
        rustc_wrapper(sys.argv[1:])
    args = sys.argv[1:]
    verb = args[0] if args else ""
    if verb == "install":
        install()
    elif verb == "uninstall":
        uninstall()
    elif verb == "status":
        status()
    elif verb == "sample":
        sample(float(args[1]) if len(args) > 1 else float("inf"))
    elif verb == "run" and "--" in args:
        run_command(args[args.index("--") + 1:])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
