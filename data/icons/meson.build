scalable_dir = 'hicolor' / 'scalable' / 'apps'
symbolic_dir = 'hicolor' / 'symbolic' / 'apps'

install_data(
  scalable_dir / '@0@.svg'.format(base_id),
  install_dir: iconsdir / scalable_dir,
  rename: '@0@.svg'.format(application_id),
)
install_data(
  symbolic_dir / '@0@-symbolic.svg'.format(base_id),
  install_dir: iconsdir / symbolic_dir,
  rename: '@0@-symbolic.svg'.format(application_id),
)

# Unthemed copies in build/data/icons so the app finds them when run uninstalled.
configure_file(
  input: scalable_dir / '@0@.svg'.format(base_id),
  output: '@0@.svg'.format(application_id),
  copy: true,
)
configure_file(
  input: symbolic_dir / '@0@-symbolic.svg'.format(base_id),
  output: '@0@-symbolic.svg'.format(application_id),
  copy: true,
)
