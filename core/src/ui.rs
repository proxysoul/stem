use nu_ansi_term::{Color, Style};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Look,
    Change,
    Run,
    Know,
    Ask,
}

pub fn kind_of(call: &str) -> Kind {
    match call {
        "read" | "files" | "grep" | "project" => Kind::Look,
        "write" | "edit" | "rename" | "remove" => Kind::Change,
        "sh" | "test" | "run" => Kind::Run,
        "ask" => Kind::Ask,
        _ => Kind::Know,
    }
}

type Rgb = (u8, u8, u8);

const LEAF: Rgb = (108, 210, 138);
const LEAF_GLOW: Rgb = (196, 255, 210);
const AMBER: Rgb = (242, 182, 88);
const AMBER_GLOW: Rgb = (255, 226, 170);
const TRACK: Rgb = (70, 84, 92);
const NUCLEUS: Rgb = (214, 92, 146);
const FAILED: Rgb = (255, 107, 107);
const FLASH: Duration = Duration::from_millis(450);
const BAR: usize = 36;

fn kind_color(kind: Kind) -> Rgb {
    match kind {
        Kind::Look => (122, 182, 236),
        Kind::Change => (108, 210, 138),
        Kind::Run => (236, 138, 184),
        Kind::Know => (160, 142, 244),
        Kind::Ask => (242, 182, 88),
    }
}

fn glyph(bytes: usize) -> &'static str {
    match bytes {
        0..=1_500 => "▪",
        1_501..=15_000 => "■",
        15_001..=150_000 => "█",
        _ => "██",
    }
}

struct Block {
    kind: Kind,
    ok: bool,
    bytes: usize,
    born: Instant,
}

#[derive(Clone, PartialEq)]
pub enum ServerState {
    Starting,
    Ready,
    Failed,
}

#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    phase: String,
    detail: String,
    scripts: usize,
    started: Option<Instant>,
    drawn: usize,
    hit: Option<u64>,
    cache_until: Option<(Instant, Duration)>,
    plan: Vec<(String, bool)>,
    servers: Vec<(String, ServerState)>,
    usage: Option<crate::sys::Usage>,
    sampled: Option<Instant>,
    banner: Option<String>,
    context: Option<u64>,
    ejected: (usize, usize),
}

#[derive(Clone)]
pub struct Board {
    state: Arc<Mutex<State>>,
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    live: bool,
}

fn to_256(c: Rgb) -> u8 {
    let q = |v: u8| if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 };
    16 + 36 * q(c.0) + 6 * q(c.1) + q(c.2)
}

fn fg(c: Rgb, truecolor: bool) -> String {
    if truecolor { format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2) } else { format!("\x1b[38;5;{}m", to_256(c)) }
}

fn paint(c: Rgb, text: &str, truecolor: bool) -> String {
    format!("{}{text}\x1b[0m", fg(c, truecolor))
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)) as u8;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

pub fn tokens(n: usize) -> String {
    if n < 1000 { format!("{n}") } else if n < 10_000 { format!("{:.1}k", n as f32 / 1000.0) } else { format!("{}k", n / 1000) }
}

fn bar(fraction: Option<f32>, t: f32, truecolor: bool) -> String {
    bar_in(fraction, t, truecolor, LEAF)
}

fn bar_in(fraction: Option<f32>, t: f32, truecolor: bool, tint: Rgb) -> String {
    let glow_tint = if tint == LEAF { LEAF_GLOW } else { AMBER_GLOW };
    let mut out = String::new();
    match fraction {
        Some(f) => {
            let filled = f.clamp(0.0, 1.0) * BAR as f32;
            let full = filled.floor() as usize;
            let shimmer = (t * 14.0) % (BAR as f32 + 10.0) - 5.0;
            for i in 0..BAR {
                let glow = (1.0 - ((i as f32 - shimmer).abs() / 3.0)).max(0.0);
                if i < full {
                    out.push_str(&paint(mix(tint, glow_tint, glow * 0.8), "━", truecolor));
                } else if i == full && filled - full as f32 > 0.25 {
                    out.push_str(&paint(tint, "╸", truecolor));
                } else {
                    out.push_str(&paint(TRACK, "━", truecolor));
                }
            }
        }
        None => {
            let span = 9.0;
            let travel = BAR as f32 + span;
            let head = (t * 16.0) % (travel * 2.0);
            let head = if head > travel { travel * 2.0 - head } else { head } - span / 2.0;
            for i in 0..BAR {
                let d = (i as f32 - head).abs();
                let glow = (1.0 - d / (span / 2.0)).max(0.0);
                out.push_str(&paint(if glow > 0.0 { mix(TRACK, tint, glow) } else { TRACK }, "━", truecolor));
            }
        }
    }
    out
}

fn blocks_line(blocks: &[Block], room: usize, frame: usize, truecolor: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut width = 0;
    for b in blocks.iter().rev() {
        let g = if b.ok { glyph(b.bytes) } else { "✕" };
        let w = g.chars().count();
        if width + w > room {
            break;
        }
        width += w;
        let age = b.born.elapsed();
        let color = if !b.ok {
            if age < FLASH * 3 && (frame / 3) % 2 == 0 { (255, 200, 200) } else { FAILED }
        } else if age < FLASH {
            mix((255, 255, 255), kind_color(b.kind), age.as_secs_f32() / FLASH.as_secs_f32())
        } else {
            kind_color(b.kind)
        };
        parts.push(paint(color, g, truecolor));
    }
    parts.reverse();
    let hidden = blocks.len().saturating_sub(parts.len());
    let prefix = if hidden > 0 { Style::new().dimmed().paint(format!("+{hidden} ")).to_string() } else { String::new() };
    format!("{prefix}{}", parts.join(""))
}

impl Board {
    pub fn new() -> Board {
        Board { state: Arc::default(), running: Arc::new(AtomicBool::new(false)), paused: Arc::new(AtomicBool::new(false)), live: std::io::stderr().is_terminal() }
    }

    pub fn quiet() -> Board {
        Board { state: Arc::default(), running: Arc::new(AtomicBool::new(false)), paused: Arc::new(AtomicBool::new(false)), live: false }
    }

    pub fn phase(&self, text: &str) {
        let mut s = self.state.lock().unwrap();
        s.phase = text.to_string();
        s.detail.clear();
    }

    pub fn script(&self) {
        self.state.lock().unwrap().scripts += 1;
    }

    pub fn cache(&self, hit: u64, until: Option<(Instant, Duration)>) {
        let mut s = self.state.lock().unwrap();
        s.hit = Some(hit);
        if until.is_some() {
            s.cache_until = until;
        }
    }

    pub fn plan(&self, steps: Vec<(String, bool)>) {
        self.state.lock().unwrap().plan = steps;
    }

    pub fn server(&self, name: &str, state: ServerState) {
        let mut s = self.state.lock().unwrap();
        match s.servers.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = state,
            None => s.servers.push((name.to_string(), state)),
        }
    }

    pub fn rename_server(&self, from: &str, to: &str) {
        if let Some(entry) = self.state.lock().unwrap().servers.iter_mut().find(|(n, _)| n == from) {
            entry.0 = to.to_string();
        }
    }

    pub fn banner(&self, text: Option<String>) {
        self.state.lock().unwrap().banner = text;
    }

    pub fn context(&self, percent: u64) {
        self.state.lock().unwrap().context = Some(percent);
    }

    pub fn ejected(&self, steps: usize, tokens: usize) {
        let mut s = self.state.lock().unwrap();
        s.ejected.0 += steps;
        s.ejected.1 += tokens;
    }

    pub fn notice(&self, text: &str) {
        let running = self.live && self.running.load(Ordering::SeqCst);
        if running {
            self.pause();
        }
        let line = format!("  {} {}", Color::Rgb(242, 182, 88).bold().paint("⇣"), Color::Rgb(242, 182, 88).paint(text));
        eprintln!("{line}");
        if running {
            self.resume();
        }
    }

    pub fn piece(&self, call: &str, target: Option<&str>, bytes: usize, ok: bool) {
        if matches!(call, "plan" | "done") {
            return;
        }
        let mut s = self.state.lock().unwrap();
        s.blocks.push(Block { kind: kind_of(call), ok, bytes, born: Instant::now() });
        s.phase = format!("$.{call}");
        s.detail = target.map(|t| t.chars().take(60).collect()).unwrap_or_default();
    }

    pub fn start(&self) {
        {
            let mut s = self.state.lock().unwrap();
            let servers = std::mem::take(&mut s.servers);
            let (cache_until, context, ejected) = (s.cache_until, s.context, s.ejected);
            *s = State { started: Some(Instant::now()), phase: "thinking".into(), servers, cache_until, context, ejected, ..Default::default() };
        }
        if !self.live || self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let board = self.clone();
        std::thread::spawn(move || {
            let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"))
                || std::env::var("TERM_PROGRAM").is_ok_and(|v| v != "Apple_Terminal");
            let mut frame = 0usize;
            let started = Instant::now();
            while board.running.load(Ordering::SeqCst) {
                if !board.paused.load(Ordering::SeqCst) {
                    board.draw(frame, started.elapsed().as_secs_f32(), truecolor);
                }
                frame += 1;
                std::thread::sleep(Duration::from_millis(60));
            }
        });
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        self.erase();
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    fn erase(&self) {
        let mut s = self.state.lock().unwrap();
        if self.live && s.drawn > 0 {
            eprint!("{}", erase(s.drawn));
            let _ = std::io::stderr().flush();
            s.drawn = 0;
        }
    }

    pub fn stop(&self) -> usize {
        self.running.store(false, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        self.erase();
        self.state.lock().unwrap().blocks.len()
    }

    pub fn peak(&self) -> Option<crate::sys::Usage> {
        self.state.lock().unwrap().usage
    }

    fn draw(&self, frame: usize, t: f32, truecolor: bool) {
        let mut s = self.state.lock().unwrap();
        if s.sampled.is_none_or(|x| x.elapsed() > Duration::from_millis(1000)) {
            s.sampled = Some(Instant::now());
            drop(s);
            let usage = crate::sys::sample();
            s = self.state.lock().unwrap();
            s.usage = usage;
        }
        let term = terminal_width().max(40);
        let elapsed = s.started.map(|x| x.elapsed()).unwrap_or_default();
        let fit = |text: &str, room: usize| -> String { text.chars().take(room).collect() };
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][frame % 10];
        let done = s.plan.iter().filter(|(_, d)| *d).count();
        let total = s.plan.len();
        let current = s.plan.iter().find(|(_, d)| !*d).map(|(n, _)| n.clone());
        let title = match (&current, total) {
            (Some(step), n) if n > 0 => format!("{} of {n} · {step}", done + 1),
            (None, n) if n > 0 => format!("{n} of {n} · wrapping up"),
            _ => s.phase.clone(),
        };
        let mut lines = Vec::new();
        if let Some(banner) = &s.banner {
            let pulse = ((t * 4.0).sin() + 1.0) / 2.0;
            let arrow = ["⇣", "↓", "⇣", "⤓"][(frame / 4) % 4];
            lines.push(format!("  {} {}", paint(mix(AMBER, AMBER_GLOW, pulse), arrow, truecolor), paint(AMBER, &fit(banner, term.saturating_sub(6)), truecolor)));
            let used = s.context.unwrap_or(0) as f32 / 100.0;
            lines.push(format!("  {} {}  {}", bar_in(Some(used), t, truecolor, AMBER), paint(AMBER, &format!("{:>3}% context", (used * 100.0).round() as u32), truecolor), Style::new().dimmed().paint(clock(elapsed))));
        } else {
            lines.push(format!("  {} {}", paint(NUCLEUS, spinner, truecolor), Style::new().bold().paint(fit(&title, term.saturating_sub(6)))));
            let fraction = (total > 0).then(|| (done as f32 + 0.5 * current.is_some() as u8 as f32) / total as f32);
            let percent = fraction.map(|f| format!("{:>3}%", (f * 100.0).round() as u32)).unwrap_or_default();
            lines.push(format!("  {} {}  {}", bar(fraction, t, truecolor), Style::new().bold().paint(percent), Style::new().dimmed().paint(clock(elapsed))));
        }
        let action = if total > 0 && !s.phase.is_empty() { format!(" {}{}", s.phase, if s.detail.is_empty() { String::new() } else { format!(" {}", s.detail) }) } else if s.detail.is_empty() { String::new() } else { format!(" {}", s.detail) };
        let room = term.saturating_sub(6).min(BAR + 30);
        let blocks = blocks_line(&s.blocks, room.saturating_sub(action.chars().count().min(room / 2)), frame, truecolor);
        lines.push(format!("  {blocks}{}", Style::new().dimmed().paint(fit(&action, room / 2))));
        let mut facts: Vec<String> = vec![format!("{} scripts", s.scripts), format!("{} calls", s.blocks.len())];
        if let Some(hit) = s.hit {
            facts.push(format!("cache {hit}%"));
        }
        if let Some((at, ttl)) = s.cache_until {
            let left = ttl.saturating_sub(at.elapsed());
            facts.push(if left.is_zero() { "cache cold".into() } else { format!("warm {}", clock(left)) });
        }
        let mut warn: Vec<String> = Vec::new();
        match s.context {
            Some(p) if p >= 80 && s.banner.is_none() => warn.push(format!("context {p}% · force-ejects at 95%")),
            Some(p) if p >= 70 && s.banner.is_none() => warn.push(format!("context {p}% · compacts at 80%")),
            Some(p) => facts.push(format!("context {p}%")),
            None => {}
        }
        if s.ejected.0 > 0 {
            warn.push(format!("ejected {} · {}", s.ejected.0, tokens(s.ejected.1)));
        }
        if let Some(u) = s.usage {
            facts.push(format!("{} MB", u.rss_mb));
            facts.push(format!("cpu {:.0}%", u.cpu));
        }
        for (name, state) in &s.servers {
            facts.push(match state {
                ServerState::Ready => format!("{name} ✓"),
                ServerState::Starting => format!("{name} …"),
                ServerState::Failed => format!("{name} ✕"),
            });
        }
        let warn_text = warn.join(" · ");
        let room = term.saturating_sub(4 + if warn_text.is_empty() { 0 } else { warn_text.chars().count() + 3 });
        lines.push(format!(
            "  {}{}",
            if warn_text.is_empty() { String::new() } else { format!("{} {} ", paint(AMBER, &warn_text, truecolor), Style::new().dimmed().paint("·")) },
            Style::new().dimmed().paint(fit(&facts.join(" · "), room))
        ));
        let mut out = erase(s.drawn);
        out.push_str("\x1b[?25l");
        out.push_str(&lines.join("\x1b[0m\x1b[K\n"));
        out.push_str("\x1b[0m\x1b[K\x1b[?25h");
        s.drawn = lines.len();
        eprint!("{out}");
        let _ = std::io::stderr().flush();
    }
}

fn erase(lines: usize) -> String {
    if lines == 0 {
        return String::new();
    }
    let mut s = String::from("\r\x1b[2K");
    for _ in 1..lines {
        s.push_str("\x1b[1A\x1b[2K");
    }
    s
}

fn terminal_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

pub fn dim(text: &str) -> String {
    Style::new().dimmed().paint(text).to_string()
}

pub fn accent(text: &str) -> String {
    Color::Rgb(127, 216, 143).bold().paint(text).to_string()
}

pub fn warn(text: &str) -> String {
    Color::Rgb(255, 138, 128).paint(text).to_string()
}

pub fn amber(text: &str) -> String {
    Color::Rgb(240, 180, 90).bold().paint(text).to_string()
}
