subdir('icons')
subdir('resources')

# Desktop file
desktop_conf = configuration_data()
desktop_conf.set('app-id', application_id)
desktop_file = i18n.merge_file(
  input: configure_file(
    input: '@0@.desktop.in.in'.format(base_id),
    output: '@0@.desktop.in'.format(application_id),
    configuration: desktop_conf,
  ),
  output: '@0@.desktop'.format(application_id),
  type: 'desktop',
  po_dir: podir,
  install: true,
  install_dir: datadir / 'applications',
)
if desktop_file_validate.found()
  test('validate-desktop', desktop_file_validate, args: [desktop_file])
endif

# AppStream metainfo
metainfo_conf = configuration_data()
metainfo_conf.set('app-id', application_id)
metainfo_conf.set('gettext-package', gettext_package)
metainfo_file = i18n.merge_file(
  input: configure_file(
    input: '@0@.metainfo.xml.in.in'.format(base_id),
    output: '@0@.metainfo.xml.in'.format(application_id),
    configuration: metainfo_conf,
  ),
  output: '@0@.metainfo.xml'.format(application_id),
  po_dir: podir,
  install: true,
  install_dir: datadir / 'metainfo',
)
if appstreamcli.found()
  test('validate-metainfo', appstreamcli, args: ['validate', '--no-net', '--explain', metainfo_file])
endif

# GSettings schema
gschema_conf = configuration_data()
gschema_conf.set('app-id', application_id)
gschema_conf.set('app-path', application_path)
gschema_conf.set('gettext-package', gettext_package)
gschema_file = configure_file(
  input: '@0@.gschema.xml.in'.format(base_id),
  output: '@0@.gschema.xml'.format(application_id),
  configuration: gschema_conf,
  install: true,
  install_dir: datadir / 'glib-2.0' / 'schemas',
)
# Compiles gschemas.compiled into build/data so the app can run uninstalled
# (the generated schema lives in the build dir, so gnome.compile_schemas()
# would not see it).
compiled_schemas = custom_target(
  'gschemas.compiled',
  input: gschema_file,
  output: 'gschemas.compiled',
  build_by_default: true,
  command: [glib_compile_schemas, '--strict', '--targetdir=@OUTDIR@', meson.current_build_dir()],
)
test(
  'validate-gschema',
  glib_compile_schemas,
  args: ['--strict', '--dry-run', meson.current_build_dir()],
)
