ui_dir = meson.project_source_root() / 'crates' / 'obscure-app' / 'src' / 'ui'

blueprints = custom_target(
  'blueprints',
  input: files(
    ui_dir / 'window.blp',
    ui_dir / 'shortcuts-dialog.blp',
    ui_dir / 'preferences-dialog.blp',
  ),
  output: 'ui',
  command: [blueprint_compiler, 'batch-compile', '@OUTPUT@', ui_dir, '@INPUT@'],
)

resources = gnome.compile_resources(
  'obscure',
  'resources.gresource.xml',
  gresource_bundle: true,
  source_dir: [meson.current_build_dir(), meson.current_source_dir()],
  dependencies: blueprints,
  install: true,
  install_dir: pkgdatadir,
)
