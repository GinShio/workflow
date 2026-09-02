//! Toolchain selection and resolution (§5.4 inputs).
//!
//! Selection picks a toolchain *name* by the env → `--toolchain` → project chain;
//! resolution renders its raw templated fields against the pipeline context and
//! exposes the result under `toolchain.*` so config can reference `{{toolchain.cc}}`.
//! The backend-native translation of a resolved [`Toolchain`] into env/definitions
//! is a separate concern (the `ToolchainInjector` seam in [`super::resolve`]).

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use minijinja::Value;

use super::context::{from_toml, Ctx};
use super::model::{Profile, RawToolchain, Toolchain};
use super::workspace::{ProjectData, Workspace};

/// Select the toolchain by name via the chain: env → `--toolchain` → the
/// project's `toolchain` field. (Env wins, per the codebase's "env is the
/// deliberate override" rule.) Returns the name and its raw definition.
pub(crate) fn select_toolchain(
    ws: &Workspace,
    project: &ProjectData,
    profile: &Profile,
) -> Result<Option<(String, RawToolchain)>> {
    let name = std::env::var("WITS_PROJECT_TOOLCHAIN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| profile.toolchain.clone())
        .or_else(|| project.project.toolchain.clone());
    let Some(name) = name else {
        return Ok(None);
    };
    let raw =
        ws.toolchains().get(&name).cloned().with_context(|| {
            format!("unknown toolchain '{name}' (none is declared by that name)")
        })?;
    Ok(Some((name, raw)))
}

/// Render a raw toolchain's templated fields against `ctx` and publish the result
/// under `toolchain.*`.
pub(crate) fn resolve_toolchain(
    ctx: &mut Ctx,
    name: String,
    raw: &RawToolchain,
) -> Result<Toolchain> {
    let opt = |ctx: &Ctx, s: &Option<String>| -> Result<Option<String>> {
        match s {
            Some(v) => Ok(Some(ctx.render(v)?)),
            None => Ok(None),
        }
    };
    let list = |ctx: &Ctx, xs: &[String]| -> Result<Vec<String>> {
        Ok(xs.iter().map(|x| ctx.render(x)).collect::<Result<_, _>>()?)
    };
    let environment = raw
        .environment
        .iter()
        .map(|(k, v)| Ok((k.clone(), ctx.render_value(&from_toml(v))?)))
        .collect::<Result<Vec<_>>>()?;
    let definitions = raw
        .definitions
        .iter()
        .map(|(k, v)| Ok((k.clone(), ctx.resolve(&from_toml(v))?)))
        .collect::<Result<Vec<_>>>()?;

    let tc = Toolchain {
        cc: opt(ctx, &raw.cc)?,
        cxx: opt(ctx, &raw.cxx)?,
        rustc: opt(ctx, &raw.rustc)?,
        ar: opt(ctx, &raw.ar)?,
        nm: opt(ctx, &raw.nm)?,
        ranlib: opt(ctx, &raw.ranlib)?,
        strip: opt(ctx, &raw.strip)?,
        linker: opt(ctx, &raw.linker)?,
        launcher: opt(ctx, &raw.launcher)?,
        c_flags: list(ctx, &raw.c_flags)?,
        cxx_flags: list(ctx, &raw.cxx_flags)?,
        link_flags: list(ctx, &raw.link_flags)?,
        environment,
        definitions,
        name: name.clone(),
    };

    // Expose toolchain.* so config can reference {{toolchain.cc}} etc.
    let mut m = BTreeMap::new();
    m.insert("name".to_owned(), Value::from(name.clone()));
    let put = |m: &mut BTreeMap<String, Value>, k: &str, v: &Option<String>| {
        m.insert(k.to_owned(), Value::from(v.clone().unwrap_or_default()));
    };
    put(&mut m, "cc", &tc.cc);
    put(&mut m, "cxx", &tc.cxx);
    put(&mut m, "rustc", &tc.rustc);
    put(&mut m, "ar", &tc.ar);
    put(&mut m, "nm", &tc.nm);
    put(&mut m, "ranlib", &tc.ranlib);
    put(&mut m, "strip", &tc.strip);
    put(&mut m, "linker", &tc.linker);
    put(&mut m, "launcher", &tc.launcher);
    m.insert("c_flags".to_owned(), Value::from(tc.c_flags.join(" ")));
    m.insert("cxx_flags".to_owned(), Value::from(tc.cxx_flags.join(" ")));
    m.insert(
        "link_flags".to_owned(),
        Value::from(tc.link_flags.join(" ")),
    );
    ctx.set("toolchain", Value::from(m));

    Ok(tc)
}
