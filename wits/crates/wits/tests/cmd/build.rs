//! Black-box tests for the install phase of `wits build`.
//!
//! The phase is a boundary between the project registry, the backend, and a
//! shell script, so a mock would only test the mock. These drive the real binary
//! against a real `meson` (a wits build requirement) and check the files that
//! actually land on disk: a declared `install` hook must replace the backend's
//! install step, `post_install` must run after it, and `-n` must do neither.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The config root every command reads (`WITS_PROJECT_CONFIG`).
    config: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Git env that ignores the developer's own config while still supplying a
/// commit identity.
fn hermetic_git() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@e.com"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@e.com"),
    ]
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(hermetic_git())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {} failed", dir.display());
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        Fixture {
            _dir: dir,
            root,
            config,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// A git checkout at `rel` holding `files`, committed on `main`.
    fn checkout(&self, rel: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.path(rel);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        for (name, body) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "_build/\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "c1"]);
        dir
    }

    /// A meson project with one install rule, so the backend install has a
    /// visible effect. `hooks` is appended to the repo table as-is.
    fn meson_project(&self, name: &str, hooks: &str) -> PathBuf {
        self.meson_project_with(name, "{{repos.main.workdir}}/_install", "", hooks)
    }

    /// `meson_project`, with the install prefix and any extra `[project]`
    /// entries (such as `[project.environment]`) spelled by the caller.
    fn meson_project_with(
        &self,
        name: &str,
        install_dir: &str,
        project_extra: &str,
        hooks: &str,
    ) -> PathBuf {
        let src = self.checkout(
            name,
            &[
                (
                    "meson.build",
                    "project('probe')\ninstall_data('backend.txt', install_dir: '.')\n",
                ),
                ("backend.txt", "backend"),
            ],
        );
        std::fs::write(
            self.config.join(format!("{name}.toml")),
            format!(
                "[project]\nbuild_system = \"meson\"\n{project_extra}\n\n\
                 [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
                 build_dir = \"{{{{repos.main.workdir}}}}/_build\"\n\
                 install_dir = \"{install_dir}\"\n{hooks}",
                src.display()
            ),
        )
        .unwrap();
        src
    }

    /// A cargo project with no dependencies, plus an install hook. The point is
    /// that the hook replaces an install step the cargo backend does not have.
    fn cargo_project(&self, name: &str, hooks: &str) -> PathBuf {
        let src = self.checkout(
            name,
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("src/main.rs", "fn main() {}\n"),
            ],
        );
        std::fs::write(
            self.config.join(format!("{name}.toml")),
            format!(
                "[project]\nbuild_system = \"cargo\"\n\n\
                 [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
                 build_dir = \"{{{{repos.main.workdir}}}}/_build\"\n\
                 install_dir = \"{{{{repos.main.workdir}}}}/_install\"\n{hooks}",
                src.display()
            ),
        )
        .unwrap();
        src
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(cwd)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", &self.config)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str]) -> Out {
        let out = self.run_in(&self.root, args);
        assert!(
            out.success,
            "`wits {}` failed:\n{}\n{}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        out
    }
}

const INSTALL_HOOKS: &str = r#"
[repos.main.hooks]
install = "printf hook > {{install_dir}}/hook.txt"
post_install = "printf post > {{install_dir}}/post.txt"
"#;

/// A declared `install` hook owns the phase: the backend's install rule must not
/// run, and `post_install` must run after the hook, in the install directory.
#[test]
fn an_install_hook_replaces_the_backend_install_and_post_install_runs() {
    let fx = Fixture::new();
    let src = fx.meson_project("probe", INSTALL_HOOKS);

    fx.ok(&["build", "probe", "-b", "main", "--install"]);

    let install = src.join("_install");
    assert!(
        install.join("hook.txt").exists(),
        "install hook did not run"
    );
    assert!(
        install.join("post.txt").exists(),
        "post_install hook did not run"
    );
    assert!(
        !install.join("backend.txt").exists(),
        "the backend install step ran despite the hook"
    );
}

/// Without a hook, `--install` keeps its original meaning: the backend's own
/// install step runs.
#[test]
fn without_an_install_hook_the_backend_install_runs() {
    let fx = Fixture::new();
    let src = fx.meson_project("plain", "");

    fx.ok(&["build", "plain", "-b", "main", "--install"]);

    assert!(
        src.join("_install/backend.txt").exists(),
        "the backend install step did not run"
    );
}

/// A `post_install` hook adds to the backend's install rather than replacing it.
#[test]
fn post_install_runs_after_the_backend_install() {
    let fx = Fixture::new();
    let src = fx.meson_project(
        "after",
        r#"
[repos.main.hooks]
post_install = "printf post > {{install_dir}}/post.txt"
"#,
    );

    fx.ok(&["build", "after", "-b", "main", "--install"]);

    assert!(src.join("_install/backend.txt").exists());
    assert!(
        src.join("_install/post.txt").exists(),
        "post_install did not run"
    );
}

/// The phase is opt-in: a plain build does not run either hook.
#[test]
fn without_install_the_hooks_do_not_run() {
    let fx = Fixture::new();
    let src = fx.meson_project("noinstall", INSTALL_HOOKS);

    fx.ok(&["build", "noinstall", "-b", "main"]);

    assert!(
        !src.join("_install").exists(),
        "an install hook ran without --install"
    );
}

/// A failing hook fails the command, and names the phase and exit status.
#[test]
fn a_failing_install_hook_fails_the_build() {
    let fx = Fixture::new();
    fx.meson_project(
        "fail",
        r#"
[repos.main.hooks]
install = "exit 7"
"#,
    );

    let out = fx.run_in(&fx.root, &["build", "fail", "-b", "main", "--install"]);
    assert!(!out.success, "a failing hook did not fail the build");
    assert!(
        out.stderr.contains("install hook failed (exit 7)"),
        "{}",
        out.stderr
    );
}

/// The hook is handed the plan's paths in the environment, so a script does not
/// have to re-resolve them (or guess the build type).
#[test]
fn an_install_hook_receives_the_plan_paths_in_the_environment() {
    let fx = Fixture::new();
    let src = fx.meson_project(
        "env",
        r#"
[repos.main.hooks]
install = "env > {{install_dir}}/env.txt"
"#,
    );

    fx.ok(&["build", "env", "-b", "main", "--install"]);

    let text = std::fs::read_to_string(src.join("_install/env.txt")).unwrap();
    let has = |line: String| text.lines().any(|l| l == line);
    assert!(has("WITS_PROJECT=env".into()), "{text}");
    assert!(has(format!("WITS_SOURCE_DIR={}", src.display())), "{text}");
    assert!(
        has(format!("WITS_BUILD_DIR={}", src.join("_build").display())),
        "{text}"
    );
    assert!(
        has(format!(
            "WITS_INSTALL_DIR={}",
            src.join("_install").display()
        )),
        "{text}"
    );
    assert!(has("WITS_BUILD_TYPE=debug".into()), "{text}");
    assert!(has("WITS_BRANCH=main".into()), "{text}");
}

/// A relative install prefix is refused before the build: the hook would
/// otherwise resolve it against its own cwd and stage under the wrong root.
#[test]
fn a_relative_install_dir_is_refused_before_the_build() {
    let fx = Fixture::new();
    fx.meson_project_with("relative", "_install", "", INSTALL_HOOKS);

    let out = fx.run_in(&fx.root, &["build", "relative", "-b", "main", "--install"]);
    assert!(!out.success, "a relative install_dir was accepted");
    assert!(
        out.stderr.contains("absolute install_dir"),
        "{}",
        out.stderr
    );
}

/// The process and the hook template see the same declared environment;
/// otherwise `{{env.FOO}}` and `$FOO` could disagree.
#[test]
fn an_install_hook_sees_the_build_environment() {
    let fx = Fixture::new();
    let src = fx.meson_project_with(
        "hookenv",
        "{{repos.main.workdir}}/_install",
        "[project.environment]\nHOOK_ENV = \"from-project\"",
        r#"
[repos.main.hooks]
install = "printf '%s|%s' \"$HOOK_ENV\" \"{{env.HOOK_ENV}}\" > {{install_dir}}/env.txt"
"#,
    );

    fx.ok(&["build", "hookenv", "-b", "main", "--install"]);

    let text = std::fs::read_to_string(src.join("_install/env.txt")).unwrap();
    assert_eq!(text.trim(), "from-project|from-project");
}

/// cargo has no install step; an `install` hook is what makes `--install` work
/// there instead of failing the request at the backend.
#[test]
fn an_install_hook_makes_a_cargo_install_possible() {
    let fx = Fixture::new();
    let src = fx.cargo_project(
        "cargo",
        r#"
[repos.main.hooks]
install = "printf hook > {{install_dir}}/hook.txt"
"#,
    );

    fx.ok(&["build", "cargo", "-b", "main", "--install"]);

    assert!(
        src.join("_install/hook.txt").exists(),
        "the cargo install hook did not run"
    );
}

/// The phase and its reversal cannot both be asked for.
#[test]
fn install_and_uninstall_are_mutually_exclusive() {
    let fx = Fixture::new();
    let out = fx.run_in(&fx.root, &["build", "whatever", "--install", "--uninstall"]);
    assert!(!out.success, "both flags were accepted");
    assert!(out.stderr.contains("--install"), "{}", out.stderr);
}

/// A dry run prints the steps and creates nothing, including the install
/// directory the hook would otherwise run in.
#[test]
fn a_dry_run_neither_creates_nor_runs_the_install_hook() {
    let fx = Fixture::new();
    let src = fx.meson_project("dry", INSTALL_HOOKS);

    fx.ok(&["build", "dry", "-b", "main", "--install", "-n"]);

    assert!(
        !src.join("_install").exists(),
        "dry run created the install directory"
    );
}
