mod agent;
mod catalog;
mod claude_code;
mod claude_native;
mod config;
mod env;
mod events;
mod facts;
mod growth;
mod guard;
mod host;
mod llm;
mod lsp;
mod project;
mod sandbox;
mod servers;
mod sys;
mod ui;

use agent::{Session, TurnReport};
use catalog::Catalog;
use host::{Approval, Approver};
use llm::Client;
use reedline::{
    Completer, CompletionResult, DescriptionMode, EditCommand, Emacs, FileBackedHistory, IdeMenu, KeyCode, KeyModifiers, MenuBuilder, Prompt, PromptEditMode,
    PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion, default_emacs_keybindings,
};
use serde_json::Value;
use std::borrow::Cow;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Args {
    model: Option<String>,
    effort: Option<String>,
    dir: Option<PathBuf>,
    yolo: bool,
    saved_model: bool,
    command: Option<String>,
    rest: Vec<String>,
}

const HELP: &str = "stem: a tiny coding engine that grows with your project

  stem                      open the input
  stem \"fix the failing test\"  run one task and print the answer
  stem models [filter]      models you can use, live from models.dev
  stem stats                cost, tokens, cache and scripts for this project
  stem doctor               show the model, key, project and language servers Stem picks
  stem tools                functions this project grew: uses, failures, age, and the dropped ones with why
  stem js [file]            run one script against $ (from the file or stdin) and print its result
  stem mcp                  serve Stem as one MCP tool over stdio, for other harnesses
  stem login claude         sign in with your Claude plan (through Claude Code) and use it
  stem login claude-native  sign in with your Claude plan directly (no Claude Code needed); alias claude-n

  -m, --model provider/model   pick a model (or set STEM_MODEL)
  -e, --effort level           reasoning effort, when the model has one (or STEM_EFFORT)
  -y, --yolo                   never ask before destructive steps (or STEM_YOLO=1)
  -C, --dir path               work in another folder

In the input, type / to see commands:
  /model opus     pick a model as you type (Tab or arrows, Enter)
  /effort high    change reasoning effort
  /yolo           switch approvals off or on
  /login claude   sign in with your Claude plan and switch to it
  /login claude-native   the same without Claude Code (alias claude-n)
  /cache 1h       Anthropic prompt cache TTL: 5m (default) or 1h
  /new            new conversation     /cost  cost and cache     /exit  quit

Keys go in ~/.stem/.env or the project's .env, for example ANTHROPIC_API_KEY=...
No key? `stem login claude` uses your Claude plan instead.";

fn parse_args() -> Args {
    let mut args = Args { model: None, effort: None, dir: None, yolo: false, saved_model: false, command: None, rest: Vec::new() };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-m" | "--model" => args.model = it.next(),
            "-e" | "--effort" => args.effort = it.next(),
            "-C" | "--dir" => args.dir = it.next().map(PathBuf::from),
            "-y" | "--yolo" => args.yolo = true,
            "-h" | "--help" => args.command = Some("help".into()),
            "-V" | "--version" => args.command = Some("version".into()),
            _ if args.rest.is_empty() && args.command.is_none() && matches!(a.as_str(), "models" | "stats" | "doctor" | "js" | "mcp" | "tools" | "login") => args.command = Some(a),
            _ => args.rest.push(a),
        }
    }
    args.yolo |= std::env::var("STEM_YOLO").is_ok_and(|v| v == "1" || v == "true");
    args
}

fn apply_config(args: &mut Args) {
    let saved = config::Config::load();
    if args.model.is_none() && std::env::var("STEM_MODEL").is_err() {
        args.saved_model = saved.model.is_some();
        args.model = saved.model;
    }
    if args.effort.is_none() && std::env::var("STEM_EFFORT").is_err() {
        args.effort = saved.effort;
    }
    if std::env::var("STEM_YOLO").is_err() {
        args.yolo |= saved.yolo;
    }
}

fn main() {
    let mut args = parse_args();
    let root = args.dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    env::load_dotenv(&root);
    apply_config(&mut args);
    if let Some(e) = &args.effort {
        unsafe { std::env::set_var("STEM_EFFORT", e) };
    }
    let code = match args.command.as_deref() {
        Some("help") => {
            println!("{HELP}");
            0
        }
        Some("version") => {
            println!("stem {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("models") => models(args.rest.first().map(String::as_str)),
        Some("stats") => stats(&root),
        Some("doctor") => doctor(&args, &root),
        Some("js") => js(&args, &root),
        Some("tools") => {
            let growth = growth::Growth::open(&root);
            let rows = growth.table();
            if rows.is_empty() {
                println!("no saved functions yet in {}", root.display());
            } else {
                println!("{}\n{}", rows.join("\n"), ui::dim(&format!("generation {}", growth.generation())));
            }
            0
        }
        Some("mcp") => mcp(&args, &root),
        Some("login") => match login(args.rest.first().map(String::as_str)) {
            Ok(_) => {
                eprintln!("{}", ui::accent("signed in with Claude; Stem now uses your Claude plan (saved)"));
                0
            }
            Err(e) => fail(&e),
        },
        _ => run(&args, &root),
    };
    std::process::exit(code);
}

fn fail(message: &str) -> i32 {
    eprintln!("{}", ui::warn(&format!("stem: {message}")));
    1
}

fn login(provider: Option<&str>) -> Result<&'static str, String> {
    let chosen = match provider {
        Some("claude") => {
            claude_code::login()?;
            claude_code::PROVIDER
        }
        Some(claude_native::PROVIDER | claude_native::ALIAS) => {
            if let Some(email) = claude_native::login()? {
                eprintln!("{}", ui::dim(&format!("signed in as {email}")));
            }
            claude_native::PROVIDER
        }
        Some(other) => return Err(format!("no login for {other}; use `login claude` or `login {}`. Other providers use an API key in ~/.stem/.env", claude_native::PROVIDER)),
        None => return Err(format!("usage: stem login claude | stem login {}", claude_native::PROVIDER)),
    };
    config::Config::update(|c| c.model = Some(chosen.into()));
    Ok(chosen)
}

fn ask_client(catalog: &Catalog, resolved: &catalog::Resolved) -> Option<Client> {
    catalog.cheapest_sibling(resolved).map(|m| {
        let mut r = resolved.clone();
        r.model = m;
        let mut c = Client::new(r);
        let _ = c.set_effort("low");
        c.max_output = c.max_output.min(8000);
        c
    })
}

fn tty_approver() -> Option<Approver> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return None;
    }
    Some(Box::new(|what: &str, reason: &str| {
        let shown: String = what.chars().take(160).collect();
        eprintln!(
            "  {} {}\n    {}\n    {}",
            ui::amber("approve?"),
            shown,
            ui::dim(reason),
            ui::dim("y  yes once    a  yes to everything (yolo)    n  no")
        );
        let _ = std::io::stderr().flush();
        let answer = read_choice();
        eprint!("\x1b[3A\r\x1b[J");
        let label = match answer {
            Approval::Once => ui::accent("allowed"),
            Approval::Always => ui::amber("allowed, yolo on"),
            Approval::Deny => ui::warn("declined"),
        };
        eprintln!("  {label} {}", ui::dim(&shown));
        answer
    }))
}

fn read_choice() -> Approval {
    use crossterm::event::{Event, KeyCode as K, KeyEventKind, KeyModifiers as M, read};
    let raw = crossterm::terminal::enable_raw_mode().is_ok();
    let answer = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                K::Char('y') | K::Char('Y') => break Approval::Once,
                K::Char('a') | K::Char('A') => break Approval::Always,
                K::Char('c') if k.modifiers.contains(M::CONTROL) => break Approval::Deny,
                K::Char('n') | K::Char('N') | K::Esc | K::Enter => break Approval::Deny,
                _ => {}
            },
            Ok(_) => {}
            Err(_) => break Approval::Deny,
        }
    };
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    answer
}

fn open_session(model: Option<&str>, root: &std::path::Path, cancel: Arc<AtomicBool>, yolo: Arc<AtomicBool>) -> Result<Session, String> {
    let catalog = Catalog::load()?;
    let resolved = catalog.resolve(model)?;
    let ask = ask_client(&catalog, &resolved);
    Session::new(Client::new(resolved), ask, root.to_path_buf(), cancel, yolo, tty_approver())
}

fn run(args: &Args, root: &std::path::Path) -> i32 {
    let cancel = Arc::new(AtomicBool::new(false));
    let presses = Arc::new(AtomicU64::new(0));
    {
        let (cancel, presses) = (cancel.clone(), presses.clone());
        let _ = ctrlc::set_handler(move || {
            if presses.fetch_add(1, Ordering::SeqCst) >= 1 {
                eprintln!();
                std::process::exit(130);
            }
            cancel.store(true, Ordering::SeqCst);
        });
    }
    let mut prompt = args.rest.join(" ");
    if prompt.is_empty() && !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut prompt);
    }
    let yolo = Arc::new(AtomicBool::new(args.yolo));
    let opened = open_session(args.model.as_deref(), root, cancel.clone(), yolo.clone()).or_else(|e| {
        if !args.saved_model {
            return Err(e);
        }
        eprintln!("{}", ui::warn(&format!("saved model unavailable ({e}); picking one from your keys")));
        open_session(None, root, cancel, yolo.clone())
    });
    let mut session = match opened {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    if !prompt.trim().is_empty() {
        presses.store(0, Ordering::SeqCst);
        let report = session.turn(prompt.trim());
        print_report(&session, &report);
        return if report.error.is_some() && report.answer.is_empty() { 1 } else { 0 };
    }
    repl(session, root, presses, yolo)
}

fn render_answer(answer: &str) -> String {
    let mut out = Vec::new();
    for line in answer.trim().lines() {
        let styled = [("Asked:", 0), ("Answer:", 1), ("Done:", 2), ("Not done:", 3), ("Checked:", 2)]
            .iter()
            .find(|(label, _)| line.starts_with(label))
            .map(|(label, kind)| {
                let rest = &line[label.len()..];
                let head = match kind {
                    0 => ui::dim(label),
                    1 => nu_ansi_term::Style::new().bold().paint(*label).to_string(),
                    2 => ui::accent(label),
                    _ => ui::amber(label),
                };
                if *kind == 0 { format!("{head}{}", ui::dim(rest)) } else { format!("{head}{rest}") }
            });
        out.push(styled.unwrap_or_else(|| line.to_string()));
    }
    out.join("\n")
}

fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn print_report(session: &Session, r: &TurnReport) {
    if !r.answer.trim().is_empty() {
        let text = if std::io::stdout().is_terminal() { render_answer(&r.answer) } else { r.answer.trim().to_string() };
        println!("{text}");
    }
    if let Some(e) = &r.error {
        eprintln!("{}", ui::warn(&format!("stem: {e}")));
    }
    let u = r.usage;
    eprintln!(
        "{}",
        ui::dim(&format!(
            "{}{} · {} scripts · {} in · cache {}% · {} out · ${:.4} · {:.0} s{}",
            session.client.resolved.model.id,
            session.client.effort().map(|e| format!(" {e}")).unwrap_or_default(),
            r.scripts,
            tokens(u.input + u.cache_write + u.cache_read),
            agent::cache_percent(u),
            tokens(u.output),
            r.cost,
            r.elapsed.as_secs_f64(),
            session.engine.board.peak().map(|p| format!(" · {} MB", p.rss_mb)).unwrap_or_default()
        ))
    );
}

struct StemPrompt {
    yolo: Arc<AtomicBool>,
    cache: Arc<Mutex<Option<(Instant, Duration)>>>,
}

impl Prompt for StemPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Owned(if self.yolo.load(Ordering::SeqCst) { ui::amber("stem yolo") } else { ui::accent("stem") })
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        match *self.cache.lock().unwrap() {
            Some((at, ttl)) => {
                let left = ttl.saturating_sub(at.elapsed());
                Cow::Owned(if left.is_zero() { ui::dim("cache cold") } else { ui::dim(&format!("cache warm {}", ui::clock(left))) })
            }
            None => Cow::Borrowed(""),
        }
    }
    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(" › ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("     · ")
    }
    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Owned(format!("(search: {}) ", search.term))
    }
}

#[derive(Clone)]
struct ModelRow {
    spec: String,
    id: String,
    provider: String,
    description: String,
    has_key: bool,
    gateway: bool,
    date: String,
    words: String,
}

fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut last: Option<char> = None;
    for c in text.to_lowercase().chars() {
        let c = if c.is_alphanumeric() { c } else { ' ' };
        if let Some(l) = last {
            if c != ' ' && l != ' ' && (l.is_ascii_digit() != c.is_ascii_digit()) {
                out.push(' ');
            }
        }
        out.push(c);
        last = Some(c);
    }
    out
}

fn load_rows() -> Vec<ModelRow> {
    let Ok(catalog) = Catalog::load() else { return Vec::new() };
    catalog
        .list(Some(""))
        .into_iter()
        .map(|(spec, m, has_key)| {
            let (provider, id) = spec.split_once('/').map(|(p, i)| (p.to_string(), i.to_string())).unwrap_or_default();
            let context = if m.context >= 1_000_000 { format!("{}M", m.context / 1_000_000) } else { format!("{}k", m.context / 1000) };
            let description = format!(
                "{} · {} · ${}/${} · {} ctx · {}",
                if m.name.is_empty() { id.clone() } else { m.name.clone() },
                if has_key { "key set" } else { "no key" },
                m.cost.input,
                m.cost.output,
                context,
                m.release_date
            );
            let words = normalize(&format!("{spec} {}", m.name));
            ModelRow { gateway: id.contains('/'), spec, id, provider, description, has_key, date: m.release_date, words }
        })
        .collect()
}

fn rank_models(rows: &[ModelRow], query: &str) -> Vec<ModelRow> {
    let terms: Vec<String> = normalize(query).split_whitespace().map(str::to_string).collect();
    let mut hits: Vec<&ModelRow> = rows.iter().filter(|r| terms.iter().all(|t| r.words.contains(t.as_str()))).collect();
    hits.sort_by(|a, b| b.has_key.cmp(&a.has_key).then(a.gateway.cmp(&b.gateway)).then(b.date.cmp(&a.date)).then(a.spec.len().cmp(&b.spec.len())));
    hits.into_iter().take(40).cloned().collect()
}

struct StemCompleter {
    rows: Arc<Mutex<Option<Vec<ModelRow>>>>,
    efforts: Arc<Mutex<Vec<String>>>,
}

const COMMANDS: [(&str, &str); 12] = [
    ("/status", "project, code intelligence, cache, memory"),
    ("/model ", "pick a model: type part of its name"),
    ("/effort ", "reasoning effort for this model"),
    ("/yolo", "approvals off or on"),
    ("/login claude", "sign in with your Claude plan and use it"),
    ("/login claude-native", "sign in with your Claude plan directly, no Claude Code"),
    ("/cache 5m", "Anthropic prompt cache TTL: 5 minutes (default)"),
    ("/cache 1h", "Anthropic prompt cache TTL: 1 hour (higher write price)"),
    ("/new", "start a new conversation"),
    ("/cost", "cost and cache for this session"),
    ("/help", "keys and commands"),
    ("/exit", "quit"),
];

impl Completer for StemCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let typed = &line[..pos.min(line.len())];
        if !typed.starts_with('/') {
            return CompletionResult::fresh(Vec::new());
        }
        let span = Span::new(0, pos);
        if let Some(query) = typed.strip_prefix("/model").map(|q| q.trim_start_matches('s')) {
            if query.is_empty() || query.starts_with(' ') {
                let rows = self.rows.lock().unwrap();
                let Some(rows) = rows.as_ref() else {
                    return CompletionResult::fresh(vec![Suggestion { value: typed.to_string(), description: Some("loading models from models.dev…".into()), span, ..Default::default() }]);
                };
                let suggestions: Vec<Suggestion> = rank_models(rows, query)
                    .into_iter()
                    .map(|r| Suggestion {
                        value: format!("/model {}", r.spec),
                        display_override: Some(format!("{}  {}{}", r.id, ui::dim(&r.provider), if r.has_key { String::new() } else { ui::dim(" ○") })),
                        description: Some(r.description),
                        span,
                        ..Default::default()
                    })
                    .collect();
                return CompletionResult::fresh(suggestions);
            }
        }
        if let Some(query) = typed.strip_prefix("/effort") {
            let query = query.trim();
            let efforts = self.efforts.lock().unwrap();
            let suggestions: Vec<Suggestion> = efforts
                .iter()
                .filter(|e| e.starts_with(query))
                .map(|e| Suggestion { value: format!("/effort {e}"), description: Some("reasoning effort".into()), span, ..Default::default() })
                .collect();
            return CompletionResult::fresh(suggestions);
        }
        CompletionResult::fresh(
            COMMANDS
                .iter()
                .filter(|(c, _)| c.starts_with(typed) || typed.starts_with(c.trim_end()) && c.trim_end() == typed)
                .map(|(c, d)| Suggestion { value: c.to_string(), description: Some(d.to_string()), span, ..Default::default() })
                .collect::<Vec<_>>(),
        )
    }
}

fn repl(mut session: Session, root: &std::path::Path, presses: Arc<AtomicU64>, yolo: Arc<AtomicBool>) -> i32 {
    let rows: Arc<Mutex<Option<Vec<ModelRow>>>> = Arc::default();
    {
        let rows = rows.clone();
        std::thread::spawn(move || {
            let loaded = load_rows();
            *rows.lock().unwrap() = Some(loaded);
        });
    }
    let efforts = Arc::new(Mutex::new(session.client.efforts()));
    let mut keys = default_emacs_keybindings();
    for modifier in [KeyModifiers::ALT, KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
        keys.add_binding(modifier, KeyCode::Enter, ReedlineEvent::Edit(vec![EditCommand::InsertNewline]));
    }
    keys.add_binding(KeyModifiers::CONTROL, KeyCode::Char('j'), ReedlineEvent::Edit(vec![EditCommand::InsertNewline]));
    keys.add_binding(KeyModifiers::NONE, KeyCode::Tab, ReedlineEvent::UntilFound(vec![ReedlineEvent::Menu("completion_menu".into()), ReedlineEvent::MenuNext]));
    keys.add_binding(KeyModifiers::SHIFT, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
    keys.add_binding(KeyModifiers::NONE, KeyCode::Enter, ReedlineEvent::Multiple(vec![ReedlineEvent::Enter, ReedlineEvent::Enter]));
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Char('/'),
        ReedlineEvent::Multiple(vec![ReedlineEvent::Edit(vec![EditCommand::InsertChar('/')]), ReedlineEvent::Menu("completion_menu".into())]),
    );
    let menu = IdeMenu::default()
        .with_name("completion_menu")
        .with_max_completion_height(12)
        .with_description_mode(DescriptionMode::PreferRight)
        .with_max_description_width(64)
        .with_selected_text_style(nu_ansi_term::Style::new().bold().on(nu_ansi_term::Color::Rgb(40, 70, 80)))
        .with_marker("");
    let home = env::stem_home();
    let _ = std::fs::create_dir_all(&home);
    let mut editor = Reedline::create()
        .with_edit_mode(Box::new(Emacs::new(keys)))
        .with_completer(Box::new(StemCompleter { rows: rows.clone(), efforts: efforts.clone() }))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_quick_completions(false)
        .use_kitty_keyboard_enhancement(true)
        .use_bracketed_paste(true);
    if let Ok(history) = FileBackedHistory::with_file(2000, home.join("history")) {
        editor = editor.with_history(Box::new(history));
    }
    let header = |s: &Session, yolo: bool| {
        let r = &s.client.resolved;
        let host = s.engine.host.borrow();
        let spaces: Vec<String> = host
            .project
            .roots
            .iter()
            .take(5)
            .map(|(l, root, n)| {
                let at = host.project.rel_root(root);
                if at == "." { format!("{} {n}", l.label()) } else { format!("{} {n} in {at}", l.label()) }
            })
            .collect();
        let detected = if spaces.is_empty() { "no source files detected".to_string() } else { format!("{} · /status for details", spaces.join(" · ")) };
        ui::dim(&format!(
            "{}{} via {} · {} · {}\n{detected}\n/ for commands · /model to switch · Enter sends · Alt+Enter new line · Ctrl+C stops",
            r.model.id,
            s.client.effort().map(|e| format!(" ({e})")).unwrap_or_default(),
            r.provider_name,
            if yolo { "yolo: no approvals" } else { "asks before destructive steps" },
            root.display()
        ))
    };
    eprintln!("{}", header(&session, yolo.load(Ordering::SeqCst)));
    let cache_clock: Arc<Mutex<Option<(Instant, Duration)>>> = Arc::default();
    let prompt = StemPrompt { yolo: yolo.clone(), cache: cache_clock.clone() };
    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                let (command, rest) = text.split_once(' ').map(|(c, r)| (c, r.trim())).unwrap_or((text, ""));
                match command {
                    "/exit" | "/quit" | "exit" | "quit" => break,
                    "/help" => {
                        eprintln!("{}", ui::dim(HELP));
                        continue;
                    }
                    "/new" | "/clear" => {
                        session.clear();
                        eprintln!("{}", ui::dim("new conversation"));
                        continue;
                    }
                    "/yolo" => {
                        let on = !yolo.load(Ordering::SeqCst);
                        yolo.store(on, Ordering::SeqCst);
                        config::Config::update(|c| c.yolo = on);
                        eprintln!("{}", if on { ui::amber("yolo on: destructive steps run without asking") } else { ui::accent("approvals on: Stem asks before destructive steps") });
                        continue;
                    }
                    "/status" => {
                        eprintln!("{}", status(&session, &yolo));
                        continue;
                    }
                    "/cost" => {
                        let u = session.usage;
                        eprintln!(
                            "{}",
                            ui::dim(&format!(
                                "session ${:.4} · {} in · cache {}% · {} out · log {}",
                                session.cost,
                                tokens(u.input + u.cache_read + u.cache_write),
                                agent::cache_percent(u),
                                tokens(u.output),
                                session.engine.log.path.display()
                            ))
                        );
                        continue;
                    }
                    "/effort" => {
                        match session.client.set_effort(rest) {
                            Ok(e) => {
                                config::Config::update(|c| c.effort = Some(e.clone()));
                                eprintln!("{}", ui::accent(&format!("effort {e} (saved)")));
                            }
                            Err(e) => eprintln!("{}", ui::warn(&e)),
                        }
                        continue;
                    }
                    "/cache" | "/ttl" => {
                        if rest.is_empty() {
                            eprintln!("{}", ui::dim(&format!("cache TTL {} · /cache 5m or /cache 1h", session.client.cache_ttl_choice())));
                            continue;
                        }
                        match session.client.set_cache_ttl(rest) {
                            Ok(ttl) => {
                                config::Config::update(|c| c.cache_ttl = Some(ttl.into()));
                                eprintln!("{}", ui::accent(&format!("cache TTL {ttl} (saved)")));
                            }
                            Err(e) => eprintln!("{}", ui::warn(&e)),
                        }
                        continue;
                    }
                    "/login" => {
                        let provider = match login(Some(rest).filter(|r| !r.is_empty())) {
                            Ok(p) => p,
                            Err(e) => {
                                eprintln!("{}", ui::warn(&e));
                                continue;
                            }
                        };
                        match open_session(Some(provider), root, session.cancel.clone(), yolo.clone()) {
                            Ok(s) => {
                                session = s;
                                *efforts.lock().unwrap() = session.client.efforts();
                                eprintln!("{}", ui::accent(&format!("signed in; now {} on your Claude plan (saved; new conversation)", session.client.resolved.model.id)));
                            }
                            Err(e) => eprintln!("{}", ui::warn(&e)),
                        }
                        continue;
                    }
                    "/model" | "/models" => {
                        let spec = if rest.contains('/') && !rest.contains(' ') {
                            Some(rest.to_string())
                        } else {
                            rows.lock().unwrap().as_ref().and_then(|r| rank_models(r, rest).into_iter().find(|m| m.has_key)).map(|m| m.spec)
                        };
                        let Some(spec) = spec else {
                            eprintln!("{}", ui::warn(&format!("no model with a key set matches \"{rest}\"; type /model and pick from the list")));
                            continue;
                        };
                        let effort = session.client.effort();
                        match open_session(Some(&spec), root, session.cancel.clone(), yolo.clone()) {
                            Ok(s) => {
                                session = s;
                                if let Some(e) = effort {
                                    let _ = session.client.set_effort(&e);
                                }
                                *efforts.lock().unwrap() = session.client.efforts();
                                let saved = format!("{}/{}", session.client.resolved.provider_id, session.client.resolved.model.id);
                                config::Config::update(|c| c.model = Some(saved));
                                eprintln!("{}", ui::accent(&format!("now {} (saved; new conversation)", spec)));
                            }
                            Err(e) => eprintln!("{}", ui::warn(&e)),
                        }
                        continue;
                    }
                    c if c.starts_with('/') => {
                        eprintln!("{}", ui::warn(&format!("unknown command {c}; type / to see them")));
                        continue;
                    }
                    _ => {}
                }
                presses.store(0, Ordering::SeqCst);
                let report = session.turn(text);
                print_report(&session, &report);
                *cache_clock.lock().unwrap() = session.cache_at.zip(session.client.cache_ttl());
                println!();
            }
            Ok(Signal::CtrlD) => break,
            Ok(Signal::CtrlC) => continue,
            Ok(_) => continue,
            Err(e) => return fail(&format!("input: {e}")),
        }
    }
    let u = session.usage;
    eprintln!(
        "{}",
        ui::dim(&format!("session ${:.4} · cache {}% · log {}", session.cost, agent::cache_percent(u), session.engine.log.path.display()))
    );
    0
}

fn status(session: &Session, yolo: &Arc<AtomicBool>) -> String {
    let host = session.engine.host.borrow();
    let p = &host.project;
    let r = &session.client.resolved;
    let mut lines = vec![
        format!("{} {}", ui::accent("project"), p.root.display()),
        format!("  runner {} · branch {}", if p.runner.is_empty() { "none" } else { &p.runner }, p.branch.clone().unwrap_or("none".into())),
    ];
    if !p.commands.is_empty() {
        lines.push(format!("  {}", p.commands.iter().map(|(k, c)| format!("{k}: {c}")).collect::<Vec<_>>().join(" · ")));
    }
    lines.push(ui::accent("code intelligence"));
    for item in host.intelligence() {
        let state = &item["intelligence"];
        let detail = if let Some(name) = state["server"].as_str() {
            format!("{} ✓ {} · ready in {} ms", name, ui::dim(state["source"].as_str().unwrap_or("")), state["ready_ms"])
        } else if let Some(e) = state["error"].as_str() {
            ui::warn(&format!("✕ {}", e.chars().take(90).collect::<String>()))
        } else {
            ui::dim("starts on first use")
        };
        lines.push(format!("  {:<22} {:<24} {:>6} files  {detail}", item["language"].as_str().unwrap_or(""), item["root"].as_str().unwrap_or(""), item["files"]));
    }
    lines.push(ui::accent("model"));
    lines.push(format!("  {}/{}{} · {} · {}", r.provider_id, r.model.id, session.client.effort().map(|e| format!(" ({e})")).unwrap_or_default(), if r.gateway { "gateway" } else { "direct" }, if yolo.load(Ordering::SeqCst) { "yolo" } else { "asks before destructive steps" }));
    let u = session.usage;
    let cache = match (session.cache_at, session.client.cache_ttl()) {
        (Some(at), Some(ttl)) => {
            let left = ttl.saturating_sub(at.elapsed());
            if left.is_zero() { format!("cold (TTL {} passed)", ui::clock(ttl)) } else { format!("warm for {} of {}", ui::clock(left), ui::clock(ttl)) }
        }
        (None, Some(ttl)) => format!("not written yet · TTL {}", ui::clock(ttl)),
        (_, None) => "provider does not report a cache TTL".into(),
    };
    lines.push(format!("  cache {}% hit · {cache} · session ${:.4}", agent::cache_percent(u), session.cost));
    if let Some(s) = sys::sample() {
        lines.push(format!("  memory {} MB · cpu {:.0}% · {} processes (Stem and its language servers)", s.rss_mb, s.cpu, s.processes));
    }
    lines.join("\n")
}

fn models(filter: Option<&str>) -> i32 {
    let catalog = match Catalog::load() {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let rows = catalog.list(filter);
    if rows.is_empty() {
        return fail(if filter.is_some() {
            "no models match"
        } else {
            "no provider key found; pass a filter to search every provider, e.g. `stem models claude`"
        });
    }
    for (id, m, has_key) in rows.iter().take(80) {
        println!(
            "{} {:<52} {:>8} ctx  ${:>6.2} / ${:<6.2} {}",
            if *has_key { ui::accent("●") } else { ui::dim("○") },
            id,
            if m.context >= 1000 {
                format!("{}k", m.context / 1000)
            } else {
                m.context.to_string()
            },
            m.cost.input,
            m.cost.output,
            ui::dim(&m.release_date)
        );
    }
    if rows.len() > 80 {
        eprintln!("{}", ui::dim(&format!("… {} more; add a filter", rows.len() - 80)));
    }
    eprintln!("{}", ui::dim(&format!("● key set · price per million tokens in / out · {}", catalog.source)));
    0
}

fn doctor(args: &Args, root: &std::path::Path) -> i32 {
    let started = Instant::now();
    let catalog = match Catalog::load() {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    println!("catalog   {} ({} ms)", catalog.source, started.elapsed().as_millis());
    match catalog.resolve(args.model.as_deref()) {
        Ok(r) => {
            let masked = format!("{}…{}", &r.key[..r.key.len().min(7)], &r.key[r.key.len().saturating_sub(4)..]);
            println!("model     {}/{} ({})", r.provider_id, r.model.id, r.model.name);
            println!("why       {}", r.why);
            println!("api       {} {:?}{}", r.base_url, r.protocol, if r.gateway { ", gateway" } else { "" });
            println!("key       {} from {}", masked, r.key_source);
            println!(
                "limits    {} context, {} output, reasoning {:?}",
                r.model.context, r.model.output, r.model.reasoning
            );
            println!(
                "price     ${} in, ${} out, ${} cache read per million tokens",
                r.model.cost.input, r.model.cost.output, r.model.cost.cache_read
            );
            if let Some(small) = catalog.cheapest_sibling(&r) {
                println!("$.ask     {}", small.id);
            }
        }
        Err(e) => println!("model     {}", ui::warn(&e)),
    }
    let project = project::Project::detect(root);
    println!(
        "project   {} · runner {} · branch {}",
        root.display(),
        if project.runner.is_empty() { "none" } else { &project.runner },
        project.branch.clone().unwrap_or("none".into())
    );
    for (k, c) in &project.commands {
        println!("          {k}: {c}");
    }
    for (lang, files) in project.languages.iter().take(6) {
        let t = Instant::now();
        let line = match servers::find(*lang, root).and_then(|spec| lsp::Server::start(spec, root)) {
            Ok(mut server) => {
                server.wait_ready(Duration::from_secs(60));
                format!("{} · {} · ready in {} ms", server.name, server.source, t.elapsed().as_millis())
            }
            Err(e) => ui::warn(&e),
        };
        println!(
            "{:<9} {} ({} files): {line}",
            if lang == &project.languages[0].0 { "servers" } else { "" },
            lang.label(),
            files
        );
    }
    0
}

fn stats(root: &std::path::Path) -> i32 {
    let dir = events::sessions_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return fail(&format!("no sessions yet in {}", dir.display()));
    };
    let (mut sessions, mut turns, mut scripts, mut failed, mut cost) = (0, 0, 0, 0, 0.0);
    let (mut input, mut output, mut cached, mut touched, mut returned) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut calls: std::collections::BTreeMap<String, u64> = Default::default();
    for entry in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        sessions += 1;
        for line in text.lines() {
            let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
            match e["k"].as_str() {
                Some("turn") => {
                    turns += 1;
                    cost += e["cost"].as_f64().unwrap_or(0.0);
                    input += e["in"].as_u64().unwrap_or(0) + e["cache_write"].as_u64().unwrap_or(0);
                    cached += e["cache_read"].as_u64().unwrap_or(0);
                    output += e["out"].as_u64().unwrap_or(0);
                }
                Some("script") => {
                    scripts += 1;
                    if e["ok"] == false {
                        failed += 1;
                    }
                    touched += e["touched_bytes"].as_u64().unwrap_or(0);
                    returned += e["return_bytes"].as_u64().unwrap_or(0);
                    for (name, n) in e["calls"].as_object().into_iter().flatten() {
                        *calls.entry(name.clone()).or_default() += n.as_u64().unwrap_or(0);
                    }
                }
                _ => {}
            }
        }
    }
    println!("sessions  {sessions} · turns {turns} · cost ${cost:.4}");
    println!("tokens    {input} in · {cached} cached · {output} out");
    println!("scripts   {scripts} ({failed} failed)");
    if touched > 0 {
        println!(
            "context   scripts touched {touched} bytes and returned {returned} ({:.0}% kept out of the model)",
            100.0 - returned as f64 * 100.0 / touched.max(1) as f64
        );
    }
    let mut ranked: Vec<(String, u64)> = calls.into_iter().collect();
    ranked.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("calls     {}", ranked.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" · "));
    0
}

fn engine_for(root: &std::path::Path, cancel: Arc<AtomicBool>, board: ui::Board, yolo: bool) -> Result<agent::Engine, String> {
    let ask = Catalog::load().ok().and_then(|catalog| {
        let resolved = catalog.resolve(None).ok()?;
        ask_client(&catalog, &resolved)
    });
    agent::Engine::new(root.to_path_buf(), cancel, ask, board, Arc::new(AtomicBool::new(yolo)), None)
}

fn js(args: &Args, root: &std::path::Path) -> i32 {
    let mut code = String::new();
    match args.rest.first().map(String::as_str) {
        Some("-") | None => {
            let _ = std::io::stdin().read_to_string(&mut code);
        }
        Some(file) => match std::fs::read_to_string(file) {
            Ok(c) => code = c,
            Err(e) => return fail(&format!("{file}: {e}")),
        },
    }
    let mut engine = match engine_for(root, Arc::new(AtomicBool::new(false)), ui::Board::quiet(), args.yolo) {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    let (text, is_error) = engine.run_script(&code);
    println!("{text}");
    if is_error { 1 } else { 0 }
}

fn mcp(args: &Args, root: &std::path::Path) -> i32 {
    use std::io::{BufRead, Write};
    let cancel = Arc::new(AtomicBool::new(false));
    let mut engine = match engine_for(root, cancel, ui::Board::quiet(), args.yolo) {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    let description = format!(
        "Stem: run one JavaScript block against the project at {}. The block is the body of an async function; return a small result. Only the return value and console.log lines come back.\n\n{}",
        root.display(),
        agent::SCRIPTS_DOC
    );
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => serde_json::json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "stem", "version": env!("CARGO_PKG_VERSION")}
            }),
            "ping" => serde_json::json!({}),
            "tools/list" => serde_json::json!({"tools": [{
                "name": "js",
                "description": description,
                "inputSchema": {"type": "object", "properties": {"code": {"type": "string"}}, "required": ["code"]}
            }]}),
            "tools/call" => {
                let code = msg["params"]["arguments"]["code"].as_str().unwrap_or("");
                let (text, is_error) = engine.run_script(code);
                serde_json::json!({"content": [{"type": "text", "text": text}], "isError": is_error})
            }
            other => {
                let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {other}")}});
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
                continue;
            }
        };
        let _ = writeln!(out, "{}", serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let _ = out.flush();
    }
    0
}
