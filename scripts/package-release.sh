#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

version="$(python3 -c 'import json; print(json.load(open("manifest.json"))["Version"])')"
code_path="$(python3 -c 'import json; print(json.load(open("manifest.json"))["CodePathLin"])')"
plugin_name="com.victormarin.volume-controller.sdPlugin"
artifact_name="opendeck-volume-dial-controller-v${version}-linux-x86_64.zip"
dist_dir="$repo_dir/dist"
mkdir -p "$dist_dir"
stage_parent="$(mktemp -d "$dist_dir/.package-stage.XXXXXX")"
stage_dir="$stage_parent/$plugin_name"
trap 'rm -rf "$stage_parent"' EXIT

command -v python3 >/dev/null
command -v sha256sum >/dev/null

cargo build --release
mkdir -p "$stage_dir"

source_binary="$repo_dir/target/release/oa-volume-controller"
test -x "$source_binary"
install -m 0755 "$source_binary" "$stage_dir/$code_path"

for file in manifest.json LICENSE README.md ATTRIBUTION.md THIRD_PARTY_LICENSES.md CHANGELOG.md; do
    test -f "$file"
    install -m 0644 "$file" "$stage_dir/$file"
done

while IFS= read -r path; do
    test -n "$path" || continue
    if test -f "$path"; then
        install -m 0644 "$path" "$stage_dir/$path"
    elif test -f "$path.png"; then
        mkdir -p "$stage_dir/$(dirname "$path")"
        install -m 0644 "$path.png" "$stage_dir/$path.png"
    elif test -f "$path.svg"; then
        mkdir -p "$stage_dir/$(dirname "$path")"
        install -m 0644 "$path.svg" "$stage_dir/$path.svg"
    else
        echo "Missing manifest-referenced path: $path" >&2
        exit 1
    fi
done < <(
    python3 - <<'PY'
import json
m = json.load(open("manifest.json"))
paths = [m.get("PropertyInspectorPath"), m.get("Icon")]
for action in m.get("Actions", []):
    paths.extend((action.get("PropertyInspectorPath"), action.get("Icon")))
    paths.extend(state.get("Image") for state in action.get("States", []))
for path in dict.fromkeys(filter(None, paths)):
    print(path)
PY
)

test -d img
cp -a img "$stage_dir/"
# The retained upstream README screenshot is source documentation, not a
# runtime plugin asset.
rm -f "$stage_dir/img/readme.png"
if test -d pi; then
    cp -a pi "$stage_dir/"
fi

python3 -m json.tool "$stage_dir/manifest.json" >/dev/null
test -x "$stage_dir/$code_path"

while IFS= read -r path; do
    test -n "$path" || continue
    if ! test -e "$stage_dir/$path" \
        && ! test -e "$stage_dir/$path.png" \
        && ! test -e "$stage_dir/$path.svg"; then
        echo "Staged manifest path is missing: $path" >&2
        exit 1
    fi
done < <(
    python3 - <<'PY'
import json
m = json.load(open("manifest.json"))
paths = [m.get("PropertyInspectorPath"), m.get("Icon")]
for action in m.get("Actions", []):
    paths.extend((action.get("PropertyInspectorPath"), action.get("Icon")))
    paths.extend(state.get("Image") for state in action.get("States", []))
for path in dict.fromkeys(filter(None, paths)):
    print(path)
PY
)

archive="$dist_dir/$artifact_name"
checksum="$archive.sha256"
rm -f "$archive" "$checksum"
STAGE_PARENT="$stage_parent" ARCHIVE="$archive" python3 - <<'PY'
import os
import pathlib
import zipfile

stage = pathlib.Path(os.environ["STAGE_PARENT"])
archive = pathlib.Path(os.environ["ARCHIVE"])
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as output:
    for path in sorted(stage.rglob("*")):
        relative = path.relative_to(stage).as_posix()
        if path.is_dir():
            info = zipfile.ZipInfo(relative + "/")
            info.external_attr = (0o40755 << 16) | 0x10
            output.writestr(info, b"")
            continue
        info = zipfile.ZipInfo.from_file(path, relative)
        mode = 0o100755 if os.access(path, os.X_OK) else 0o100644
        info.external_attr = mode << 16
        with path.open("rb") as source:
            output.writestr(info, source.read(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
PY
(
    cd "$dist_dir"
    sha256sum "$artifact_name" > "$artifact_name.sha256"
)

size="$(stat -c '%s' "$archive")"
digest="$(sha256sum "$archive" | awk '{print $1}')"
echo "Package: $archive"
echo "Size: $size bytes"
echo "SHA-256: $digest"
echo "Checksum: $checksum"
