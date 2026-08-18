//! What the manifest files say, and the one merge rule they all share.
//!
//! Every input is TOML and every input is *data* — there is no expression
//! language here, and deliberately so: the whole point of the manifest layer is
//! that resolution is a pure function of the files plus the module tree, so it
//! can be re-run anywhere and produce byte-identical output. Templating is
//! Dotdrop's job, downstream; a `{{@@ … @@}}` string is opaque to us.
//!
//! The types are split by *who writes them*: [`Composition`] is the one
//! hand-written file that describes machines (`hosts.toml`), [`Manifest`] is the
//! per-module file that describes deployable units, and [`Fragment`] is the
//! per-module-per-overlay file that carries values which must not be shared.
//! Everything else in this module is [`merge_into`], because a single, stated
//! merge rule is what makes the layering in `resolve` legible.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use toml::{Table, Value};

/// `modules/dotdrop/hosts.toml` — the composition table.
///
/// This is the only file that knows machines exist. Planes live here rather
/// than being built into the tool because a plane is not intrinsically `user`
/// or `system`: it is *an execution context with its own Dotdrop settings*, and
/// which contexts a setup has is a property of the setup.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composition {
    /// The backend's settings block every generated entrypoint starts from,
    /// before its plane's overrides. Passed through untouched apart from the
    /// keys the generator computes — see [`Composition::RESERVED_CONFIG_KEYS`].
    #[serde(default)]
    pub config: Table,
    /// The execution contexts this setup deploys into, by name.
    #[serde(default)]
    pub planes: BTreeMap<String, Plane>,
    /// Variables every host starts with, overridden per host.
    #[serde(default)]
    pub defaults: Table,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
}

impl Composition {
    /// `[config]` keys the generator computes from the layout, so a hand-written
    /// value would be silently overwritten. Rejected at check time instead.
    ///
    /// Deliberately only these three. Every other key is passed through, because
    /// the settings block belongs to the deployment backend and guessing which
    /// of its keys are sensible is not this tool's job.
    pub const RESERVED_CONFIG_KEYS: [&'static str; 3] =
        ["dotpath", "import_variables", "import_actions"];
}

/// One execution context. A plane partitions *output* — installs from two
/// planes cannot share a Dotdrop run, because they need different privileges
/// and usually a different `workdir` — which is what distinguishes it from a
/// capability, a pure filter that changes nothing about how deployment runs.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plane {
    /// Dotdrop `[config]` overrides layered over [`Composition::config`] for
    /// this plane's entrypoints. This is the whole reason a plane carries data
    /// rather than being a bare name: a privileged run resolves `~` to a
    /// different home, so it needs its own `workdir`.
    #[serde(default)]
    pub config: Table,
    /// Every install in this plane must have a `dst` starting with one of these.
    /// Empty means unconstrained. A cheap guard against the failure this layout
    /// invites — a `~/…` destination that ends up under `/root` when the run is
    /// privileged.
    #[serde(default)]
    pub dst_prefixes: Vec<String>,
}

/// One concrete machine. Hosts never list modules; they state what they *are*
/// (capabilities), what content layers they want (overlays), and which
/// execution contexts they deploy (planes), and selection follows.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Merge and deployment order; later overlays win.
    #[serde(default)]
    pub overlays: Vec<String>,
    /// Absent means every declared plane, which is the common case and keeps a
    /// two-plane setup from restating itself on every host.
    pub planes: Option<Vec<String>>,
    /// Machine-specific overrides, layered over [`Composition::defaults`].
    #[serde(default)]
    pub variables: Table,
}

/// `modules/<app>/manifest.toml` — what a module deploys and what it exports.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, rename = "install")]
    pub installs: Vec<Install>,
    #[serde(default)]
    pub variables: Table,
    #[serde(default)]
    pub dynvariables: Table,
    #[serde(default)]
    pub actions: Table,
}

/// One deployable unit. It names a *path inside an overlay*, not a source
/// directory: the same install produces one Dotdrop entry per host overlay that
/// actually contains that path, which is how `git` fans out to `git-common` and
/// `git-personal` while `amdgpu-pro` naturally yields only `amdgpu-pro-khronos3d`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub id: String,
    /// Deployment destination, passed to Dotdrop verbatim (it may template).
    pub dst: String,
    /// Path relative to `modules/<app>/<overlay>/`; `.` means the overlay root.
    pub path: String,
    /// Host capabilities this unit needs. Empty means unconditional — an
    /// install that applies everywhere should not have to invent a capability
    /// to say so.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Execution contexts this unit belongs to. Required: a deployable unit
    /// with no stated plane has no defined privilege, and guessing is how a
    /// user file lands in `/etc`.
    #[serde(default)]
    pub planes: Vec<String>,
    /// Overlays that must all be present on the host, for a unit whose content
    /// is meaningless without them.
    #[serde(default)]
    pub requires_overlays: Vec<String>,
    pub link: Option<String>,
    pub chmod: Option<String>,
    #[serde(default)]
    pub actions: Vec<String>,
}

/// `modules/<app>/manifest/<overlay>.toml` — values that belong to one module
/// and one overlay, and therefore to one transcrypt key.
///
/// Only variables, deliberately. Actions and dynvariables are named in a single
/// flat namespace shared by every module, so allowing an overlay to define one
/// would make the set of available names depend on which host you are — a
/// collision that only appears on one machine is the worst kind.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    #[serde(default)]
    pub variables: Table,
}

/// `modules/dotdrop/globals.toml` — genuinely cross-module plaintext values.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Globals {
    #[serde(default)]
    pub variables: Table,
    #[serde(default)]
    pub dynvariables: Table,
    #[serde(default)]
    pub actions: Table,
}

/// Layer `overlay` onto `base`: tables recurse, everything else replaces.
///
/// Replacing rather than concatenating lists is the deliberate half. A list in
/// this model is a *setting* — a host's overlays, a module's kernel parameters —
/// and the only useful thing an overlay can say about a setting is what it
/// should now be. Appending would make "drop the inherited value" inexpressible.
pub fn merge_into(base: &mut Table, overlay: &Table) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Table(existing)), Value::Table(incoming)) => {
                merge_into(existing, incoming);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Every name a template inside `value` might be reaching for.
///
/// Deliberately over-approximate: it collects every identifier appearing in a
/// `{{ … }}` region, including the tail of a dotted path and the inside of a
/// subscript, and leaves it to the caller to keep only the ones that name a real
/// variable. Over-collecting costs a redundant lookup; under-collecting produces
/// a variables file Dotdrop cannot resolve, which is a deployment-time failure —
/// so the asymmetry decides the design.
pub fn template_references(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => scan(text, out),
        Value::Array(items) => items.iter().for_each(|v| template_references(v, out)),
        Value::Table(table) => table.values().for_each(|v| template_references(v, out)),
        _ => {}
    }
}

fn scan(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(start) = text[i..].find("{{").map(|p| i + p + 2) {
        let end = text[start..].find("}}").map_or(text.len(), |p| start + p);
        let mut j = start;
        while j < end {
            if bytes[j].is_ascii_alphabetic() || bytes[j] == b'_' {
                let from = j;
                while j < end && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                out.insert(text[from..j].to_owned());
            } else {
                j += 1;
            }
        }
        i = (end + 2).min(text.len());
    }
}

/// Every scalar path in `table`, dotted, paired with its value. Used to report
/// two modules writing the same variable — a silent last-writer-wins that the
/// merge rule alone cannot distinguish from an intentional override.
pub fn leaves(table: &Table, prefix: &str, out: &mut Vec<(String, Value)>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(inner) => leaves(inner, &path, out),
            other => out.push((path, other.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> Table {
        s.parse().unwrap()
    }

    #[test]
    fn tables_recurse_and_scalars_replace() {
        let mut base = table("[a]\nx = 1\ny = 2\n[b]\nz = 3\n");
        merge_into(&mut base, &table("[a]\ny = 20\nw = 4\n"));

        // `a.x` survived (recursed, not replaced), `a.y` took the new value, and
        // the untouched `b` table is intact.
        assert_eq!(base["a"]["x"].as_integer(), Some(1));
        assert_eq!(base["a"]["y"].as_integer(), Some(20));
        assert_eq!(base["a"]["w"].as_integer(), Some(4));
        assert_eq!(base["b"]["z"].as_integer(), Some(3));
    }

    #[test]
    fn lists_replace_rather_than_append() {
        let mut base = table("k = ['a', 'b']\n");
        merge_into(&mut base, &table("k = ['c']\n"));
        assert_eq!(base["k"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_table_replaces_a_scalar_of_the_same_name() {
        let mut base = table("k = 1\n");
        merge_into(&mut base, &table("[k]\nn = 2\n"));
        assert!(base["k"].is_table());
    }

    #[test]
    fn template_references_reach_into_every_container() {
        let mut out = BTreeSet::new();
        template_references(
            &Value::Table(table(
                "a = \"{{@@ sysenv.xdg_config_home @@}}/x\"\n\
                 b = [\"{{@@ env['HOME'] @@}}\"]\n\
                 [c]\nd = \"{{@@ testing_runner_dir @@}}\"\n\
                 e = 'no template here at all'\n",
            )),
            &mut out,
        );

        assert!(out.contains("sysenv"));
        assert!(out.contains("testing_runner_dir"));
        assert!(out.contains("env"), "the caller filters, not the scanner");
        assert!(
            !out.contains("here"),
            "text outside a template region is not a reference"
        );
    }

    #[test]
    fn leaves_are_dotted_paths() {
        let mut out = Vec::new();
        leaves(&table("[a.b]\nc = 1\n[a]\nd = 2\n"), "", &mut out);
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a.b.c", "a.d"]);
    }
}
