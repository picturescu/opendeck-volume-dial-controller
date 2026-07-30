#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugin_name="com.victormarin.volume-controller.sdPlugin"
install_root="${XDG_CONFIG_HOME:-$HOME/.config}/opendeck/plugins"
destination="$install_root/$plugin_name"

if (($# > 1)); then
    echo "Usage: $0 [release.zip]" >&2
    exit 2
fi

if (($# == 1)); then
    archive="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
else
    "$repo_dir/scripts/package-release.sh"
    version="$(python3 -c 'import json; print(json.load(open("'"$repo_dir"'/manifest.json"))["Version"])')"
    archive="$repo_dir/dist/opendeck-volume-dial-controller-v${version}-linux-x86_64.zip"
fi

test -f "$archive"
command -v unzip >/dev/null
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
unzip -q "$archive" -d "$temporary"

source_dir="$temporary/$plugin_name"
test -d "$source_dir"
python3 -m json.tool "$source_dir/manifest.json" >/dev/null
code_path="$(python3 -c 'import json; print(json.load(open("'"$source_dir"'/manifest.json"))["CodePathLin"])')"
test -x "$source_dir/$code_path"

mkdir -p "$install_root"
if test -e "$destination"; then
    timestamp="$(date +%Y%m%d-%H%M%S)"
    backup="${destination}.backup-${timestamp}"
    mv "$destination" "$backup"
    echo "Existing plugin backed up to: $backup"
fi

cp -a "$source_dir" "$destination"
test -x "$destination/$code_path"
echo "Installed: $destination"
echo "Restart OpenDeck to load the plugin."
