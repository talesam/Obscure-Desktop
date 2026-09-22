ui_dir = meson.project_source_root() / 'crates' / 'obscure-app' / 'src' / 'ui'

# One custom_target per .blp so ninja tracks each generated .ui and rebuilds
# the gresource whenever a Blueprint file changes (a directory output would
# not be noticed).
blueprint_files = [
  'window',
  'shortcuts-dialog',
  'preferences-dialog',
  'import-dialog',
  'log-dialog',
  'server-dialog',
  'connections-dialog',
  'json-dialog',
]

blueprints = []
foreach name : blueprint_files
  blueprints += custom_target(
    name + '.ui',
    input: ui_dir / name + '.blp',
    output: name + '.ui',
    command: [blueprint_compiler, 'compile', '--output', '@OUTPUT@', '@INPUT@'],
  )
endforeach

resources = gnome.compile_resources(
  'obscure',
  'resources.gresource.xml',
  gresource_bundle: true,
  dependencies: blueprints,
  install: true,
  install_dir: pkgdatadir,
)
