// vpn-ns-gui: server picker for the vpn-ns Mullvad namespace.
// Countries > cities > servers (owned/rented, provider, Gbps), live search, one click switches.
// All VPN work is done by the `vpn-ns` script next to this binary; this program is only the front end.
mod flags;

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Key, Rect, RichText, Sense, TextStyle};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

const GREEN: Color32 = Color32::from_rgb(70, 180, 100);
const AMBER: Color32 = Color32::from_rgb(214, 158, 46);
const RED: Color32 = Color32::from_rgb(220, 80, 70);

#[derive(Deserialize)]
struct RawRelay {
    hostname: String,
    country_code: String,
    country_name: String,
    city_code: String,
    city_name: String,
    active: bool,
    owned: bool,
    provider: String,
    network_port_speed: u32,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Clone)]
struct Relay {
    hostname: String,
    country_code: String,
    country_name: String,
    city_code: String,
    city_name: String,
    owned: bool,
    provider: String,
    speed: u32,
    hay: String, // lower-case search text
}

impl From<RawRelay> for Relay {
    fn from(r: RawRelay) -> Self {
        let hay = format!(
            "{} {} {} {} {} {} {} gbps",
            r.country_name,
            r.country_code,
            r.city_name,
            r.hostname,
            r.provider,
            if r.owned { "owned mullvad" } else { "rented" },
            r.network_port_speed
        )
        .to_lowercase();
        Relay {
            hostname: r.hostname,
            country_code: r.country_code,
            country_name: r.country_name,
            city_code: r.city_code,
            city_name: r.city_name,
            owned: r.owned,
            provider: r.provider,
            speed: r.network_port_speed,
            hay,
        }
    }
}

impl Relay {
    fn owner_text(&self) -> String {
        if self.owned {
            "Mullvad-owned".to_string()
        } else {
            format!("rented · {}", self.provider)
        }
    }
}

struct City {
    code: String,
    name: String,
    servers: Vec<Relay>,
}

struct Country {
    code: String,
    name: String,
    cities: Vec<City>,
}

fn load() -> Result<Vec<Country>, String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let path = PathBuf::from(home).join(".config/vpn-ns/servers.json");
    let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: Vec<RawRelay> = serde_json::from_slice(&data).map_err(|e| format!("server list: {e}"))?;
    let mut relays: Vec<Relay> = raw
        .into_iter()
        .filter(|r| r.active && (r.kind.is_empty() || r.kind == "wireguard"))
        .map(Relay::from)
        .collect();
    relays.sort_by(|a, b| {
        (&a.country_name, &a.city_name, &a.hostname).cmp(&(&b.country_name, &b.city_name, &b.hostname))
    });
    let mut countries: Vec<Country> = Vec::new();
    for r in relays {
        if countries.last().map_or(true, |c| c.code != r.country_code) {
            countries.push(Country { code: r.country_code.clone(), name: r.country_name.clone(), cities: Vec::new() });
        }
        let country = countries.last_mut().unwrap();
        if country.cities.last().map_or(true, |c| c.code != r.city_code) {
            country.cities.push(City { code: r.city_code.clone(), name: r.city_name.clone(), servers: Vec::new() });
        }
        country.cities.last_mut().unwrap().servers.push(r);
    }
    Ok(countries)
}

fn find_script() -> PathBuf {
    if let Ok(p) = std::env::var("VPN_NS") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for cand in [dir.join("vpn-ns"), dir.join("../../../vpn-ns")] {
        if cand.is_file() {
            return cand;
        }
    }
    dir.join("vpn-ns")
}

#[derive(Default)]
struct KeyDialog {
    open: bool,
    mode: usize, // 0 = import a .conf, 1 = enter by hand
    path: String,
    private: String,
    address: String,
    store: String,
    show_key: bool,
    confirm_delete: bool,
    busy: bool,
    msg: String,
    msg_error: bool,
}

#[derive(Clone)]
struct VpnApp {
    name: String,
    title: String,
    cmd: String,
    running: bool,
}

#[derive(Clone)]
struct Installed {
    id: String,
    name: String,
    exec: String,
    icon: String,
}

#[derive(Default)]
struct AppsDialog {
    open: bool,
    items: Vec<VpnApp>,
    installed: Vec<Installed>,
    scanned: bool,
    scanning: bool,
    inflight: bool,
    last: Option<std::time::Instant>,
    mode: usize, // 0 = from installed apps, 1 = custom command
    filter: String,
    c_name: String,
    c_title: String,
    c_cmd: String,
    c_icon: String,
    confirm: String,
    busy: bool,
    msg: String,
    msg_error: bool,
}

enum AppsAct {
    Launch(String),
    Remove(String),
    AddInstalled(Installed),
    AddCustom,
}

#[derive(Clone, Copy)]
enum Browse {
    Import,
    Store,
}

enum KeyAct {
    Save,
    Delete,
    Browse(Browse),
}

enum Msg {
    AppsList(Vec<VpnApp>),
    Installed(Vec<Installed>),
    AppsDone(Result<String, String>),
    KeyStatus(String),
    KeyDone(Result<String, String>),
    Browsed(Browse, Result<String, String>),
    Current(String),
    Switched(Result<String, String>),
    Disconnected(Result<String, String>),
    Reloaded,
    Updated(Result<(), String>),
}

fn run_script(script: &PathBuf, args: &[&str]) -> Result<String, String> {
    match Command::new(script).args(args).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("failed").to_string()),
        Err(e) => Err(format!("{}: {e}", script.display())),
    }
}

/// like run_script, but the private key goes in through stdin (never in the arguments)
fn run_script_stdin(script: &PathBuf, args: &[&str], input: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(script)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", script.display()))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(input.as_bytes());
    }
    let o = child.wait_with_output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("failed").to_string())
    }
}

fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(h)) => format!("{h}/{rest}"),
        _ => p.to_string(),
    }
}

struct App {
    countries: Vec<Country>,
    speeds: Vec<u32>,
    min_speed: u32,
    key_ok: bool,
    key_path: String,
    key_addr: String,
    key_problem: String,
    key_prompted: bool,
    kd: KeyDialog,
    apps: AppsDialog,
    by_host: HashMap<String, Relay>,
    load_error: Option<String>,
    query: String,
    owned_only: bool,
    expanded: HashSet<String>,
    expanded_initial: bool,
    current: String,
    status: String,
    status_is_error: bool,
    busy: bool,
    confirm_down: bool,
    first_frame: bool,
    zoom: f32,
    updating: bool,
    list_time: Option<SystemTime>,
    script: PathBuf,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let zoom: f32 = cc
            .storage
            .and_then(|s| s.get_string("zoom"))
            .and_then(|z| z.parse().ok())
            .unwrap_or(0.8);
        cc.egui_ctx.set_zoom_factor(zoom);
        cc.egui_ctx.style_mut(|s| {
            s.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
            s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
        });
        let (tx, rx) = mpsc::channel();
        let script = find_script();
        let mut app = App {
            countries: Vec::new(),
            speeds: Vec::new(),
            min_speed: 0,
            key_ok: false,
            key_path: String::new(),
            key_addr: String::new(),
            key_problem: "Checking the key…".into(),
            key_prompted: false,
            kd: KeyDialog::default(),
            apps: AppsDialog::default(),
            by_host: HashMap::new(),
            load_error: None,
            query: String::new(),
            owned_only: false,
            expanded: HashSet::new(),
            expanded_initial: false,
            current: String::new(),
            status: "Updating server list…".into(),
            status_is_error: false,
            busy: false,
            confirm_down: false,
            first_frame: true,
            zoom,
            updating: false,
            list_time: None,
            script: script.clone(),
            tx: tx.clone(),
            rx,
        };
        app.reload();
        match std::env::var("VPN_NS_GUI_OPEN").as_deref() {
            Ok("apps") => app.apps.open = true,
            Ok("key") => app.kd.open = true,
            _ => {}
        }

        // refresh the cached list (once a day) in the background, then reload
        {
            let (script, tx, ctx) = (script.clone(), tx.clone(), cc.egui_ctx.clone());
            thread::spawn(move || {
                let _ = run_script(&script, &["refresh"]);
                let _ = tx.send(Msg::Reloaded);
                ctx.request_repaint();
            });
        }
        // is a valid key stored? (asks for it in a window if not)
        {
            let (script, tx, ctx) = (script.clone(), tx.clone(), cc.egui_ctx.clone());
            thread::spawn(move || {
                let line = run_script(&script, &["key", "status"]).unwrap_or_else(|e| format!("invalid||{e}"));
                let _ = tx.send(Msg::KeyStatus(line));
                ctx.request_repaint();
            });
        }
        // poll which server the tunnel is really connected to
        {
            let (script, tx, ctx) = (script, tx, cc.egui_ctx.clone());
            thread::spawn(move || loop {
                let host = run_script(&script, &["current"]).unwrap_or_default();
                if tx.send(Msg::Current(host)).is_err() {
                    break;
                }
                ctx.request_repaint();
                thread::sleep(Duration::from_secs(4));
            });
        }
        app
    }

    fn reload(&mut self) {
        match load() {
            Ok(c) => {
                self.by_host = c
                    .iter()
                    .flat_map(|c| c.cities.iter().flat_map(|c| c.servers.iter()))
                    .map(|r| (r.hostname.clone(), r.clone()))
                    .collect();
                let mut sp: Vec<u32> = self.by_host.values().map(|r| r.speed).collect();
                sp.sort_unstable();
                sp.dedup();
                self.speeds = sp;
                self.countries = c;
                self.load_error = None;
                self.list_time = std::env::var("HOME")
                    .ok()
                    .and_then(|h| std::fs::metadata(PathBuf::from(h).join(".config/vpn-ns/servers.json")).ok())
                    .and_then(|m| m.modified().ok());
                if self.status == "Updating server list…" {
                    self.status = String::new();
                }
            }
            Err(e) => self.load_error = Some(e),
        }
    }

    fn pump(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Current(h) => {
                    self.current = h;
                    if !self.expanded_initial {
                        if let Some(r) = self.by_host.get(&self.current) {
                            self.expanded.insert(r.country_code.clone());
                            self.expanded.insert(format!("{}-{}", r.country_code, r.city_code));
                            self.expanded_initial = true;
                        }
                    }
                }
                Msg::Switched(Ok(s)) => {
                    self.busy = false;
                    self.status = s;
                    self.status_is_error = false;
                }
                Msg::Switched(Err(e)) => {
                    self.busy = false;
                    if e.contains("no valid Mullvad key") {
                        self.kd.open = true;
                    }
                    self.status = format!("Switch failed: {e}");
                    self.status_is_error = true;
                }
                Msg::Disconnected(Ok(_)) => {
                    self.busy = false;
                    self.current.clear();
                    self.status = "Disconnected. Restart any VPN app (e.g. Firefox) to use the VPN again.".into();
                    self.status_is_error = false;
                }
                Msg::Disconnected(Err(e)) => {
                    self.busy = false;
                    if e.contains("still running") {
                        self.confirm_down = true;
                    } else {
                        self.status = format!("Disconnect failed: {e}");
                        self.status_is_error = true;
                    }
                }
                Msg::AppsList(v) => {
                    self.apps.items = v;
                    self.apps.inflight = false;
                }
                Msg::Installed(v) => {
                    self.apps.installed = v;
                    self.apps.scanned = true;
                    self.apps.scanning = false;
                }
                Msg::AppsDone(r) => {
                    self.apps.busy = false;
                    self.apps.last = None; // refresh the list now
                    match r {
                        Ok(line) => {
                            let mut p = line.splitn(3, '|');
                            let kind = p.next().unwrap_or("");
                            let _ = p.next();
                            let note = p.next().unwrap_or("");
                            self.apps.msg = if kind == "ok" { format!("Added. {note}") } else { "Removed.".to_string() };
                            self.apps.msg_error = false;
                            if kind == "ok" {
                                self.apps.c_name.clear();
                                self.apps.c_title.clear();
                                self.apps.c_cmd.clear();
                                self.apps.c_icon.clear();
                            }
                        }
                        Err(e) => {
                            self.apps.msg = e;
                            self.apps.msg_error = true;
                        }
                    }
                }
                Msg::KeyStatus(line) => self.apply_key_status(&line),
                Msg::KeyDone(Ok(line)) => {
                    self.kd.busy = false;
                    self.kd.private.clear();
                    self.apply_key_status(&line);
                    self.kd.msg = if self.key_ok {
                        "Key saved. A running tunnel uses it at once; otherwise it is used when you connect.".into()
                    } else {
                        "Key deleted.".into()
                    };
                    self.kd.msg_error = false;
                }
                Msg::KeyDone(Err(e)) => {
                    self.kd.busy = false;
                    self.kd.msg = e;
                    self.kd.msg_error = true;
                }
                Msg::Browsed(which, Ok(p)) => {
                    if !p.is_empty() {
                        match which {
                            Browse::Import => self.kd.path = p,
                            Browse::Store => self.kd.store = p,
                        }
                    }
                }
                Msg::Browsed(_, Err(e)) => {
                    if e != "failed" {
                        self.kd.msg = e;
                        self.kd.msg_error = true;
                    }
                }
                Msg::Reloaded => self.reload(),
                Msg::Updated(Ok(())) => {
                    self.updating = false;
                    self.reload();
                    self.status = format!("Server list updated: {} servers", self.by_host.len());
                    self.status_is_error = false;
                }
                Msg::Updated(Err(e)) => {
                    self.updating = false;
                    self.status = format!("Update failed: {e}");
                    self.status_is_error = true;
                }
            }
        }
    }

    fn apply_key_status(&mut self, line: &str) {
        let mut p = line.splitn(3, '|');
        let kind = p.next().unwrap_or("");
        let path = p.next().unwrap_or("").to_string();
        let rest = p.next().unwrap_or("").to_string();
        if kind == "ok" {
            self.key_ok = true;
            self.key_problem.clear();
            self.key_addr = rest;
        } else {
            self.key_ok = false;
            self.key_addr.clear();
            self.key_problem = if kind == "missing" {
                "No Mullvad key is stored yet. Add one to connect.".into()
            } else {
                format!("The stored key is not valid: {rest}")
            };
            if !self.key_prompted {
                self.kd.open = true;
            }
        }
        self.key_prompted = true;
        if !path.is_empty() {
            self.key_path = path.clone();
            if self.kd.store.is_empty() || kind == "ok" {
                self.kd.store = path;
            }
        }
    }

    fn key_save(&mut self, ctx: &egui::Context) {
        if self.kd.busy {
            return;
        }
        self.kd.busy = true;
        self.kd.msg = "Saving…".into();
        self.kd.msg_error = false;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        let store = expand_home(self.kd.store.trim());
        let mode = self.kd.mode;
        let path = expand_home(self.kd.path.trim());
        let private = self.kd.private.trim().to_string();
        let address = self.kd.address.trim().to_string();
        thread::spawn(move || {
            let mut args: Vec<&str> = Vec::new();
            let res = if mode == 0 {
                args.extend(["key", "import", path.as_str()]);
                if !store.is_empty() {
                    args.extend(["--to", store.as_str()]);
                }
                run_script(&script, &args)
            } else {
                args.extend(["key", "set", "--address", address.as_str()]);
                if !store.is_empty() {
                    args.extend(["--to", store.as_str()]);
                }
                run_script_stdin(&script, &args, &format!("{private}\n"))
            };
            let _ = tx.send(Msg::KeyDone(res));
            ctx.request_repaint();
        });
    }

    fn key_delete(&mut self, ctx: &egui::Context) {
        self.kd.busy = true;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let res = run_script(&script, &["key", "remove"]).and_then(|_| run_script(&script, &["key", "status"]));
            let _ = tx.send(Msg::KeyDone(res));
            ctx.request_repaint();
        });
    }

    fn key_browse(&mut self, ctx: &egui::Context, which: Browse) {
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        let home = std::env::var("HOME").unwrap_or_default();
        let (kind, start) = match which {
            Browse::Import => ("file", home),
            Browse::Store => ("save", if self.kd.store.is_empty() { home } else { expand_home(&self.kd.store) }),
        };
        thread::spawn(move || {
            let r = run_script(&script, &["browse", kind, &start]);
            let _ = tx.send(Msg::Browsed(which, r));
            ctx.request_repaint();
        });
    }

    fn apps_refresh(&mut self, ctx: &egui::Context) {
        if self.apps.inflight {
            return;
        }
        self.apps.inflight = true;
        self.apps.last = Some(std::time::Instant::now());
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let list = run_script(&script, &["apps", "list"]).unwrap_or_default();
            let status = run_script(&script, &["apps", "status"]).unwrap_or_default();
            let running: HashSet<&str> = status.lines().filter_map(|l| l.split_once('|')).filter(|(_, v)| *v == "1").map(|(n, _)| n).collect();
            let items: Vec<VpnApp> = list
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.splitn(4, '|').collect();
                    (f.len() >= 3).then(|| VpnApp { name: f[0].into(), title: f[1].into(), cmd: f[2].into(), running: running.contains(f[0]) })
                })
                .collect();
            let _ = tx.send(Msg::AppsList(items));
            ctx.request_repaint();
        });
    }

    fn apps_scan(&mut self, ctx: &egui::Context) {
        self.apps.scanning = true;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let out = run_script(&script, &["apps", "scan"]).unwrap_or_default();
            let v: Vec<Installed> = out
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.splitn(4, '|').collect();
                    (f.len() == 4).then(|| Installed { id: f[0].into(), name: f[1].into(), exec: f[2].into(), icon: f[3].into() })
                })
                .collect();
            let _ = tx.send(Msg::Installed(v));
            ctx.request_repaint();
        });
    }

    fn apps_run(&mut self, ctx: &egui::Context, args: Vec<String>) {
        self.apps.busy = true;
        self.apps.msg = "Working…".into();
        self.apps.msg_error = false;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let a: Vec<&str> = args.iter().map(|x| x.as_str()).collect();
            let res = run_script(&script, &a);
            let _ = tx.send(Msg::AppsDone(res));
            ctx.request_repaint();
        });
    }

    fn app_launch(&mut self, name: String) {
        self.apps.msg = format!("Started {name} through the VPN.");
        self.apps.msg_error = false;
        let script = self.script.clone();
        thread::spawn(move || {
            let _ = Command::new(&script).args(["app", &name]).status();
        });
    }

    fn update_list(&mut self, ctx: &egui::Context) {
        if self.updating {
            return;
        }
        self.updating = true;
        self.status = "Downloading the server list from Mullvad…".into();
        self.status_is_error = false;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let res = run_script(&script, &["refresh", "force"]).map(|_| ());
            let _ = tx.send(Msg::Updated(res));
            ctx.request_repaint();
        });
    }

    fn disconnect(&mut self, ctx: &egui::Context, force: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "Disconnecting…".into();
        self.status_is_error = false;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let res = run_script(&script, if force { &["down", "force"][..] } else { &["down"][..] });
            let _ = tx.send(Msg::Disconnected(res));
            ctx.request_repaint();
        });
    }

    fn switch(&mut self, ctx: &egui::Context, target: String, label: String) {
        if self.busy {
            return;
        }
        if !self.key_ok {
            self.kd.open = true;
            self.status = "A valid Mullvad key is needed first.".into();
            self.status_is_error = true;
            return;
        }
        self.busy = true;
        self.status = format!("Switching to {label}…");
        self.status_is_error = false;
        let (script, tx, ctx) = (self.script.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let res = run_script(&script, &["switch", &target]).map(|_| format!("Switched to {label}"));
            let host = run_script(&script, &["current"]).unwrap_or_default();
            let _ = tx.send(Msg::Switched(res));
            let _ = tx.send(Msg::Current(host));
            ctx.request_repaint();
        });
    }
}

fn paint_flag(ui: &egui::Ui, cc: &str, rect: Rect) {
    match flags::bytes(cc) {
        Some(bytes) => {
            egui::Image::from_bytes(format!("bytes://flag-{cc}.png"), bytes)
                .corner_radius(3.0)
                .paint_at(ui, rect);
        }
        None => {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                cc.to_uppercase(),
                FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

fn age_text(t: Option<SystemTime>) -> String {
    match t.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()) {
        None => "list age unknown".into(),
        Some(s) if s < 60 => "list updated just now".into(),
        Some(s) if s < 3600 => format!("list updated {} min ago", s / 60),
        Some(s) if s < 86400 => format!("list updated {} h ago", s / 3600),
        Some(s) => format!("list updated {} d ago", s / 86400),
    }
}

fn paint_dot(ui: &egui::Ui, center: egui::Pos2, color: Color32) {
    ui.painter().circle_filled(center, 4.0, color);
}

fn paint_arrow(ui: &egui::Ui, center: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![center + vec2(-4.5, -2.5), center + vec2(4.5, -2.5), center + vec2(0.0, 3.5)]
    } else {
        vec![center + vec2(-2.5, -4.5), center + vec2(-2.5, 4.5), center + vec2(3.5, 0.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// info text after a name, plus a green "connected here" marker when `here`
fn paint_info(ui: &egui::Ui, x: f32, cy: f32, info: String, here: bool, weak: Color32) {
    let r = ui.painter().text(pos2(x, cy), Align2::LEFT_CENTER, info, FontId::proportional(13.0), if here { GREEN } else { weak });
    if here {
        paint_dot(ui, pos2(r.right() + 14.0, cy), GREEN);
        ui.painter().text(pos2(r.right() + 24.0, cy), Align2::LEFT_CENTER, "connected here", FontId::proportional(13.0), GREEN);
    }
}

fn row(ui: &mut egui::Ui, height: f32, highlight: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let bg = if highlight {
            Color32::from_rgba_unmultiplied(70, 180, 100, 40)
        } else if resp.hovered() {
            ui.visuals().widgets.hovered.weak_bg_fill
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 4.0, bg);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// small "random" pill at the right of a row; returns true when clicked
fn random_pill(ui: &mut egui::Ui, row_rect: Rect, id: &str) -> bool {
    let rect = Rect::from_min_size(pos2(row_rect.right() - 82.0, row_rect.center().y - 11.0), vec2(72.0, 22.0));
    let resp = ui.interact(rect, egui::Id::new(("random", id)), Sense::click()).on_hover_text("Connect to a random server here");
    let (fill, text) = if resp.hovered() {
        (ui.visuals().widgets.active.weak_bg_fill, ui.visuals().strong_text_color())
    } else {
        (ui.visuals().widgets.inactive.weak_bg_fill, ui.visuals().weak_text_color())
    };
    ui.painter().rect_filled(rect, 11.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, "random", FontId::proportional(12.5), text);
    resp.clicked()
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("zoom", self.zoom.to_string());
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        if self.apps.open {
            if self.apps.last.map_or(true, |t| t.elapsed() > Duration::from_secs(4)) {
                self.apps_refresh(ctx);
            }
            if !self.apps.scanned && !self.apps.scanning {
                self.apps_scan(ctx);
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }

        let tokens: Vec<String> = self.query.to_lowercase().split_whitespace().map(String::from).collect();
        let searching = !tokens.is_empty();
        let owned_only = self.owned_only;
        let min_speed = self.min_speed;
        let matches = |r: &Relay| (!owned_only || r.owned) && r.speed >= min_speed && tokens.iter().all(|t| r.hay.contains(t.as_str()));

        // single hit: remember it so Enter in the search box can connect
        let mut hits = 0usize;
        let mut only_hit: Option<Relay> = None;
        for r in self.countries.iter().flat_map(|c| c.cities.iter().flat_map(|c| c.servers.iter())) {
            if matches(r) {
                hits += 1;
                only_hit = Some(r.clone());
            }
        }

        let mut go: Option<(String, String)> = None; // (switch target, label)
        let mut toggle: Option<String> = None;
        let mut clear = false;
        let mut refresh = false;
        let mut zoom_step = 1.0f32;
        let mut open_key = false;
        let mut open_apps = false;
        let mut disconnect = false;
        let mut apps_act: Option<AppsAct> = None;
        let mut key_act: Option<KeyAct> = None;

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                match self.by_host.get(&self.current) {
                    Some(r) => {
                        let (rect, _) = ui.allocate_exact_size(vec2(26.0, 18.0), Sense::hover());
                        paint_flag(ui, &r.country_code, rect);
                        let (d, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
                        paint_dot(ui, d.center(), GREEN);
                        ui.label(RichText::new("Connected").color(GREEN).strong());
                        ui.label(format!("{}, {}  ·  {}  ·  ", r.city_name, r.country_name, r.hostname));
                        ui.label(RichText::new(r.owner_text()).color(if r.owned { GREEN } else { AMBER }));
                        ui.label(format!("·  {} Gbps", r.speed));
                    }
                    None => {
                        let (d, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
                        paint_dot(ui, d.center(), AMBER);
                        ui.label(RichText::new("Not connected").color(AMBER).strong());
                        ui.label(RichText::new("click a server to connect").weak());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("A+").on_hover_text("Bigger (Ctrl and plus)").clicked() {
                        zoom_step = 1.1;
                    }
                    if ui.button("A-").on_hover_text("Smaller (Ctrl and minus)").clicked() {
                        zoom_step = 1.0 / 1.1;
                    }
                    if ui
                        .add_enabled(!self.updating, egui::Button::new(if self.updating { "Updating…" } else { "Update list" }))
                        .on_hover_text("Download the current server list from Mullvad now (otherwise it refreshes once a day)")
                        .clicked()
                    {
                        refresh = true;
                    }
                    let key_btn = if self.key_ok {
                        egui::Button::new("Key…")
                    } else {
                        egui::Button::new(RichText::new("Key needed").color(RED))
                    };
                    if ui.add(key_btn).on_hover_text("Set up, replace or delete your Mullvad key").clicked() {
                        open_key = true;
                    }
                    if ui.button("VPN apps…").on_hover_text("Choose which apps start through the VPN").clicked() {
                        open_apps = true;
                    }
                    if ui
                        .add_enabled(!self.current.is_empty() && !self.busy, egui::Button::new("Disconnect"))
                        .on_hover_text("Turn the VPN off. VPN apps lose their connection; everything else is unaffected")
                        .clicked()
                    {
                        disconnect = true;
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let te = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search country, city, server, provider…   (Esc clears)")
                        .desired_width((ui.available_width() - 80.0).max(120.0)),
                );
                if self.first_frame {
                    te.request_focus();
                }
                if te.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) && hits == 1 {
                    if let Some(r) = &only_hit {
                        go = Some((r.hostname.clone(), format!("{}, {} ({})", r.city_name, r.country_name, r.hostname)));
                    }
                }
                if ui.button("Clear").on_hover_text("Clear search").clicked() {
                    clear = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.owned_only, "Mullvad-owned only");
                ui.add_space(14.0);
                ui.label("Speed:");
                egui::ComboBox::from_id_salt("speed")
                    .selected_text(if self.min_speed == 0 { "any speed".to_string() } else { format!("at least {} Gbps", self.min_speed) })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.min_speed, 0, "any speed");
                        for sp in &self.speeds {
                            ui.selectable_value(&mut self.min_speed, *sp, format!("at least {sp} Gbps"));
                        }
                    });
            });
            ui.add_space(6.0);
        });

        if open_key {
            self.kd.open = true;
            self.kd.msg.clear();
        }
        if self.kd.open {
            let mut open = true;
            egui::Window::new("Mullvad key setup")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_width(540.0);
                    if self.key_ok {
                        ui.label(RichText::new(format!("Key stored at {}  (tunnel address {})", self.key_path, self.key_addr)).color(GREEN));
                    } else {
                        ui.label(RichText::new(&self.key_problem).color(AMBER).strong());
                    }
                    ui.label(RichText::new("The key stays on this computer, in a file only you can read.").weak());
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.kd.mode, 0, "Import a Mullvad .conf file");
                        ui.selectable_value(&mut self.kd.mode, 1, "Enter the key by hand");
                    });
                    ui.add_space(4.0);
                    if self.kd.mode == 0 {
                        ui.label("Get the file in your Mullvad account: WireGuard configuration > Linux > Generate key > Download file.");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.kd.path).hint_text("/path/to/mullvad.conf").desired_width(410.0));
                            if ui.button("Browse…").clicked() {
                                key_act = Some(KeyAct::Browse(Browse::Import));
                            }
                        });
                    } else {
                        ui.label("The WireGuard private key (44 characters) and the tunnel address Mullvad assigned to it.");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.kd.private).password(!self.kd.show_key).hint_text("private key").desired_width(410.0));
                            ui.checkbox(&mut self.kd.show_key, "show");
                        });
                        ui.add(egui::TextEdit::singleline(&mut self.kd.address).hint_text("tunnel address, e.g. 10.64.12.34/32").desired_width(410.0));
                    }
                    ui.add_space(6.0);
                    ui.label("Store the key at (a folder gets mullvad.conf inside):");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.kd.store).desired_width(410.0));
                        if ui.button("Browse…").clicked() {
                            key_act = Some(KeyAct::Browse(Browse::Store));
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let ready = !self.kd.busy
                            && if self.kd.mode == 0 {
                                !self.kd.path.trim().is_empty()
                            } else {
                                !self.kd.private.trim().is_empty() && !self.kd.address.trim().is_empty()
                            };
                        if ui.add_enabled(ready, egui::Button::new("Save key")).clicked() {
                            key_act = Some(KeyAct::Save);
                        }
                        if self.key_ok {
                            let label = if self.kd.confirm_delete { "Really delete?" } else { "Delete stored key" };
                            if ui.add_enabled(!self.kd.busy, egui::Button::new(label)).clicked() {
                                if self.kd.confirm_delete {
                                    key_act = Some(KeyAct::Delete);
                                    self.kd.confirm_delete = false;
                                } else {
                                    self.kd.confirm_delete = true;
                                }
                            }
                        }
                        if ui.button("Close").clicked() {
                            self.kd.open = false;
                        }
                    });
                    if !self.kd.msg.is_empty() {
                        ui.label(RichText::new(&self.kd.msg).color(if self.kd.msg_error { RED } else { GREEN }));
                    }
                });
            if !open {
                self.kd.open = false;
            }
        }

        if disconnect {
            self.disconnect(ctx, false);
        }
        if self.confirm_down {
            egui::Window::new("Disconnect the VPN?")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Apps are still open inside the VPN (for example the VPN Firefox).");
                    ui.label("They will lose their connection for good and must be restarted.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Disconnect anyway").clicked() {
                            self.confirm_down = false;
                            self.disconnect(ctx, true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_down = false;
                            self.status.clear();
                        }
                    });
                });
        }
        if open_apps {
            self.apps.open = true;
            self.apps.msg.clear();
            self.apps.last = None;
        }
        if self.apps.open {
            let mut open = true;
            egui::Window::new("VPN apps")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(680.0)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(RichText::new("These apps always start through the Mullvad VPN. Everything else keeps your normal connection.").weak());
                    ui.separator();
                    if self.apps.items.is_empty() {
                        ui.label("No apps yet. Add one below.");
                    }
                    for it in &self.apps.items {
                        ui.horizontal(|ui| {
                            let (d, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
                            paint_dot(ui, d.center(), if it.running { GREEN } else { Color32::GRAY });
                            ui.label(RichText::new(&it.title).strong());
                            if it.running {
                                ui.label(RichText::new("running in the VPN").color(GREEN));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let rm = if self.apps.confirm == it.name { "Really remove?" } else { "Remove" };
                                if ui.button(rm).clicked() {
                                    if self.apps.confirm == it.name {
                                        apps_act = Some(AppsAct::Remove(it.name.clone()));
                                        self.apps.confirm.clear();
                                    } else {
                                        self.apps.confirm = it.name.clone();
                                    }
                                }
                                if ui.button("Launch").clicked() {
                                    apps_act = Some(AppsAct::Launch(it.name.clone()));
                                }
                            });
                        });
                        ui.label(RichText::new(&it.cmd).weak().small());
                        ui.add_space(2.0);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.apps.mode, 0, "Add from installed apps");
                        ui.selectable_value(&mut self.apps.mode, 1, "Add a custom command");
                    });
                    if self.apps.mode == 0 {
                        ui.add(egui::TextEdit::singleline(&mut self.apps.filter).hint_text("Filter installed apps…").desired_width(f32::INFINITY));
                        let f = self.apps.filter.to_lowercase();
                        if self.apps.scanning {
                            ui.label(RichText::new("Looking for installed apps…").weak());
                        }
                        egui::ScrollArea::vertical().max_height(220.0).id_salt("installed").show(ui, |ui| {
                            for ins in &self.apps.installed {
                                if !f.is_empty() && !(ins.name.to_lowercase().contains(&f) || ins.exec.to_lowercase().contains(&f)) {
                                    continue;
                                }
                                let first = ins.exec.split_whitespace().next().unwrap_or("");
                                let added = self.apps.items.iter().any(|a| a.cmd.split_whitespace().next().unwrap_or("") == first);
                                ui.horizontal(|ui| {
                                    ui.label(&ins.name);
                                    ui.label(RichText::new(&ins.exec).weak().small());
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if added {
                                            ui.label(RichText::new("in the list").weak());
                                        } else if ui.add_enabled(!self.apps.busy, egui::Button::new("Add")).clicked() {
                                            apps_act = Some(AppsAct::AddInstalled(ins.clone()));
                                        }
                                    });
                                });
                            }
                        });
                    } else {
                        ui.label("Name (lower-case letters, digits, - and _), menu title, command, optional icon name:");
                        ui.add(egui::TextEdit::singleline(&mut self.apps.c_name).hint_text("name, e.g. telegram").desired_width(f32::INFINITY));
                        ui.add(egui::TextEdit::singleline(&mut self.apps.c_title).hint_text("menu title, e.g. Telegram (VPN)").desired_width(f32::INFINITY));
                        ui.add(egui::TextEdit::singleline(&mut self.apps.c_cmd).hint_text("command, e.g. telegram-desktop").desired_width(f32::INFINITY));
                        ui.add(egui::TextEdit::singleline(&mut self.apps.c_icon).hint_text("icon name (optional)").desired_width(f32::INFINITY));
                        let ready = !self.apps.busy && !self.apps.c_name.trim().is_empty() && !self.apps.c_title.trim().is_empty() && !self.apps.c_cmd.trim().is_empty();
                        if ui.add_enabled(ready, egui::Button::new("Add app")).clicked() {
                            apps_act = Some(AppsAct::AddCustom);
                        }
                    }
                    if !self.apps.msg.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&self.apps.msg).color(if self.apps.msg_error { RED } else { GREEN }));
                    }
                });
            if !open {
                self.apps.open = false;
            }
        }

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                if let Some(e) = &self.load_error {
                    ui.label(RichText::new(format!("Server list: {e}")).color(RED));
                } else if !self.status.is_empty() {
                    let t = RichText::new(&self.status);
                    ui.label(if self.status_is_error { t.color(RED) } else { t.weak() });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{hits} {}  ·  {}", if hits == 1 { "server" } else { "servers" }, age_text(self.list_time))).weak());
                });
            });
            ui.add_space(3.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let weak = ui.visuals().weak_text_color();
                let strong = ui.visuals().strong_text_color();
                let normal = ui.visuals().text_color();
                let cur = self.by_host.get(&self.current);
                for country in &self.countries {
                    let mut n_city = 0;
                    let mut n_srv = 0;
                    for c in &country.cities {
                        let k = c.servers.iter().filter(|r| matches(r)).count();
                        if k > 0 {
                            n_city += 1;
                            n_srv += k;
                        }
                    }
                    if n_srv == 0 {
                        continue;
                    }
                    let open = searching || self.expanded.contains(&country.code);
                    let here = cur.map_or(false, |r| r.country_code == country.code);

                    // country row
                    let resp = row(ui, 32.0, false);
                    let rect = resp.rect;
                    let cy = rect.center().y;
                    paint_arrow(ui, pos2(rect.left() + 14.0, cy), open, weak);
                    paint_flag(ui, &country.code, Rect::from_min_size(pos2(rect.left() + 28.0, cy - 9.0), vec2(26.0, 18.0)));
                    let name = ui.painter().text(pos2(rect.left() + 64.0, cy), Align2::LEFT_CENTER, &country.name, FontId::proportional(17.0), strong);
                    let info = format!("{} {} · {} {}", n_city, if n_city == 1 { "city" } else { "cities" }, n_srv, if n_srv == 1 { "server" } else { "servers" });
                    paint_info(ui, name.right() + 12.0, cy, info, here, weak);
                    if random_pill(ui, rect, &country.code) {
                        go = Some((country.code.clone(), format!("{} (random)", country.name)));
                    } else if resp.clicked() {
                        toggle = Some(country.code.clone());
                    }
                    if !open {
                        continue;
                    }

                    for city in &country.cities {
                        let shown: Vec<&Relay> = city.servers.iter().filter(|r| matches(r)).collect();
                        if shown.is_empty() {
                            continue;
                        }
                        let key = format!("{}-{}", country.code, city.code);
                        let open = searching || self.expanded.contains(&key);
                        let here = cur.map_or(false, |r| r.country_code == country.code && r.city_code == city.code);
                        let owned = shown.iter().filter(|r| r.owned).count();
                        let top = shown.iter().map(|r| r.speed).max().unwrap_or(0);

                        // city row
                        let resp = row(ui, 28.0, false);
                        let rect = resp.rect;
                        let cy = rect.center().y;
                        paint_arrow(ui, pos2(rect.left() + 40.0, cy), open, weak);
                        let name = ui.painter().text(pos2(rect.left() + 54.0, cy), Align2::LEFT_CENTER, &city.name, FontId::proportional(15.5), normal);
                        let info = format!("{} {} · {} Mullvad-owned · up to {} Gbps", shown.len(), if shown.len() == 1 { "server" } else { "servers" }, owned, top);
                        paint_info(ui, name.right() + 12.0, cy, info, here, weak);
                        if random_pill(ui, rect, &key) {
                            go = Some((key.clone(), format!("{}, {} (random)", city.name, country.name)));
                        } else if resp.clicked() {
                            toggle = Some(key.clone());
                        }
                        if !open {
                            continue;
                        }

                        for r in shown {
                            let is_cur = r.hostname == self.current;
                            let resp = row(ui, 25.0, is_cur);
                            let rect = resp.rect;
                            let cy = rect.center().y;
                            if is_cur {
                                paint_dot(ui, pos2(rect.left() + 64.0, cy), GREEN);
                            }
                            ui.painter().text(pos2(rect.left() + 78.0, cy), Align2::LEFT_CENTER, &r.hostname, FontId::monospace(13.5), if is_cur { strong } else { normal });
                            ui.painter().text(pos2(rect.left() + 250.0, cy), Align2::LEFT_CENTER, r.owner_text(), FontId::proportional(14.0), if r.owned { GREEN } else { AMBER });
                            ui.painter().text(pos2(rect.right() - 14.0, cy), Align2::RIGHT_CENTER, format!("{} Gbps", r.speed), FontId::proportional(14.0), if r.speed >= 20 { strong } else { weak });
                            if resp.clicked() {
                                go = Some((r.hostname.clone(), format!("{}, {} ({})", r.city_name, r.country_name, r.hostname)));
                            }
                        }
                    }
                }
                if hits == 0 && self.load_error.is_none() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new("No server matches").weak()));
                }
            });
        });

        if clear || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.query.clear();
        }
        if let Some(k) = toggle {
            if !self.expanded.remove(&k) {
                self.expanded.insert(k);
            }
        }
        match apps_act {
            Some(AppsAct::Launch(n)) => self.app_launch(n),
            Some(AppsAct::Remove(n)) => self.apps_run(ctx, vec!["apps".into(), "remove".into(), n]),
            Some(AppsAct::AddInstalled(i)) => {
                let name: String = i
                    .id
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                    .collect::<String>()
                    .trim_start_matches(['-', '_'])
                    .to_string();
                let title = format!("{} (VPN)", i.name);
                self.apps_run(ctx, vec!["apps".into(), "add".into(), name, title, i.exec, i.icon]);
            }
            Some(AppsAct::AddCustom) => {
                let a = &self.apps;
                let args = vec!["apps".into(), "add".into(), a.c_name.trim().to_string(), a.c_title.trim().to_string(), a.c_cmd.trim().to_string(), a.c_icon.trim().to_string()];
                self.apps_run(ctx, args);
            }
            None => {}
        }
        match key_act {
            Some(KeyAct::Save) => self.key_save(ctx),
            Some(KeyAct::Delete) => self.key_delete(ctx),
            Some(KeyAct::Browse(w)) => self.key_browse(ctx, w),
            None => {}
        }
        if refresh {
            self.update_list(ctx);
        }
        if zoom_step != 1.0 {
            ctx.set_zoom_factor((ctx.zoom_factor() * zoom_step).clamp(0.5, 2.0));
        }
        self.zoom = ctx.zoom_factor();
        if let Some((target, label)) = go {
            self.switch(ctx, target, label);
        }
        self.first_frame = false;
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 860.0])
            .with_min_inner_size([560.0, 360.0])
            .with_title("Mullvad VPN servers")
            .with_app_id("vpn-ns-gui"),
        ..Default::default()
    };
    eframe::run_native("vpn-ns-gui", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
