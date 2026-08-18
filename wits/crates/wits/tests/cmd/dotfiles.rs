//! Black-box tests for `wits dotfiles`, driven through the real binary against a
//! real directory tree.
//!
//! Three things here can only be tested this way. Selection reads the **module
//! tree**, not just the manifests — whether a module has content for an overlay
//! is half the answer to what gets deployed — so a fixture on disk is the
//! subject, not scaffolding around it. The output has to satisfy invariants
//! Dotdrop imposes but never states: it rejects a config missing any top-level
//! section, it resolves `src` against the declaring config's own `dotpath`, and
//! it resolves each imported variables file with a templater built from that
//! file alone. And every path in the output is computed from the layout, so the
//! fixture below deliberately uses **none** of the default names — anything
//! still hard-coded fails here rather than in someone's repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// Two planes, two hosts with different overlays and capabilities, and a
    /// private fragment whose override drags a shared referent along with it —
    /// all under a layout that shares no path with the defaults.
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        write(
            &root.join("dotfiles.toml"),
            r#"
[layout]
modules = 'apps'
module_manifest = 'app.toml'
module_fragments = 'private'
composition = 'etc/machines.toml'

[output]
dir = 'build'
entrypoint = '{plane}/{host}.conf'
variables = 'shared/values.conf'
actions = 'shared/hooks.conf'
overlay_variables = 'secret/{overlay}.conf'
"#,
        );
        write(
            &root.join("etc/machines.toml"),
            r#"
[config]
backup = true
workdir = '~/dotfiles'

[planes.user]
dst_prefixes = ['~/']

[planes.system]
dst_prefixes = ['/etc/']
[planes.system.config]
workdir = '/var/lib/dotdrop'

[hosts.alpha]
capabilities = ['develop']
overlays = ['common', 'personal']
planes = ['user']

[hosts.beta]
capabilities = ['develop', 'desktop']
overlays = ['common']
"#,
        );
        write(
            &root.join("etc/globals.toml"),
            r#"
[variables]
runner_root = "{{@@ env['XDG_RUNTIME_DIR'] @@}}/run"

[variables.testing]
runner = '{{@@ runner_root @@}}/r'
result = 'shared'

[actions]
reload = 'true'
"#,
        );

        write(&root.join("apps/git/common/config"), "");
        write(&root.join("apps/git/personal/config"), "");
        write(
            &root.join("apps/git/app.toml"),
            "[[install]]\nid = 'git'\ndst = '~/.config/git/'\npath = '.'\n\
             capabilities = ['develop']\nplanes = ['user']\n",
        );
        write(
            &root.join("apps/git/private/personal.toml"),
            "[variables.testing]\nresult = 'private'\n",
        );

        write(&root.join("apps/mpv/common/mpv.conf"), "");
        write(
            &root.join("apps/mpv/app.toml"),
            "[[install]]\nid = 'mpv'\ndst = '~/.config/mpv/'\npath = '.'\n\
             capabilities = ['desktop']\nplanes = ['user']\n",
        );

        write(&root.join("apps/sshd/common/sshd_config"), "");
        write(
            &root.join("apps/sshd/app.toml"),
            "[[install]]\nid = 'sshd'\ndst = '/etc/ssh/sshd_config'\n\
             path = 'sshd_config'\nplanes = ['system']\nactions = ['reload']\n",
        );

        Fixture { _dir: dir, root }
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(cwd)
            // The root can come from git config, so a developer who has set
            // `wits.dotfiles.root` must not steer these tests.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("WITS_DOTFILES_CONFIG")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_in(&self.root, args)
    }

    fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert!(
            out.success,
            "`wits {}` failed:\n{}\n{}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        out
    }

    fn toml(&self, relative: &str) -> toml::Table {
        let path = self.root.join(relative);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        text.parse()
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn generate_produces_one_entrypoint_per_plane_and_host() {
    let fx = Fixture::new();
    fx.ok(&["dotfiles", "generate"]);

    for name in [
        "build/user/alpha.conf",
        "build/user/beta.conf",
        "build/system/beta.conf",
        "build/shared/values.conf",
        "build/shared/hooks.conf",
        "build/secret/personal.conf",
    ] {
        assert!(fx.root.join(name).is_file(), "missing {name}");
    }
    assert!(
        !fx.root.join("build/system/alpha.conf").is_file(),
        "alpha names only the user plane"
    );
}

/// Dotdrop refuses a config that omits any top-level section, and it renders a
/// dotfile's `dst` with only the variables of the config that declares it — so
/// an entrypoint has to be complete on its own.
#[test]
fn every_entrypoint_is_a_complete_dotdrop_config() {
    let fx = Fixture::new();
    fx.ok(&["dotfiles", "generate"]);

    for (name, host) in [
        ("build/user/alpha.conf", "alpha"),
        ("build/user/beta.conf", "beta"),
        ("build/system/beta.conf", "beta"),
    ] {
        let doc = fx.toml(name);
        // Computed from where this file actually landed: build/<plane>/ back
        // to the module tree at apps/.
        assert_eq!(
            doc["config"]["dotpath"].as_str(),
            Some("../../apps"),
            "{name}"
        );
        assert!(doc.contains_key("dotfiles"), "{name}");

        let profile = &doc["profiles"][host];
        let declared = doc["dotfiles"].as_table().unwrap();
        for id in profile["dotfiles"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            assert!(
                declared.contains_key(id),
                "{name} selects '{id}' without declaring it"
            );
        }
        assert_eq!(
            doc["profiles"].as_table().unwrap().len(),
            1,
            "{name} should carry only its own host's variables"
        );
    }
}

#[test]
fn capabilities_and_overlays_select_independently() {
    let fx = Fixture::new();
    fx.ok(&["dotfiles", "generate"]);

    let alpha = fx.toml("build/user/alpha.conf");
    let ids: Vec<&str> = alpha["profiles"]["alpha"]["dotfiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["git-common", "git-personal"],
        "alpha has the personal overlay but not the desktop capability"
    );

    let beta = fx.toml("build/user/beta.conf");
    let ids: Vec<&str> = beta["profiles"]["beta"]["dotfiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["git-common", "mpv-common"],
        "beta has the desktop capability but no personal overlay"
    );
}

/// Dotdrop merges imported variables shallowly and resolves each file with a
/// templater built from that file alone, so an aggregate has to republish the
/// whole top-level key it touches *and* bring along whatever those values refer
/// to — while leaving everything else in the shared file, which is what keeps a
/// shared edit from rewriting an encrypted one.
#[test]
fn an_overlay_aggregate_is_self_contained_but_not_a_copy() {
    let fx = Fixture::new();
    fx.ok(&["dotfiles", "generate"]);

    let vars = fx.toml("build/secret/personal.conf");
    let vars = vars["variables"].as_table().unwrap();

    assert_eq!(vars["testing"]["result"].as_str(), Some("private"));
    assert_eq!(
        vars["testing"]["runner"].as_str(),
        Some("{{@@ runner_root @@}}/r"),
        "the sibling leaf has to survive the shallow merge"
    );
    assert!(
        vars.contains_key("runner_root"),
        "and its referent has to travel with it: {vars:?}"
    );

    let alpha = fx.toml("build/user/alpha.conf");
    let imports: Vec<&str> = alpha["config"]["import_variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        imports,
        vec!["../shared/values.conf", "../secret/personal.conf"],
        "relative to where the entrypoint landed, private overlay last so it wins"
    );
}

/// An overlay may be spread over several files so that values needing different
/// encryption treatment can be marked separately in `.gitattributes` — which
/// only works if they still arrive as one merged layer.
#[test]
fn an_overlay_split_across_files_arrives_as_one_layer() {
    let fx = Fixture::new();
    write(
        &fx.root.join("apps/git/private/personal.identity.toml"),
        "[variables.git.identity]\nemail = 'me@example.com'\n",
    );
    write(
        &fx.root.join("apps/git/private/personal.secret.toml"),
        "[variables.testing]\nresult = 'from the later part'\n",
    );
    fx.ok(&["dotfiles", "generate"]);

    let vars = fx.toml("build/secret/personal.conf");
    let vars = vars["variables"].as_table().unwrap();

    assert_eq!(
        vars["git"]["identity"]["email"].as_str(),
        Some("me@example.com")
    );
    assert_eq!(
        vars["testing"]["result"].as_str(),
        Some("from the later part"),
        "a named part layers over the overlay's plain fragment"
    );
    assert_eq!(
        vars["testing"]["runner"].as_str(),
        Some("{{@@ runner_root @@}}/r"),
        "and the shallow-merge republishing still holds across the split"
    );
    assert!(
        vars.contains_key("runner_root"),
        "as does the reference closure: {vars:?}"
    );
}

#[test]
fn regenerating_an_unchanged_tree_writes_nothing() {
    let fx = Fixture::new();
    let first = fx.ok(&["dotfiles", "generate"]);
    assert!(first.stdout.contains("10 written") || first.stdout.contains("written"));

    let second = fx.ok(&["dotfiles", "generate"]);
    assert!(
        second.stdout.contains("0 written"),
        "second run rewrote files:\n{}",
        second.stdout
    );
}

#[test]
fn a_dry_run_writes_nothing_at_all() {
    let fx = Fixture::new();
    let out = fx.ok(&["dotfiles", "-n", "generate"]);

    assert!(out.stdout.contains("[DRY-RUN]"), "{}", out.stdout);
    assert!(!fx.root.join("build/user/alpha.conf").exists());
}

#[test]
fn the_root_is_found_by_walking_up_and_by_environment() {
    let fx = Fixture::new();
    let nested = fx.root.join("apps/git/common");

    let walked = fx.run_in(&nested, &["dotfiles", "check"]);
    assert!(walked.success, "{}{}", walked.stdout, walked.stderr);

    let elsewhere = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_wits"))
        .args(["dotfiles", "check"])
        .current_dir(elsewhere.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("WITS_DOTFILES_CONFIG", fx.root.join("dotfiles.toml"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lost = Command::new(env!("CARGO_BIN_EXE_wits"))
        .args(["dotfiles", "check"])
        .current_dir(elsewhere.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("WITS_DOTFILES_CONFIG")
        .output()
        .unwrap();
    assert!(!lost.status.success());
    assert!(String::from_utf8_lossy(&lost.stderr).contains("no dotfiles repository"));
}

#[test]
fn a_dst_outside_its_planes_prefixes_fails_the_check() {
    let fx = Fixture::new();
    write(
        &fx.root.join("apps/sshd/app.toml"),
        "[[install]]\nid = 'sshd'\ndst = '~/sshd_config'\npath = 'sshd_config'\n\
         planes = ['system']\n",
    );

    let out = fx.run(&["dotfiles", "check"]);
    assert!(!out.success);
    assert!(
        out.stderr.contains("outside plane 'system'"),
        "{}",
        out.stderr
    );
}

/// A fragment left encrypted is a normal state of a clone without that
/// overlay's key, and generating from it would silently drop the overlay's
/// values — so it has to stop the run and say which file and why.
#[test]
fn a_locked_fragment_stops_the_run() {
    let fx = Fixture::new();
    write(
        &fx.root.join("apps/git/private/personal.toml"),
        "U2FsdGVkX19jaXBoZXJ0ZXh0LWdvZXMtaGVyZQ==",
    );

    let out = fx.run(&["dotfiles", "generate"]);
    assert!(!out.success);
    assert!(out.stderr.contains("still encrypted"), "{}", out.stderr);
    assert!(out.stderr.contains("personal.toml"), "{}", out.stderr);
}

#[test]
fn a_renamed_host_leaves_its_old_entrypoint_reported() {
    let fx = Fixture::new();
    fx.ok(&["dotfiles", "generate"]);
    std::fs::rename(
        fx.root.join("build/user/alpha.conf"),
        fx.root.join("build/user/gamma.conf"),
    )
    .unwrap();

    let out = fx.ok(&["dotfiles", "check"]);
    assert!(
        out.stderr.contains("stale generated file") && out.stderr.contains("build/user/gamma.conf"),
        "{}",
        out.stderr
    );
}
