#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$project_dir"
if [ -x "$HOME/.cargo/bin/cargo" ]; then
    cargo_bin="$HOME/.cargo/bin/cargo"
else
    cargo_bin=cargo
fi
"$cargo_bin" build --locked --release -j32
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
dpkg --validate-version "$version"
architecture=$(dpkg --print-architecture)
mkdir -p target dist
build_dir=$(mktemp -d "$project_dir/target/deb.XXXXXX")
trap 'rm -rf -- "$build_dir"' EXIT HUP INT TERM
package_root="$build_dir/root"
docs="$package_root/usr/share/doc/zterm-r"
install -d "$package_root/DEBIAN" "$package_root/usr/bin" "$docs" \
    "$package_root/usr/share/applications" \
    "$package_root/usr/share/icons/hicolor/scalable/apps" "$build_dir/debian"
install -m 755 target/release/zterm-r "$package_root/usr/bin/zterm-r"
install -m 644 packaging/local.zterm.r.desktop "$package_root/usr/share/applications/"
install -m 644 workbench.svg "$package_root/usr/share/icons/hicolor/scalable/apps/local.zterm.r.svg"
install -m 644 LICENSE "$docs/copyright"
install -m 644 README.md THEME_SOURCES.txt "$docs/"
gzip -n -9 -c RELEASE_NOTES.txt > "$docs/changelog.gz"

# Preserve license/notice files for the Rust dependencies linked into the binary.
"$cargo_bin" metadata --locked --offline --format-version 1 > "$build_dir/metadata.json"
python3 - "$build_dir/metadata.json" "$docs" "$project_dir" <<'PY'
import json
from pathlib import Path
import shutil
import sys
metadata, docs, project = map(Path, sys.argv[1:])
lines = []
for package in json.loads(metadata.read_text())["packages"]:
    source = Path(package["manifest_path"]).parent
    if source == project:
        continue
    label = f'{package["name"]}-{package["version"]}'
    lines.append(f'{label}: {package.get("license") or "see included license file"}')
    files = {path for pattern in ("LICENSE*", "COPYING*", "NOTICE*")
             for path in source.glob(pattern) if path.is_file()}
    if package.get("license_file"):
        files.add(source / package["license_file"])
    for path in files:
        destination = docs / "licenses" / label / path.name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
(docs / "THIRD_PARTY_LICENSES.txt").write_text("\n".join(sorted(lines)) + "\n")
PY

cat > "$build_dir/debian/control" <<'CONTROL'
Source: zterm-r
Section: utils
Priority: optional
Maintainer: zhou411X <zhou411X@users.noreply.github.com>

Package: zterm-r
Architecture: any
Description: Native GTK4 terminal and SSH workspace
CONTROL
depends=$(cd "$build_dir" && dpkg-shlibdeps -O -e"$package_root/usr/bin/zterm-r" | sed -n 's/^shlibs:Depends=//p')
test -n "$depends"
installed_size=$(du -sk "$package_root/usr" | cut -f1)
cat > "$package_root/DEBIAN/control" <<CONTROL
Package: zterm-r
Version: $version
Architecture: $architecture
Section: utils
Priority: optional
Maintainer: zhou411X <zhou411X@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $depends, openssh-client
Recommends: gnome-keyring
Homepage: https://github.com/zhou411X/zterm-r
Description: Native GTK4 terminal and SSH workspace
 Rust terminal with grouped SSH sessions, SFTP transfers, live appearance
 previews and 30 built-in color schemes. Runs alongside the Python zterm app.
CONTROL
artifact="zterm-r_${version}_${architecture}.deb"
dpkg-deb --root-owner-group --build "$package_root" "$project_dir/dist/$artifact"
(cd dist && sha256sum "$artifact" > SHA256SUMS)
printf 'Built: %s/dist/%s\n' "$project_dir" "$artifact"
