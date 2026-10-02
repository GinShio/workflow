//! Black-box tests for `wits devenv`.
//!
//! The behaviour worth pinning lives at two process boundaries a mock would not
//! tell the truth about: reading a build system's own developer environment back
//! out of `meson devenv`, and handing the composed environment to a program that
//! replaces `wits`. So these drive the real binary, with a real `meson` (a wits
//! build requirement) configuring a project that declares no language and
//! therefore needs no compiler.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
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
            std::fs::write(dir.join(name), body).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "_build/\ntarget/\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "c1"]);
        dir
    }

    fn run_in(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(cwd)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", &self.config)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let out = self.run_in(&self.root, args, env);
        assert!(
            out.success,
            "`wits {}` failed:\n{}\n{}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        out
    }

    /// A cargo project named `name` — a backend with no developer environment
    /// of its own — whose build directory, `target`, already exists.
    fn cargo_project(&self, name: &str, devenv: &str) -> PathBuf {
        let src = self.checkout(name, &[("Cargo.toml", "")]);
        std::fs::create_dir_all(src.join("target")).unwrap();
        std::fs::write(
            self.config.join(format!("{name}.toml")),
            format!(
                "[project]\nbuild_system = \"cargo\"\n{devenv}\n\
                 [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
                 build_dir = \"{{{{repos.main.workdir}}}}/target\"\n",
                src.display()
            ),
        )
        .unwrap();
        src
    }

    /// A meson project whose `meson.build` registers a developer environment of
    /// its own, configured into `_build`.
    fn meson_project(&self, devenv: &str) -> PathBuf {
        let src = self.checkout(
            "probe",
            &[(
                "meson.build",
                "project('probe')\n\
                 env = environment()\n\
                 env.set('PROBE_SET', 'from-meson')\n\
                 env.prepend('PROBE_LIST', 'meson')\n\
                 meson.add_devenv(env)\n",
            )],
        );
        let ok = Command::new("meson")
            .args(["setup", "_build", "."])
            .current_dir(&src)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("meson is a wits build requirement")
            .success();
        assert!(ok, "meson setup failed in {}", src.display());
        std::fs::write(
            self.config.join("probe.toml"),
            format!(
                "[project]\nbuild_system = \"meson\"\n{devenv}\n\
                 [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
                 build_dir = \"{{{{repos.main.workdir}}}}/_build\"\n",
                src.display()
            ),
        )
        .unwrap();
        src
    }
}

const SHOW: &str =
    r#"printf '%s|%s|%s|%s\n' "${PROBE_SET-}" "${PROBE_LIST-}" "${DECLARED-}" "${WITS_DEVENV-}""#;

/// The meson base is Meson's own developer environment, read from a process it
/// started — so its prepend lands on the caller's value — and the declared
/// operations then land on *that*, including a prepend onto a variable Meson set.
#[test]
fn declared_operations_land_on_mesons_own_developer_environment() {
    let fx = Fixture::new();
    let src = fx.meson_project(
        r#"
[project.devenv]
PROBE_LIST = { prepend = "declared" }
DECLARED = "{{build_dir}}/x"
"#,
    );

    let out = fx.ok(
        &["devenv", "probe", "-b", "main", "--", "sh", "-c", SHOW],
        &[("PROBE_LIST", "caller")],
    );
    assert_eq!(
        out.stdout.trim(),
        format!(
            "from-meson|declared:meson:caller|{}|probe",
            src.join("_build/x").display()
        )
    );

    // The program runs where the caller stands, not in the build directory
    // `meson devenv` itself would have changed into.
    let here = fx.path("elsewhere");
    std::fs::create_dir_all(&here).unwrap();
    let pwd = fx.run_in(
        &here,
        &["devenv", "probe", "-b", "main", "--", "pwd", "-P"],
        &[],
    );
    assert!(pwd.success, "stderr: {}", pwd.stderr);
    assert_eq!(
        PathBuf::from(pwd.stdout.trim()),
        here.canonicalize().unwrap()
    );
}

/// The dump is the same composition printed instead of run, in the syntax its
/// reader takes; it names only what the devenv changes.
#[test]
fn the_dump_speaks_each_readers_syntax() {
    let fx = Fixture::new();
    fx.meson_project("[project.devenv]\nDECLARED = \"it's\"\n");

    let sh = fx.ok(&["devenv", "probe", "-b", "main", "--dump"], &[]);
    assert!(
        sh.stdout.contains("export PROBE_SET='from-meson'\n"),
        "{}",
        sh.stdout
    );
    assert!(
        sh.stdout.contains("export DECLARED='it'\\''s'\n"),
        "{}",
        sh.stdout
    );
    assert!(
        sh.stdout.contains("export WITS_DEVENV='probe'\n"),
        "{}",
        sh.stdout
    );
    assert!(
        !sh.stdout.contains("GIT_AUTHOR_NAME"),
        "unchanged variables leak: {}",
        sh.stdout
    );

    let fish = fx.ok(&["devenv", "probe", "-b", "main", "--dump=fish"], &[]);
    assert!(
        fish.stdout.contains("set -gx DECLARED 'it\\'s'\n"),
        "{}",
        fish.stdout
    );

    let json = fx.ok(&["devenv", "probe", "-b", "main", "--dump=json"], &[]);
    let parsed: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(parsed["set"]["PROBE_SET"], "from-meson");
    assert_eq!(parsed["set"]["DECLARED"], "it's");
    assert_eq!(parsed["unset"], serde_json::json!([]));
}

/// A backend with no developer environment of its own starts from the caller's,
/// and a prepend onto `PATH` decides which program the name resolves to.
#[test]
fn without_a_backend_base_the_callers_environment_is_the_base() {
    let fx = Fixture::new();
    let src = fx.cargo_project(
        "tool",
        "[project.devenv]\nPATH = { prepend = \"{{build_dir}}/bin\" }",
    );
    let bin = src.join("target/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("wits-devenv-probe");
    std::fs::write(&script, "#!/bin/sh\necho from-the-build\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = fx.ok(
        &["devenv", "tool", "-b", "main", "--", "wits-devenv-probe"],
        &[],
    );
    assert_eq!(out.stdout.trim(), "from-the-build");
}

/// With no command, the program a devenv runs is `$SHELL`.
#[test]
fn without_a_command_the_shell_runs_in_the_devenv() {
    let fx = Fixture::new();
    fx.cargo_project("tool", "");
    // `env` stands in for a shell: it prints the environment it was started in.
    let out = fx.ok(
        &["devenv", "tool", "-b", "main"],
        &[("SHELL", "/usr/bin/env")],
    );
    assert!(
        out.stdout.lines().any(|line| line == "WITS_DEVENV=tool"),
        "{}",
        out.stdout
    );
}

/// `--build-dir` and `--install-dir` select the build as they do for `wits
/// build`: they replace the resolved directories the templates read.
#[test]
fn the_output_overrides_select_the_build_as_they_do_for_build() {
    let fx = Fixture::new();
    fx.cargo_project(
        "tool",
        "[project.devenv]\nDECLARED = \"{{build_dir}}|{{install_dir}}\"",
    );
    let build = fx.path("elsewhere/build");
    std::fs::create_dir_all(&build).unwrap();
    let prefix = fx.path("elsewhere/prefix");
    let out = fx.ok(
        &[
            "devenv",
            "tool",
            "-b",
            "main",
            "--build-dir",
            build.to_str().unwrap(),
            "--install-dir",
            prefix.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            r#"printf %s "$DECLARED""#,
        ],
        &[],
    );
    assert_eq!(
        out.stdout,
        format!("{}|{}", build.display(), prefix.display())
    );
}

/// A detached review checkout resolves exactly as `build --detach` does, and a
/// detached HEAD without the flag is the same error `build` gives.
#[test]
fn a_detached_checkout_resolves_like_build_detach() {
    let fx = Fixture::new();
    let src = fx.cargo_project("app", "[project.devenv]\nDECLARED = \"{{build_dir}}\"");
    let review = fx.path("app.review");
    git(
        &src,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            review.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::create_dir_all(review.join("target")).unwrap();

    let implicit = fx.run_in(&review, &["devenv", "--", "true"], &[]);
    assert!(!implicit.success);
    assert!(
        implicit.stderr.contains("--detach") && implicit.stderr.contains("--branch"),
        "stderr: {}",
        implicit.stderr
    );

    let out = fx.run_in(
        &review,
        &["devenv", "--detach", "--", "sh", "-c", SHOW],
        &[],
    );
    assert!(out.success, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        format!("||{}|app", review.join("target").display())
    );
}

/// Running against a build that is not there is an error naming the remedy,
/// never a run against whatever happens to be in the caller's environment.
#[test]
fn a_missing_or_unconfigured_build_is_an_error() {
    let fx = Fixture::new();
    let src = fx.checkout("raw", &[("meson.build", "project('raw')\n")]);
    std::fs::write(
        fx.config.join("raw.toml"),
        format!(
            "[project]\nbuild_system = \"meson\"\n\
             [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
             build_dir = \"{{{{repos.main.workdir}}}}/_build\"\n",
            src.display()
        ),
    )
    .unwrap();

    let missing = fx.run_in(
        &fx.root,
        &["devenv", "raw", "-b", "main", "--", "true"],
        &[],
    );
    assert!(!missing.success);
    assert!(
        missing.stderr.contains("does not exist"),
        "{}",
        missing.stderr
    );

    std::fs::create_dir_all(src.join("_build")).unwrap();
    let unconfigured = fx.run_in(
        &fx.root,
        &["devenv", "raw", "-b", "main", "--", "true"],
        &[],
    );
    assert!(!unconfigured.success);
    assert!(
        unconfigured.stderr.contains("not configured"),
        "{}",
        unconfigured.stderr
    );
}

/// `-n` still reads the base — the result has to be described — but prints the
/// program instead of becoming it.
#[test]
fn a_dry_run_prints_the_program_instead_of_running_it() {
    let fx = Fixture::new();
    fx.meson_project("");
    let out = fx.ok(&["-n", "devenv", "probe", "-b", "main", "--", "false"], &[]);
    assert!(out.stdout.contains("[DRY-RUN]"), "{}", out.stdout);
    assert!(
        out.stdout.contains("PROBE_SET=from-meson"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.trim_end().ends_with("false"), "{}", out.stdout);
}

/// `project info` shows the runtime half next to the build environment: the
/// base it starts from, and every declared operation in application order.
#[test]
fn project_info_reports_the_devenv_base_and_its_operations() {
    let fx = Fixture::new();
    fx.meson_project("[project.devenv]\nPROBE_LIST = { append = \"tail\", separator = \";\" }\n");
    let out = fx.ok(&["project", "info", "probe", "-b", "main"], &[]);
    assert!(out.stdout.contains("devenv:"), "{}", out.stdout);
    assert!(
        out.stdout.contains("  base: meson devenv -C "),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("  PROBE_LIST append tail (separator \";\")"),
        "{}",
        out.stdout
    );
}
