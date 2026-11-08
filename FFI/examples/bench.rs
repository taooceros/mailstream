//! Compares the Swift and the Rust engine through the path front ends use (`Mailstream::new`,
//! then commands through `mailstream::call`): time from start to the first answer, each
//! command's latency after warm-up (median, p99) and throughput (one at a time, and 32 in
//! flight), and memory. `CONFIG=release ./scripts/ffi.sh` first, then
//!
//! ```sh
//! cd FFI && MAILSTREAM_KIT_CONFIG=release cargo run --release --example bench -- [options]
//! ```
//!
//! `--rounds N` (3), `--engines swift,rust`, `--cases config.list,…` (default: every case),
//! `--scale F` (iterations × F; 1), `--out FILE` (every result as JSON lines).
//!
//! Each engine runs in a process of its own (the engine is chosen once per process), started
//! from this one with a fresh home folder holding the same settings.json and accounts.json, and
//! a Keychain item of the run's own (`MAILSTREAM_KEYCHAIN_SERVICE`), so the person's mail,
//! settings and sign-ins are never touched. Rounds alternate which engine goes first. Results
//! are lines `BENCH {json}`, one kind of line for every phase: a command ported later gets a row
//! in `CASES` (with data in `seed` if it needs some) and is measured the same way.
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures_executor::block_on;
use mailstream::{Change, EventListener, Mailstream, call, run_main_loop};
use serde_json::{Value, json};

/// One command as measured: its arguments for the `i`th call (writes alternate values so each
/// call changes the file), and how many calls to time.
struct Case {
    name: &'static str,
    command: &'static str,
    args: fn(usize) -> Value,
    /// Writes a file: fewer iterations.
    write: bool,
}

const SETTING_KEYS: [&str; 4] =
    ["sync.pollSeconds", "remoteContent.trustedSenders", "aiTagging.model", "chat.pollSeconds"];

const CASES: &[Case] = &[
    Case { name: "config.list", command: "config.list", args: |_| json!({}), write: false },
    Case { name: "config.get", command: "config.get", args: |i| json!({ "key": SETTING_KEYS[i % 4] }), write: false },
    Case { name: "config.schema", command: "config.schema", args: |_| json!({}), write: false },
    Case { name: "config.path", command: "config.path", args: |_| json!({}), write: false },
    Case { name: "accounts.list", command: "accounts.list", args: |_| json!({}), write: false },
    Case {
        name: "accounts.client",
        command: "accounts.client",
        args: |i| json!({ "provider": (["google", "microsoft"][i % 2]) }),
        write: false,
    },
    Case {
        name: "config.set",
        command: "config.set",
        args: |i| json!({ "key": "sync.pollSeconds", "value": (120 + i % 2).to_string() }),
        write: true,
    },
    Case {
        name: "accounts.rename",
        command: "accounts.rename",
        args: |i| json!({ "account": "bench0@gmail.com", "label": (["Bench A", "Bench B"][i % 2]) }),
        write: true,
    },
];

/// In-flight calls for the concurrent throughput.
const CONCURRENCY: usize = 32;
const KEYCHAIN_PREFIX: &str = "dev.mailstream.bench.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        child(&args[1..]);
    } else {
        parent(&args);
    }
}

// MARK: - Parent: data, processes, report

struct Options {
    rounds: usize,
    engines: Vec<String>,
    cases: Vec<String>,
    scale: f64,
    out: Option<PathBuf>,
}

fn options(args: &[String]) -> Options {
    let mut options = Options {
        rounds: 3,
        engines: vec!["swift".into(), "rust".into()],
        cases: CASES.iter().map(|case| case.name.to_string()).collect(),
        scale: 1.0,
        out: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(&format!("{arg} needs a value"))).clone();
        match arg.as_str() {
            "--rounds" => options.rounds = value().parse().unwrap_or_else(|_| fail("--rounds takes a number")),
            "--engines" => options.engines = value().split(',').map(String::from).collect(),
            "--cases" => options.cases = value().split(',').map(String::from).collect(),
            "--scale" => options.scale = value().parse().unwrap_or_else(|_| fail("--scale takes a number")),
            "--out" => options.out = Some(value().into()),
            _ => fail(&format!("unknown option {arg}; see the header of FFI/examples/bench.rs")),
        }
    }
    if let Some(unknown) = options.engines.iter().find(|engine| !["swift", "rust"].contains(&engine.as_str())) {
        fail(&format!("unknown engine {unknown}"));
    }
    if let Some(unknown) = options.cases.iter().find(|name| !CASES.iter().any(|case| case.name == *name)) {
        fail(&format!("unknown case {unknown}"));
    }
    options
}

fn fail(message: &str) -> ! {
    eprintln!("bench: {message}");
    std::process::exit(2)
}

/// The run's temporary folders and Keychain item, removed when it ends.
struct Scratch {
    root: PathBuf,
    service: String,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = security_framework::passwords::delete_generic_password(&self.service, "refresh-tokens");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn parent(args: &[String]) {
    let options = options(args);
    let engine = engine_library();
    println!("engine library: {} (built {})", engine.display(), modified(&engine));
    let root = std::env::temp_dir().join(format!("mailstream-bench-{}", std::process::id()));
    let scratch = Scratch { root, service: format!("{KEYCHAIN_PREFIX}{}", std::process::id()) };
    seed_keychain(&scratch.service);
    let executable = std::env::current_exe().expect("this program's path");
    let mut lines: Vec<Value> = Vec::new();
    for round in 1..=options.rounds {
        let mut order = options.engines.clone();
        if round % 2 == 0 {
            order.reverse();
        }
        for engine in order {
            let home = scratch.root.join(format!("{round}-{engine}"));
            seed_home(&home);
            let load = load_average();
            eprintln!("round {round}: {engine} (load {load:.2})");
            let mut command = Command::new(&executable);
            command
                .args(["--child", &engine, &round.to_string(), &options.scale.to_string(), &options.cases.join(",")])
                .env("HOME", &home)
                .env("CFFIXED_USER_HOME", &home)
                .env("MAILSTREAM_KEYCHAIN_SERVICE", &scratch.service)
                .env_remove("MAILSTREAM_ENGINE")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if engine == "rust" {
                command.env("MAILSTREAM_ENGINE", "rust");
            }
            let output = command.output().expect("the engine process starts");
            if !output.status.success() {
                fail(&format!("the {engine} run failed ({})", output.status));
            }
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some(json) = line.strip_prefix("BENCH ") {
                    let mut value: Value = serde_json::from_str(json).expect("the child writes JSON");
                    value["load"] = json!((load * 100.0).round() / 100.0);
                    lines.push(value);
                }
            }
        }
    }
    if let Some(out) = &options.out {
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(out, text).unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", out.display())));
    }
    report(&lines, &options);
}

/// The `libMailstreamKit.dylib` this process loaded: it must be the one this checkout built.
fn engine_library() -> PathBuf {
    unsafe extern "C" {
        fn mailstream_start();
    }
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::dladdr(mailstream_start as *const c_void, &mut info) };
    if found == 0 || info.dli_fname.is_null() {
        fail("cannot tell which libMailstreamKit.dylib is loaded");
    }
    let path = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned());
    let path = path.canonicalize().unwrap_or(path);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").canonicalize().expect("the repository exists");
    if !path.starts_with(repository.join(".build")) {
        fail(&format!("loaded {} instead of this checkout's .build engine", path.display()));
    }
    path
}

fn modified(path: &Path) -> String {
    let age =
        std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok().and_then(|time| time.elapsed().ok());
    age.map_or_else(|| "at an unknown time".into(), |age| format!("{} min ago", age.as_secs() / 60))
}

/// A sign-ins item with the clients last used for each provider and no refresh tokens, so no
/// engine asks a provider for a token. Created by this program, which its child processes are,
/// so reading it asks nobody.
fn seed_keychain(service: &str) {
    let contents = br#"{"clients":{},"lastClients":{"google":{"id":"bench-google.apps.googleusercontent.com","secret":"bench"},"microsoft":{"id":"00000000-0000-0000-0000-000000000000"}},"refreshTokens":{}}"#;
    let mut options = security_framework::passwords::PasswordOptions::new_generic_password(service, "refresh-tokens");
    options.set_label("Mailstream sign-ins (bench)");
    security_framework::passwords::set_generic_password_options(contents, options)
        .unwrap_or_else(|error| fail(&format!("cannot create the bench Keychain item: {error}")));
}

/// The same files for every engine and round: a few changed settings (AI tagging off, so no
/// engine runs omp) and five accounts.
fn seed_home(home: &Path) {
    let support = home.join("Library/Application Support/Mailstream");
    std::fs::create_dir_all(&support).expect("the bench home is writable");
    let senders: Vec<String> = (0..50).map(|n| format!("sender{n:02}@example.com")).collect();
    let settings = json!({
        "aiTagging.enabled": false,
        "remoteContent.trustedSenders": senders,
        "sync.pollSeconds": 120,
    });
    std::fs::write(support.join("settings.json"), format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()))
        .unwrap();
    let google = [
        "openid",
        "email",
        "profile",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/chat.messages.readonly",
    ];
    let microsoft = ["openid", "email", "profile", "offline_access", "https://graph.microsoft.com/Mail.ReadWrite"];
    let accounts: Vec<Value> = (0..5)
        .map(|n| {
            let (provider, domain, scopes) =
                if n < 3 { ("google", "gmail.com", &google[..]) } else { ("microsoft", "outlook.com", &microsoft[..]) };
            let id = format!("bench{n}@{domain}");
            json!({ "id": id, "provider": provider, "label": id, "displayName": format!("Bench {n}"), "scopes": scopes })
        })
        .collect();
    std::fs::write(support.join("accounts.json"), serde_json::to_string_pretty(&accounts).unwrap()).unwrap();
}

fn load_average() -> f64 {
    let mut loads = [0f64; 3];
    if unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) } < 1 { f64::NAN } else { loads[0] }
}

/// A table per round, then the medians over the rounds with Rust's time as a share of Swift's.
fn report(lines: &[Value], options: &Options) {
    fn lines_of<'a>(lines: &'a [Value], kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        lines.iter().filter(move |line| line["kind"] == kind)
    }
    let of = |kind: &'static str| lines_of(lines, kind);
    println!();
    println!(
        "{:<6} {:>5} {:<16} {:>10} {:>10} {:>10} {:>12} {:>12}",
        "engine", "round", "case", "median µs", "p99 µs", "mean µs", "seq calls/s", "×32 calls/s"
    );
    for line in of("case") {
        println!(
            "{:<6} {:>5} {:<16} {:>10.1} {:>10.1} {:>10.1} {:>12.0} {:>12.0}",
            str(&line["engine"]),
            line["round"],
            str(&line["case"]),
            num(&line["median_us"]),
            num(&line["p99_us"]),
            num(&line["mean_us"]),
            num(&line["sequential_per_s"]),
            num(&line["concurrent_per_s"]),
        );
    }
    println!();
    println!(
        "{:<6} {:>5} {:>7} {:>12} {:>12} {:>12} {:>12} {:>12} {:>7}",
        "engine", "round", "load", "new() µs", "first ms", "base MB", "ready MB", "end MB", "events"
    );
    for line in of("process") {
        println!(
            "{:<6} {:>5} {:>7.2} {:>12.1} {:>12.2} {:>12.1} {:>12.1} {:>12.1} {:>7}",
            str(&line["engine"]),
            line["round"],
            num(&line["load"]),
            num(&line["new_us"]),
            num(&line["first_answer_ms"]),
            num(&line["footprint_base_mb"]),
            num(&line["footprint_ready_mb"]),
            num(&line["footprint_end_mb"]),
            line["events"],
        );
    }
    if !(options.engines.contains(&"swift".to_string()) && options.engines.contains(&"rust".to_string())) {
        return;
    }
    let median_of = |kind: &str, engine: &str, case: Option<&str>, field: &str| {
        let mut values: Vec<f64> = lines_of(lines, kind)
            .filter(|line| line["engine"] == engine && case.is_none_or(|case| line["case"] == case))
            .map(|line| num(&line[field]))
            .collect();
        values.sort_by(f64::total_cmp);
        if values.is_empty() { f64::NAN } else { values[values.len() / 2] }
    };
    println!();
    println!("medians over {} rounds (rust/swift: below 1 means Rust is faster or smaller)", options.rounds);
    println!(
        "{:<16} {:>11} {:>11} {:>8} {:>11} {:>11} {:>8} {:>10} {:>10} {:>8}",
        "case", "swift med", "rust med", "ratio", "swift p99", "rust p99", "ratio", "swift ×32", "rust ×32", "ratio"
    );
    for name in &options.cases {
        let row =
            |field: &str| (median_of("case", "swift", Some(name), field), median_of("case", "rust", Some(name), field));
        let (sm, rm) = row("median_us");
        let (sp, rp) = row("p99_us");
        let (sc, rc) = row("concurrent_per_s");
        println!(
            "{name:<16} {sm:>11.1} {rm:>11.1} {:>8.2} {sp:>11.1} {rp:>11.1} {:>8.2} {sc:>10.0} {rc:>10.0} {:>8.2}",
            rm / sm,
            rp / sp,
            sc / rc
        );
    }
    for (label, field) in [
        ("first answer ms", "first_answer_ms"),
        ("ready MB", "footprint_ready_mb"),
        ("end MB", "footprint_end_mb"),
        ("peak MB", "footprint_peak_mb"),
    ] {
        let (swift, rust) = (median_of("process", "swift", None, field), median_of("process", "rust", None, field));
        println!("{label:<16} {swift:>11.2} {rust:>11.2} {:>8.2}", rust / swift);
    }
}

fn num(value: &Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

fn str(value: &Value) -> &str {
    value.as_str().unwrap_or("?")
}

// MARK: - Child: one engine, measured

struct Count(Arc<AtomicU64>);

impl EventListener for Count {
    fn on_event(&self, _change: Change) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn child(args: &[String]) {
    let [engine, round, scale, cases] = args else { fail("--child takes engine, round, scale and cases") };
    let (engine, round, scale) = (engine.clone(), round.parse::<u64>().unwrap(), scale.parse::<f64>().unwrap());
    let cases: Vec<&'static Case> =
        cases.split(',').filter_map(|name| CASES.iter().find(|case| case.name == name)).collect();
    // Never run against the person's data: the parent sets all three.
    let home = std::env::var("HOME").unwrap_or_default();
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let service = std::env::var("MAILSTREAM_KEYCHAIN_SERVICE").unwrap_or_default();
    if !home.contains("mailstream-bench-")
        || std::env::var("CFFIXED_USER_HOME").ok().as_deref() != Some(home.as_str())
        || !service.starts_with(KEYCHAIN_PREFIX)
        || (std::env::var("MAILSTREAM_ENGINE").as_deref() == Ok("rust")) != (engine == "rust")
    {
        fail(&format!("refusing to run outside a bench home ({home}, temp {temp}, service {service})"));
    }
    engine_library();
    std::thread::spawn(move || {
        let base = footprint();
        let start = Instant::now();
        let mail = Mailstream::new();
        let new_us = start.elapsed().as_secs_f64() * 1e6;
        let events = Arc::new(AtomicU64::new(0));
        mail.subscribe(Arc::new(Count(events.clone())));
        let accounts: Value =
            block_on(call("accounts.list", json!({}))).unwrap_or_else(|error| fail(&format!("{engine}: {error}")));
        let first_answer_ms = start.elapsed().as_secs_f64() * 1e3;
        let ready = footprint();
        // Writes go nowhere but the bench home.
        let path: Value = block_on(call("config.path", json!({}))).unwrap();
        if !path["path"].as_str().is_some_and(|path| path.starts_with(&home)) {
            fail(&format!("{engine} keeps settings at {path}, outside the bench home"));
        }
        if accounts.as_array().map(Vec::len) != Some(5) {
            fail(&format!("{engine} lists {accounts} instead of the 5 bench accounts"));
        }
        for case in cases {
            let line = measure(case, scale);
            emit(
                json!({ "kind": "case", "engine": engine, "round": round, "case": case.name, "command": case.command }),
                line,
            );
        }
        let end = footprint();
        emit(
            json!({ "kind": "process", "engine": engine, "round": round }),
            json!({
                "new_us": new_us, "first_answer_ms": first_answer_ms,
                "footprint_base_mb": base.0, "footprint_ready_mb": ready.0, "footprint_end_mb": end.0, "footprint_peak_mb": end.1,
                "events": events.load(Ordering::Relaxed),
            }),
        );
        drop(mail);
        std::process::exit(0);
    });
    // The Swift engine works on the main run loop and both deliver events there, as in an app.
    run_main_loop();
}

/// Warm-up, then timed calls one at a time, then calls `CONCURRENCY` at a time.
fn measure(case: &Case, scale: f64) -> Value {
    let (warm, sequential, concurrent) = if case.write { (30, 300, 300) } else { (200, 2000, 4000) };
    let scaled = |count: usize| ((count as f64 * scale) as usize).max(10);
    let once = |i: usize| async move {
        let _: Value =
            call(case.command, (case.args)(i)).await.unwrap_or_else(|error| fail(&format!("{}: {error}", case.name)));
    };
    for i in 0..scaled(warm) {
        block_on(once(i));
    }
    let count = scaled(sequential);
    let mut times = Vec::with_capacity(count);
    let all = Instant::now();
    for i in 0..count {
        let started = Instant::now();
        block_on(once(i));
        times.push(started.elapsed());
    }
    let sequential_per_s = count as f64 / all.elapsed().as_secs_f64();
    let count = scaled(concurrent);
    let all = Instant::now();
    block_on(futures::stream::iter(0..count).map(once).buffer_unordered(CONCURRENCY).collect::<Vec<()>>());
    let concurrent_per_s = count as f64 / all.elapsed().as_secs_f64();
    times.sort();
    let us = |duration: Duration| duration.as_secs_f64() * 1e6;
    let p99 = times[(times.len() * 99).div_ceil(100) - 1];
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    json!({
        "n": times.len(), "median_us": us(times[times.len() / 2]), "p99_us": us(p99), "mean_us": us(mean),
        "max_us": us(*times.last().unwrap()), "sequential_per_s": sequential_per_s,
        "concurrent_per_s": concurrent_per_s, "concurrency": CONCURRENCY,
    })
}

fn emit(mut head: Value, body: Value) {
    let fields: BTreeMap<String, Value> = serde_json::from_value(body).unwrap();
    for (key, value) in fields {
        head[key] = value;
    }
    println!("BENCH {head}");
}

/// This process's physical footprint (what Activity Monitor shows as Memory) and its peak so
/// far, in MB.
fn footprint() -> (f64, f64) {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let pointer = (&raw mut info).cast::<libc::rusage_info_t>();
    if unsafe { libc::proc_pid_rusage(std::process::id() as i32, libc::RUSAGE_INFO_V4, pointer) } != 0 {
        return (f64::NAN, f64::NAN);
    }
    let mb = |bytes: u64| (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0;
    (mb(info.ri_phys_footprint), mb(info.ri_lifetime_max_phys_footprint))
}
