project(
  'obscure',
  'rust',
  # The version lives in crates/obscure-core/src/lib.rs (APP_VERSION).
  version: run_command(
    'grep', '-oP', '(?<=pub const APP_VERSION: &str = ")[^"]+',
    'crates/obscure-core/src/lib.rs',
    check: true,
  ).stdout().strip(),
  meson_version: '>= 1.3.0',
  license: 'GPL-3.0-or-later',
  default_options: ['warning_level=2'],
)

i18n = import('i18n')
gnome = import('gnome')

base_id = 'io.github.talesam.Obscure'

dependency('glib-2.0', version: '>= 2.84')
dependency('gio-2.0', version: '>= 2.84')
dependency('gtk4', version: '>= 4.22')
dependency('libadwaita-1', version: '>= 1.9')

glib_compile_resources = find_program('glib-compile-resources', required: true)
glib_compile_schemas = find_program('glib-compile-schemas', required: true)
desktop_file_validate = find_program('desktop-file-validate', required: false)
appstreamcli = find_program('appstreamcli', required: false)
cargo = find_program('cargo', required: true)
blueprint_compiler = find_program('blueprint-compiler', required: true)

version = meson.project_version()

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
localedir = prefix / get_option('localedir')

datadir = prefix / get_option('datadir')
pkgdatadir = datadir / meson.project_name()
iconsdir = datadir / 'icons'
podir = meson.project_source_root() / 'po'
gettext_package = meson.project_name()

if get_option('profile') == 'development'
  profile = 'Devel'
  # Enable the repository's git hooks for contributors (harmless if not a checkout).
  run_command('git', 'config', 'core.hooksPath', '.githooks', check: false)
else
  profile = ''
endif

application_id = '@0@@1@'.format(base_id, profile == '' ? '' : '.' + profile)
application_path = '/' + application_id.replace('.', '/') + '/'

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.project_build_root() / 'meson-dist' / meson.project_name() + '-' + version,
  meson.project_source_root(),
)

subdir('data')
subdir('po')
subdir('crates/obscure-app')

gnome.post_install(
  gtk_update_icon_cache: true,
  glib_compile_schemas: true,
  update_desktop_database: true,
)
