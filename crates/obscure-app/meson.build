global_conf = configuration_data()
global_conf.set_quoted('APP_ID', application_id)
global_conf.set_quoted('APP_VERSION', version)
global_conf.set_quoted('PROFILE', get_option('profile'))
global_conf.set_quoted('GETTEXT_PACKAGE', gettext_package)
global_conf.set_quoted('LOCALEDIR', localedir)
global_conf.set_quoted('PKGDATADIR', pkgdatadir)
global_conf.set_quoted('BUILD_DATADIR', meson.project_build_root() / 'data')

config_rs = configure_file(
  input: 'src/config.rs.in',
  output: 'config.rs',
  configuration: global_conf,
)

cargo_options = [
  '--manifest-path', meson.project_source_root() / 'Cargo.toml',
  '--target-dir', meson.project_build_root() / 'target',
  '--package', 'obscure-app',
]

if get_option('profile') == 'default'
  cargo_options += ['--release']
  rust_target = 'release'
else
  rust_target = 'debug'
endif

if get_option('offline')
  cargo_options += ['--offline']
endif

cargo_env = environment()
cargo_env.set('CODEGEN_BUILD_DIR', meson.current_build_dir())

cargo_build = custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: 'obscure',
  console: true,
  install: true,
  install_dir: bindir,
  depend_files: [config_rs],
  env: cargo_env,
  command: [
    meson.project_source_root() / 'build-aux/cargo.sh',
    meson.project_build_root() / 'target' / rust_target / 'obscure',
    '@OUTPUT@',
    cargo,
    cargo_options,
  ],
)

# Meson (>= 1.3) already checks whether rustc is present; run the unit tests
# of the whole workspace as part of `meson test`.
test(
  'cargo-test',
  cargo,
  args: ['test', '--workspace', '--manifest-path', meson.project_source_root() / 'Cargo.toml',
         '--target-dir', meson.project_build_root() / 'target'],
  env: cargo_env,
  timeout: 1200,
)
