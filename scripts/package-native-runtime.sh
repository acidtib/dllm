#!/usr/bin/env bash
#
# Stage the GGML_BACKEND_DL shared objects produced by a backend-dl build into
# the layout the universal artifact loads at runtime: a flat directory of
# backend objects plus a backends.json manifest (RuntimeManifest) that records
# each file's SHA-256 and backend family.
#
# The build itself is done elsewhere (a llama-cpp-sys-4 backend-dl build, or a
# direct llama.cpp GGML_BACKEND_DL build). This script only classifies, hashes,
# and records the resulting objects, so it stays platform-agnostic and does not
# depend on a GPU being present.
#
# Usage:
#   scripts/package-native-runtime.sh --lib-dir DIR --out DIR --version STR
#
# Classification:
#   core    libggml-base, libggml, libllama, libmtmd
#   cpu     libggml-cpu-*
#   cuda    libggml-cuda, libnccl.so.* (packaged next to the CUDA backend)
#   vulkan  libggml-vulkan
#   metal   libggml-metal
#
# Objects that do not match any rule are ignored and listed on stderr so a
# missing or misnamed backend is visible rather than silently dropped.

set -euo pipefail

lib_dir=""
out_dir=""
version=""

while [ $# -gt 0 ]; do
  case "$1" in
    --lib-dir) lib_dir="$2"; shift 2 ;;
    --out) out_dir="$2"; shift 2 ;;
    --version) version="$2"; shift 2 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$lib_dir" ] || [ -z "$out_dir" ] || [ -z "$version" ]; then
  echo "usage: $0 --lib-dir DIR --out DIR --version STR" >&2
  exit 2
fi
if [ ! -d "$lib_dir" ]; then
  echo "lib-dir not found: $lib_dir" >&2
  exit 1
fi

mkdir -p "$out_dir"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# Family for a base file name, or empty if it is not a runtime object.
classify() {
  case "$1" in
    libggml-base.*|libggml.so*|libggml.dylib|libllama.*|libmtmd.*) echo core ;;
    libggml-cpu-*) echo cpu ;;
    libggml-cuda.*|libnccl.so*) echo cuda ;;
    libggml-vulkan.*) echo vulkan ;;
    libggml-metal.*) echo metal ;;
    *) echo "" ;;
  esac
}

# Collect "family<TAB>file<TAB>sha256" rows.
rows=""
for path in "$lib_dir"/*; do
  [ -f "$path" ] || continue
  name="$(basename "$path")"
  family="$(classify "$name")"
  if [ -z "$family" ]; then
    echo "ignoring unrecognized file: $name" >&2
    continue
  fi
  cp "$path" "$out_dir/$name"
  sum="$(sha256_of "$out_dir/$name")"
  rows="${rows}${family}	${name}	${sum}
"
done

if [ -z "$rows" ]; then
  echo "no runtime objects found in $lib_dir" >&2
  exit 1
fi

# Emit backends.json in RuntimeManifest shape. Group files by family; core is a
# flat array, the rest go under backends[].
emit_files() {
  local family="$1"
  printf '%s' "$rows" | while IFS='	' read -r fam file sum; do
    [ "$fam" = "$family" ] || continue
    printf '    { "file": "%s", "sha256": "%s" },\n' "$file" "$sum"
  done | sed '$ s/,$//'
}

backend_families="$(printf '%s' "$rows" | awk -F '\t' '$1 != "core" { print $1 }' | sort -u)"

manifest="$out_dir/backends.json"
{
  printf '{\n'
  printf '  "version": "%s",\n' "$version"
  printf '  "core": [\n'
  emit_files core
  printf '  ],\n'
  printf '  "backends": [\n'
  first_family=1
  for family in $backend_families; do
    if [ "$first_family" -eq 0 ]; then printf ',\n'; fi
    first_family=0
    printf '    {\n'
    printf '      "kind": "%s",\n' "$family"
    printf '      "files": [\n'
    emit_files "$family" | sed 's/^/    /'
    printf '\n      ]\n'
    printf '    }'
  done
  printf '\n  ]\n'
  printf '}\n'
} > "$manifest"

echo "wrote $manifest"
echo "staged $(printf '%s' "$rows" | grep -c . ) objects into $out_dir"
