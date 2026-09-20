scalable_dir = 'hicolor' / 'scalable' / 'apps'
symbolic_dir = 'hicolor' / 'symbolic' / 'apps'

install_data(
  scalable_dir / '@0@.svg'.format(base_id),
  install_dir: iconsdir / scalable_dir,
  rename: '@0@.svg'.format(application_id),
)
# Unthemed copy in build/data/icons so the app finds it when run uninstalled.
configure_file(
  input: scalable_dir / '@0@.svg'.format(base_id),
  output: '@0@.svg'.format(application_id),
  copy: true,
)

# Symbolic icons: the app icon and the tray states.
foreach suffix : ['-symbolic', '-connected-symbolic']
  install_data(
    symbolic_dir / '@0@@1@.svg'.format(base_id, suffix),
    install_dir: iconsdir / symbolic_dir,
    rename: '@0@@1@.svg'.format(application_id, suffix),
  )
  configure_file(
    input: symbolic_dir / '@0@@1@.svg'.format(base_id, suffix),
    output: '@0@@1@.svg'.format(application_id, suffix),
    copy: true,
  )
endforeach
