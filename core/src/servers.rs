use crate::lsp::ServerSpec;
use serde_json::Value;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

const RECHECK_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    TypeScript,
    Python,
    Rust,
    Go,
    C,
    Lua,
    Zig,
}

impl Lang {
    pub fn of(path: &Path) -> Option<Lang> {
        Some(match path.extension()?.to_str()? {
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Lang::TypeScript,
            "py" | "pyi" => Lang::Python,
            "rs" => Lang::Rust,
            "go" => Lang::Go,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => Lang::C,
            "lua" => Lang::Lua,
            "zig" => Lang::Zig,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::TypeScript => "TypeScript/JavaScript",
            Lang::Python => "Python",
            Lang::Rust => "Rust",
            Lang::Go => "Go",
            Lang::C => "C/C++",
            Lang::Lua => "Lua",
            Lang::Zig => "Zig",
        }
    }
}

pub fn root_for(lang: Lang, file: &Path, project: &Path) -> PathBuf {
    let has = |dir: &Path, names: &[&str]| names.iter().any(|n| dir.join(n).exists());
    let mut dirs = Vec::new();
    let mut dir = file.parent();
    while let Some(d) = dir {
        if !d.starts_with(project) {
            break;
        }
        dirs.push(d.to_path_buf());
        if d == project {
            break;
        }
        dir = d.parent();
    }
    let nearest = |names: &[&str]| dirs.iter().find(|d| has(d, names)).cloned();
    let topmost = |names: &[&str]| dirs.iter().rev().find(|d| has(d, names)).cloned();
    let found = match lang {
        Lang::TypeScript => topmost(&["package.json", "tsconfig.json", "jsconfig.json"]),
        Lang::Rust => topmost(&["Cargo.toml"]),
        Lang::Go => topmost(&["go.work"]).or_else(|| nearest(&["go.mod"])),
        Lang::Python => nearest(&["pyproject.toml", "setup.py", "setup.cfg"]),
        Lang::C => nearest(&["compile_commands.json"]).or_else(|| topmost(&["CMakeLists.txt", "meson.build", "Makefile"])),
        Lang::Lua => nearest(&[".luarc.json", ".luarc.jsonc"]),
        Lang::Zig => nearest(&["build.zig"]),
    };
    found.unwrap_or_else(|| project.to_path_buf())
}

fn servers_dir() -> PathBuf {
    crate::env::stem_home().join("servers")
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe(name))).find(|p| p.is_file())
}

fn runs(command: &Path, args: &[&str]) -> bool {
    crate::sys::command(command)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into()
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut response = http().get(url).call().map_err(|e| format!("download {url}: {e}"))?;
    if response.status() != 200 {
        return Err(format!("download {url}: HTTP {}", response.status()));
    }
    response
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("download {url}: {e}"))
}

fn get_json(url: &str) -> Result<Value, String> {
    let bytes = download(url)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{url}: {e}"))
}

fn fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < RECHECK_AFTER)
}

fn mark_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

fn node_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("windows", "x86_64") => "win32-x64",
        ("windows", "aarch64") => "win32-arm64",
        _ => return None,
    })
}

fn rust_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return None,
    })
}

fn wheel_tags() -> &'static [&'static str] {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => &["macosx", "arm64"],
        ("macos", "x86_64") => &["macosx", "x86_64"],
        ("linux", "x86_64") => &["manylinux", "x86_64"],
        ("linux", "aarch64") => &["manylinux", "aarch64"],
        ("windows", "x86_64") => &["win_amd64"],
        ("windows", "aarch64") => &["win_arm64"],
        _ => &["unsupported-platform"],
    }
}

pub fn find(lang: Lang, root: &Path) -> Result<ServerSpec, String> {
    match lang {
        Lang::TypeScript => typescript(root),
        Lang::Python => python(),
        Lang::Rust => rust(),
        Lang::Go => from_path("gopls", &[], "install Go and gopls"),
        Lang::C => from_path("clangd", &[], "install clangd"),
        Lang::Lua => from_path("lua-language-server", &[], "install lua-language-server"),
        Lang::Zig => from_path("zls", &[], "install zls"),
    }
}

fn from_path(name: &str, args: &[&str], hint: &str) -> Result<ServerSpec, String> {
    let command = on_path(name).ok_or_else(|| format!("no {name} found; {hint}"))?;
    Ok(ServerSpec {
        name: name.into(),
        command,
        args: args.iter().map(|s| s.to_string()).collect(),
        source: "found on PATH".into(),
    })
}

fn tsgo_spec(command: PathBuf, source: String) -> ServerSpec {
    ServerSpec {
        name: "tsgo".into(),
        command,
        args: vec!["--lsp".into(), "--stdio".into()],
        source,
    }
}

fn typescript(root: &Path) -> Result<ServerSpec, String> {
    let platform = node_platform().ok_or("no TypeScript 7 build for this platform")?;
    let mut dir = Some(root);
    while let Some(d) = dir {
        let ts = d.join("node_modules/typescript");
        let major = std::fs::read_to_string(ts.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["version"].as_str().and_then(|s| s.split('.').next()).and_then(|m| m.parse::<u32>().ok()));
        if major.is_some_and(|m| m >= 7) {
            let real = std::fs::canonicalize(&ts).unwrap_or(ts.clone());
            for candidate in [
                real.join(format!("../@typescript/typescript-{platform}/lib")).join(exe("tsc")),
                d.join(format!("node_modules/@typescript/typescript-{platform}/lib")).join(exe("tsc")),
            ] {
                if candidate.is_file() {
                    return Ok(tsgo_spec(candidate, format!("the project's own TypeScript {}", major.unwrap_or(7))));
                }
            }
        }
        let preview = d.join(format!("node_modules/@typescript/native-preview-{platform}/lib")).join(exe("tsgo"));
        if preview.is_file() {
            return Ok(tsgo_spec(preview, "the project's own @typescript/native-preview".into()));
        }
        dir = d.parent();
    }
    let home = servers_dir().join("tsgo");
    let binary = home.join("package/lib").join(exe("tsc"));
    if binary.is_file() && fresh(&home.join("stamp")) {
        return Ok(tsgo_spec(binary, "Stem's copy of TypeScript 7".into()));
    }
    let meta = get_json(&format!("https://registry.npmjs.org/@typescript/typescript-{platform}/latest"));
    match meta {
        Ok(meta) => {
            let tarball = meta["dist"]["tarball"].as_str().ok_or("npm gave no tarball for TypeScript 7")?;
            let bytes = download(tarball)?;
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)))
                .unpack(&home)
                .map_err(|e| format!("unpacking TypeScript 7: {e}"))?;
            mark_executable(&binary)?;
            std::fs::write(home.join("stamp"), meta["version"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
            Ok(tsgo_spec(binary, format!("downloaded TypeScript {}", meta["version"].as_str().unwrap_or("7"))))
        }
        Err(e) if binary.is_file() => Ok(tsgo_spec(binary, format!("Stem's copy of TypeScript 7 (update check failed: {e})"))),
        Err(e) => Err(e),
    }
}

fn rust() -> Result<ServerSpec, String> {
    let spec = |command: PathBuf, source: String| ServerSpec {
        name: "rust-analyzer".into(),
        command,
        args: vec![],
        source,
    };
    if let Some(found) = on_path("rust-analyzer")
        && runs(&found, &["--version"]) {
            return Ok(spec(found, "found on PATH".into()));
        }
    let home = servers_dir().join("rust-analyzer");
    let binary = home.join(exe("rust-analyzer"));
    if binary.is_file() && fresh(&home.join("stamp")) {
        return Ok(spec(binary, "Stem's copy".into()));
    }
    let triple = rust_triple().ok_or("no rust-analyzer build for this platform")?;
    let ext = if cfg!(windows) { "zip" } else { "gz" };
    let fetched = download(&format!(
        "https://github.com/rust-lang/rust-analyzer/releases/latest/download/rust-analyzer-{triple}.{ext}"
    ));
    match fetched {
        Ok(bytes) => {
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            if cfg!(windows) {
                let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
                let mut file = zip.by_name("rust-analyzer.exe").map_err(|e| e.to_string())?;
                file.read_to_end(&mut out).map_err(|e| e.to_string())?;
            } else {
                flate2::read::GzDecoder::new(Cursor::new(bytes))
                    .read_to_end(&mut out)
                    .map_err(|e| format!("unpacking rust-analyzer: {e}"))?;
            }
            std::fs::write(&binary, out).map_err(|e| e.to_string())?;
            mark_executable(&binary)?;
            std::fs::write(home.join("stamp"), "").map_err(|e| e.to_string())?;
            Ok(spec(binary, "downloaded from rust-analyzer releases".into()))
        }
        Err(e) if binary.is_file() => Ok(spec(binary, format!("Stem's copy (update check failed: {e})"))),
        Err(e) => Err(e),
    }
}

fn python() -> Result<ServerSpec, String> {
    let spec = |command: PathBuf, source: String| ServerSpec {
        name: "ty".into(),
        command,
        args: vec!["server".into()],
        source,
    };
    if let Some(found) = on_path("ty") {
        return Ok(spec(found, "found on PATH".into()));
    }
    let home = servers_dir().join("ty");
    let binary = home.join(exe("ty"));
    if binary.is_file() && fresh(&home.join("stamp")) {
        return Ok(spec(binary, "Stem's copy".into()));
    }
    let fetched = (|| -> Result<(Vec<u8>, String), String> {
        let meta = get_json("https://pypi.org/pypi/ty/json")?;
        let tags = wheel_tags();
        let wheel = meta["urls"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|u| {
                u["filename"]
                    .as_str()
                    .is_some_and(|f| f.ends_with(".whl") && tags.iter().all(|t| f.contains(t)) && !f.contains("musllinux"))
            })
            .ok_or("PyPI has no ty wheel for this platform")?;
        Ok((
            download(wheel["url"].as_str().unwrap_or(""))?,
            meta["info"]["version"].as_str().unwrap_or("").to_string(),
        ))
    })();
    match fetched {
        Ok((bytes, version)) => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            let name = (0..zip.len())
                .filter_map(|i| zip.by_index(i).ok().and_then(|f| f.name().ok().map(|n| n.to_string())))
                .find(|n| n.contains(".data/scripts/") && n.ends_with(&exe("ty")))
                .ok_or("the ty wheel has no ty binary")?;
            let mut out = Vec::new();
            zip.by_name(&name)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            std::fs::write(&binary, out).map_err(|e| e.to_string())?;
            mark_executable(&binary)?;
            std::fs::write(home.join("stamp"), &version).map_err(|e| e.to_string())?;
            Ok(spec(binary, format!("downloaded ty {version} from PyPI")))
        }
        Err(e) if binary.is_file() => Ok(spec(binary, format!("Stem's copy (update check failed: {e})"))),
        Err(e) => Err(e),
    }
}
