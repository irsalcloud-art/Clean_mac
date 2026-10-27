//! Finds npm's cache directory the way npm does, without running npm
//! (running it writes logs and may contact the registry).
//!
//! npm reads its settings from these levels, highest first: command line,
//! environment (`npm_config_*`), project `.npmrc`, user npmrc, global npmrc,
//! builtin npmrc (shipped inside the npm package), defaults. See
//! `@npmcli/config` `Config::load`. MacClean follows that order for `cache`,
//! except that a project `.npmrc` is only reported: it depends on the
//! directory npm is started in, and the user-level cache is still the one
//! npm uses everywhere else.

use std::io;
use std::path::{Path, PathBuf};

/// Where the npm cache directory (the parent of `_cacache`) was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmCacheLocation {
    pub dir: PathBuf,
    /// Which setting decided it, for the plan.
    pub source: String,
    /// Settings that were noticed but not used.
    pub notes: Vec<String>,
}

/// Parses the value of `key` from npmrc-style text. The last assignment
/// wins, as in npm's ini parser.
pub fn npmrc_value(text: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((k, value)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case(key)
        {
            let v = value.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            found = Some(v.to_string());
        }
    }
    found
}

/// Turns a configured path value into an absolute path, or explains why
/// it is too ambiguous to act on.
pub fn config_path(key: &str, value: &str, home: &Path) -> Result<PathBuf, String> {
    if value.contains("${") {
        return Err(format!(
            "npm setting `{key}={value}` uses variable expansion, which MacClean does not evaluate"
        ));
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(format!(
            "npm setting `{key}={value}` is not an absolute path"
        ));
    }
    Ok(path)
}

/// One npmrc file that was read (or found missing).
struct Npmrc {
    path: PathBuf,
    label: &'static str,
    text: Option<String>,
}

impl Npmrc {
    fn read(path: PathBuf, label: &'static str) -> Result<Npmrc, String> {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {label} {}: {e}", path.display())),
        };
        Ok(Npmrc { path, label, text })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.text.as_deref().and_then(|t| npmrc_value(t, key))
    }

    fn describe(&self, key: &str) -> String {
        format!("{key}= in {} {}", self.label, self.path.display())
    }
}

/// A setting from the environment: `npm_config_<key>` in either case.
fn env_setting(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<(String, String)> {
    let lower = format!("npm_config_{key}");
    let upper = lower.to_uppercase();
    [lower, upper]
        .into_iter()
        .find_map(|var| env(&var).filter(|v| !v.is_empty()).map(|v| (var, v)))
}

/// The first executable called `name` on `PATH`, with symlinks resolved.
fn find_on_path(env: &impl Fn(&str) -> Option<String>, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env("PATH")?;
    path.split(':')
        .filter(|d| !d.is_empty() && Path::new(d).is_absolute())
        .map(|d| Path::new(d).join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|p| p.canonicalize().ok())
}

/// npm's own install directory, from the `npm` on `PATH`
/// (`…/lib/node_modules/npm/bin/npm-cli.js` → `…/lib/node_modules/npm`).
fn npm_package_dir(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let cli = find_on_path(env, "npm")?;
    if cli.file_name()? != "npm-cli.js" {
        return None;
    }
    let bin = cli.parent()?;
    (bin.file_name()? == "bin").then(|| bin.parent().map(Path::to_path_buf))?
}

/// npm's default global prefix: `$PREFIX`, else two levels above the real
/// path of `node` (`/usr/local/bin/node` → `/usr/local`), under `$DESTDIR`.
fn default_global_prefix(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let prefix = match env("PREFIX").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => find_on_path(env, "node")?.parent()?.parent()?.to_path_buf(),
    };
    match env("DESTDIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(Path::new(&d).join(prefix.strip_prefix("/").unwrap_or(&prefix))),
        None => Some(prefix),
    }
}

/// The project npmrc npm would read when started in `cwd`: `.npmrc` in the
/// nearest directory with a `package.json` or `node_modules`, else in `cwd`.
fn project_npmrc(cwd: &Path, home: &Path) -> Option<PathBuf> {
    let root = cwd
        .ancestors()
        .find(|d| d.join("package.json").exists() || d.join("node_modules").exists())
        .unwrap_or(cwd);
    // npm skips a project npmrc that is the user npmrc.
    (root != home).then(|| root.join(".npmrc"))
}

/// Resolves npm's cache directory for the current user. `env` reads
/// environment variables (including `PATH`, used to find `node` and `npm`);
/// `cwd` is where a project `.npmrc` would be looked for.
pub fn detect_npm_cache_dir(
    home: &Path,
    cwd: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<NpmCacheLocation, String> {
    let mut notes = Vec::new();
    let found = |dir: PathBuf, source: String, notes: Vec<String>| {
        Ok(NpmCacheLocation { dir, source, notes })
    };

    if let Some(cwd) = cwd
        && let Some(path) = project_npmrc(cwd, home)
        && let Ok(project) = Npmrc::read(path, "project npmrc")
        && let Some(v) = project.get("cache")
    {
        notes.push(format!(
            "{} sets cache={v}; npm uses that cache only inside that project, and MacClean \
             does not clean it",
            project.path.display()
        ));
    }

    if let Some((var, v)) = env_setting(&env, "cache") {
        return config_path("cache", &v, home).and_then(|p| found(p, format!("${var}"), notes));
    }

    let builtin = match npm_package_dir(&env) {
        Some(dir) => Some(Npmrc::read(dir.join("npmrc"), "builtin npmrc")?),
        None => None,
    };
    let builtin_get = |key: &str| builtin.as_ref().and_then(|b| b.get(key));

    let user_path = match env_setting(&env, "userconfig") {
        Some((_, v)) => config_path("userconfig", &v, home)?,
        None => match builtin_get("userconfig") {
            Some(v) => config_path("userconfig", &v, home)?,
            None => home.join(".npmrc"),
        },
    };
    let user = Npmrc::read(user_path, "user npmrc")?;
    if let Some(v) = user.get("cache") {
        return config_path("cache", &v, home)
            .and_then(|p| found(p, user.describe("cache"), notes));
    }

    // Lower levels follow the same order for the settings that locate the
    // global npmrc.
    let layered = |key: &str| {
        env_setting(&env, key)
            .map(|(_, v)| v)
            .or_else(|| user.get(key))
            .or_else(|| builtin_get(key))
    };
    let global_path = match layered("globalconfig") {
        Some(v) => Some(config_path("globalconfig", &v, home)?),
        None => match layered("prefix") {
            Some(v) => Some(config_path("prefix", &v, home)?.join("etc/npmrc")),
            None => default_global_prefix(&env).map(|p| p.join("etc/npmrc")),
        },
    };
    match global_path {
        Some(path) => {
            let global = Npmrc::read(path, "global npmrc")?;
            if let Some(v) = global.get("cache") {
                return config_path("cache", &v, home)
                    .and_then(|p| found(p, global.describe("cache"), notes));
            }
        }
        None => notes
            .push("node was not found on PATH, so the global npmrc was not checked".to_string()),
    }

    if let Some(b) = &builtin
        && let Some(v) = b.get("cache")
    {
        return config_path("cache", &v, home).and_then(|p| found(p, b.describe("cache"), notes));
    }

    found(home.join(".npm"), "npm default ~/.npm".to_string(), notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    #[test]
    fn npmrc_parsing() {
        assert_eq!(
            npmrc_value("registry=x\ncache=/a/b\n", "cache"),
            Some("/a/b".into())
        );
        assert_eq!(
            npmrc_value("cache = \"/a b\"\n; cache=/no\n", "cache"),
            Some("/a b".into())
        );
        assert_eq!(
            npmrc_value("cache=/one\nCACHE=/two", "cache"),
            Some("/two".into())
        );
        assert_eq!(npmrc_value("# cache=/x\ncache-min=10", "cache"), None);
        assert_eq!(
            npmrc_value("prefix = /opt/homebrew\n", "prefix"),
            Some("/opt/homebrew".into())
        );
    }

    #[test]
    fn config_paths() {
        let home = Path::new("/Users/ann");
        assert_eq!(config_path("cache", "~/c", home).unwrap(), home.join("c"));
        assert_eq!(config_path("cache", "~", home).unwrap(), home);
        assert!(config_path("cache", "relative/npm", home).is_err());
        assert!(config_path("cache", "${HOME}/x", home).is_err());
    }

    /// A fake machine: a home directory, and a Node.js install whose `bin`
    /// goes on PATH, laid out like Homebrew's.
    struct Machine {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        prefix: PathBuf,
        vars: HashMap<String, String>,
    }

    impl Machine {
        fn new() -> Machine {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().canonicalize().unwrap();
            let home = base.join("home");
            let prefix = base.join("node-prefix");
            let npm = prefix.join("lib/node_modules/npm");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(prefix.join("bin")).unwrap();
            fs::create_dir_all(npm.join("bin")).unwrap();
            for exe in [prefix.join("bin/node"), npm.join("bin/npm-cli.js")] {
                fs::write(&exe, "").unwrap();
                fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
            }
            symlink(npm.join("bin/npm-cli.js"), prefix.join("bin/npm")).unwrap();
            let mut vars = HashMap::new();
            vars.insert(
                "PATH".to_string(),
                format!("/nonexistent:{}", prefix.join("bin").display()),
            );
            Machine {
                _tmp: tmp,
                home,
                prefix,
                vars,
            }
        }

        fn write(&self, path: &Path, text: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn builtin(&self) -> PathBuf {
            self.prefix.join("lib/node_modules/npm/npmrc")
        }

        fn detect(&self) -> Result<NpmCacheLocation, String> {
            self.detect_in(None)
        }

        fn detect_in(&self, cwd: Option<&Path>) -> Result<NpmCacheLocation, String> {
            detect_npm_cache_dir(&self.home, cwd, |k| self.vars.get(k).cloned())
        }
    }

    #[test]
    fn default_is_home_npm() {
        let m = Machine::new();
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, m.home.join(".npm"));
        assert_eq!(loc.source, "npm default ~/.npm");
        assert!(loc.notes.is_empty());
    }

    #[test]
    fn precedence_env_user_global_builtin() {
        let mut m = Machine::new();
        m.write(&m.builtin(), "cache=/from/builtin\n");
        assert_eq!(m.detect().unwrap().dir, Path::new("/from/builtin"));

        m.write(&m.prefix.join("etc/npmrc"), "cache=/from/global\n");
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, Path::new("/from/global"));
        assert!(loc.source.contains("global npmrc"), "{}", loc.source);

        m.write(&m.home.join(".npmrc"), "cache=~/from-user\n");
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, m.home.join("from-user"));
        assert!(loc.source.contains("user npmrc"), "{}", loc.source);

        m.vars.insert("npm_config_cache".into(), "/from/env".into());
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, Path::new("/from/env"));
        assert_eq!(loc.source, "$npm_config_cache");
    }

    #[test]
    fn global_npmrc_follows_prefix_from_builtin_like_homebrew() {
        let m = Machine::new();
        // Homebrew ships `prefix = /opt/homebrew` in npm's builtin npmrc.
        let brew = m.home.join("brew");
        m.write(&m.builtin(), &format!("prefix = {}\n", brew.display()));
        m.write(&brew.join("etc/npmrc"), "cache=/brew/cache\n");
        // The default prefix's npmrc must then be ignored.
        m.write(&m.prefix.join("etc/npmrc"), "cache=/wrong\n");
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, Path::new("/brew/cache"));
        assert!(
            loc.source
                .contains(&brew.join("etc/npmrc").display().to_string())
        );
    }

    #[test]
    fn globalconfig_and_prefix_overrides() {
        let mut m = Machine::new();
        m.write(&m.home.join("g/npmrc"), "cache=/from/globalconfig\n");
        m.write(&m.home.join(".npmrc"), "globalconfig=~/g/npmrc\n");
        assert_eq!(m.detect().unwrap().dir, Path::new("/from/globalconfig"));

        m.write(&m.home.join("p/etc/npmrc"), "cache=/from/prefix\n");
        m.write(&m.home.join(".npmrc"), "prefix=~/p\n");
        assert_eq!(m.detect().unwrap().dir, Path::new("/from/prefix"));

        m.write(&m.home.join("q/etc/npmrc"), "cache=/from/PREFIX\n");
        fs::remove_file(m.home.join(".npmrc")).unwrap();
        m.vars
            .insert("PREFIX".into(), m.home.join("q").display().to_string());
        assert_eq!(m.detect().unwrap().dir, Path::new("/from/PREFIX"));

        m.vars.insert(
            "NPM_CONFIG_USERCONFIG".into(),
            m.home.join("other-npmrc").display().to_string(),
        );
        m.write(&m.home.join("other-npmrc"), "cache=/from/userconfig\n");
        assert_eq!(m.detect().unwrap().dir, Path::new("/from/userconfig"));
    }

    #[test]
    fn ambiguous_values_are_refused() {
        let mut m = Machine::new();
        m.write(&m.prefix.join("etc/npmrc"), "cache=relative/cache\n");
        assert!(m.detect().is_err());
        m.write(&m.prefix.join("etc/npmrc"), "cache=${HOME}/c\n");
        assert!(m.detect().unwrap_err().contains("variable expansion"));
        m.write(&m.home.join(".npmrc"), "globalconfig=${PREFIX}/etc/npmrc\n");
        assert!(m.detect().is_err());
        m.vars.insert("npm_config_cache".into(), "rel".into());
        assert!(m.detect().is_err());
    }

    #[test]
    fn missing_node_skips_global_npmrc_with_a_note() {
        let mut m = Machine::new();
        m.vars.insert("PATH".into(), "/nonexistent".into());
        m.write(&m.prefix.join("etc/npmrc"), "cache=/from/global\n");
        let loc = m.detect().unwrap();
        assert_eq!(loc.dir, m.home.join(".npm"));
        assert!(loc.notes.iter().any(|n| n.contains("global npmrc")));
    }

    #[test]
    fn project_npmrc_is_reported_not_used() {
        let m = Machine::new();
        let project = m.home.join("code/app");
        m.write(&project.join("package.json"), "{}");
        m.write(&project.join(".npmrc"), "cache=/project/cache\n");
        let sub = project.join("src");
        fs::create_dir_all(&sub).unwrap();
        let loc = m.detect_in(Some(&sub)).unwrap();
        assert_eq!(loc.dir, m.home.join(".npm"));
        assert_eq!(loc.notes.len(), 1);
        assert!(loc.notes[0].contains("cache=/project/cache"));

        // In home itself, .npmrc is the user npmrc, not a project npmrc.
        m.write(&m.home.join(".npmrc"), "cache=~/user-cache\n");
        let loc = m.detect_in(Some(&m.home)).unwrap();
        assert_eq!(loc.dir, m.home.join("user-cache"));
        assert!(loc.notes.is_empty());
    }
}
