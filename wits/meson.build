# Meson orchestrates the build and install; cargo does the actual Rust
# compilation underneath (option "b"). This is deliberate: wits depends on
# crates with native-code build scripts (notably `ring`, via the forge's HTTPS
# client), which meson's native Rust support cannot build without hand-porting
# their build.rs — so cargo owns compilation, and meson owns install/packaging.
#
# No project language is declared: meson never invokes rustc itself here, it
# only drives cargo through a custom_target.

project(
  'wits',
  version : '0.1.0',
  license : 'MIT',
  # install_symlink() is the newest feature used; it sets the floor.
  meson_version : '>= 0.61.0',
)

cargo = find_program('cargo')
python = find_program('python3')

manifest = meson.project_source_root() / 'Cargo.toml'
cargo_target = meson.current_build_dir() / 'cargo-target'

# Map meson's buildtype onto a cargo profile, so `meson setup --buildtype=...`
# actually controls the Rust build instead of being silently ignored. Meson
# owns only this coarse mode; cargo keeps ownership of the fine-grained compile
# config (rustflags, profiles, per-dependency settings), which is the whole
# reason compilation stays with cargo.
if get_option('buildtype') in ['release', 'debugoptimized', 'minsize']
  cargo_profile_args = ['--release']
  cargo_profile_dir = 'release'
else
  cargo_profile_args = []
  cargo_profile_dir = 'debug'
endif

# The cargo -> meson bridge, inline in meson's own runtime (python3) rather than
# a shell script: meson's custom_target requires the command to leave its
# product at @OUTPUT@, while cargo always writes under <target-dir>/<profile>/,
# so we build and then place the binary. cargo does its own up-to-date checking,
# which is why the target is always stale.
#   argv: cargo, manifest, target-dir, profile-dir, binname, out, [extra cargo args...]
cargo_bridge = '''
import os, shutil, subprocess, sys
cargo, manifest, target_dir, profile_dir, name, out = sys.argv[1:7]
subprocess.run(
    [cargo, "build", "--manifest-path", manifest, "--target-dir", target_dir]
    + sys.argv[7:],
    check=True,
)
shutil.copyfile(os.path.join(target_dir, profile_dir, name), out)
os.chmod(out, 0o755)
'''

wits = custom_target(
  'wits',
  output : 'wits',
  command : [
    python, '-c', cargo_bridge,
    cargo, manifest, cargo_target, cargo_profile_dir, 'wits', '@OUTPUT@',
    cargo_profile_args,
  ],
  console : true,
  build_by_default : true,
  build_always_stale : true,
  install : true,
  install_dir : get_option('bindir'),
)

# Each built-in subcommand gets a `wits-<name>` applet symlink next to the
# binary, matching wits's own argv[0] dispatch. This list is explicit on
# purpose: meson deliberately has no source-tree globbing (explicit over
# implicit), and the authoritative set of built-ins is the Commands enum in
# crates/wits/src/main.rs, not the cmd/ directory. Keep the two in step; a
# runtime cross-check is `wits __applets`.
builtins = [
  'build',
  'dotfiles',
  'project',
  'review',
  'stack',
  'transcrypt',
  'update',
  'worktree',
]
foreach sub : builtins
  install_symlink(
    'wits-' + sub,
    pointing_to : 'wits',
    install_dir : get_option('bindir'),
  )
endforeach

# Plugin binaries: an in-tree Rust plugin is a `crates/wits-<name>` bin crate
# added to the workspace `members`. When one exists, cargo builds it into the
# same target dir; add a custom_target + install here mirroring `wits` above.
# (None yet — gputest will be the first; see docs/plugins.md.)
