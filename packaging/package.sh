#!/usr/bin/env bash
# Build release binaries and pack .deb and .rpm for each requested target.
# With no arguments, builds x86_64 and aarch64. Filenames have no version so a
# tag release matches the download page. The package version still comes from
# the tag, which must match Cargo.toml. Every other build uses <version>~ci.<sha>
# inside the package and is not a release.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm is required (https://github.com/goreleaser/nfpm)" >&2
  exit 1
fi
if ! command -v go >/dev/null 2>&1; then
  echo "go is required to bundle hypermesh-cli" >&2
  exit 1
fi

cli_src="${HYPERMESH_CLI_SRC:-$root/third_party/hypermesh-cli}"
if [[ ! -f "$cli_src/go.mod" ]]; then
  echo "hypermesh-cli sources not found at $cli_src" >&2
  exit 1
fi

cargo_version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
if [[ -z "$cargo_version" ]]; then
  echo "Cargo.toml has no version" >&2
  exit 1
fi

if [[ -n "${VERSION:-}" ]]; then
  version="$VERSION"
elif [[ "${GITHUB_REF:-}" == refs/tags/v* ]]; then
  version="${GITHUB_REF#refs/tags/v}"
  if [[ "$version" != "$cargo_version" ]]; then
    echo "tag v${version} does not match Cargo.toml version ${cargo_version}" >&2
    exit 1
  fi
else
  if [[ -n "${GITHUB_SHA:-}" ]]; then
    sha="${GITHUB_SHA:0:12}"
  else
    sha=$(git rev-parse --short=12 HEAD)
  fi
  version="${cargo_version}~ci.${sha}"
fi

if [[ "$version" == *[/[:space:]]* || -z "$version" ]]; then
  echo "refusing version ${version@Q}" >&2
  exit 1
fi

if [[ $# -gt 0 ]]; then
  targets=("$@")
else
  targets=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
fi

case "$(uname -m)" in
  x86_64) host_target=x86_64-unknown-linux-gnu ;;
  aarch64 | arm64) host_target=aarch64-unknown-linux-gnu ;;
  *)
    echo "unsupported host $(uname -m)" >&2
    exit 1
    ;;
esac

mkdir -p dist
find dist -mindepth 1 -delete

for target in "${targets[@]}"; do
  case "$target" in
    x86_64-unknown-linux-gnu)
      deb_arch=amd64
      rpm_arch=x86_64
      asset_arch=x86_64
      linker_var=CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER
      linker_bin=x86_64-linux-gnu-gcc
      elf_needle="x86-64"
      ;;
    aarch64-unknown-linux-gnu)
      deb_arch=arm64
      rpm_arch=aarch64
      asset_arch=aarch64
      linker_var=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER
      linker_bin=aarch64-linux-gnu-gcc
      elf_needle="ARM aarch64"
      ;;
    *)
      echo "unsupported target $target" >&2
      exit 1
      ;;
  esac

  if [[ "$target" != "$host_target" ]]; then
    if ! command -v "$linker_bin" >/dev/null 2>&1; then
      echo "cross linker $linker_bin is required to build $target" >&2
      exit 1
    fi
    export "${linker_var}=${linker_bin}"
  fi

  rustup target add "$target" >/dev/null
  cargo build --release --locked --target "$target" --workspace

  built="target/${target}/release/hypermesh-visor"
  companion="target/${target}/release/hypermesh-companion"
  raw="dist/hypermesh-visor-${asset_arch}"
  install -m 0755 "$built" "$raw"
  if [[ ! -x "$companion" ]]; then
    echo "missing companion binary $companion" >&2
    exit 1
  fi

  case "$asset_arch" in
    x86_64) goarch=amd64 ;;
    aarch64) goarch=arm64 ;;
  esac
  cli_bin="target/nfpm/hypermesh-${asset_arch}"
  hm_bin="target/nfpm/hm-${asset_arch}"
  mkdir -p target/nfpm
  (
    cd "$cli_src"
    CGO_ENABLED=0 GOOS=linux GOARCH="$goarch" go build -mod=readonly -trimpath -o "$root/$cli_bin" ./cmd/hypermesh
    CGO_ENABLED=0 GOOS=linux GOARCH="$goarch" go build -mod=readonly -trimpath -o "$root/$hm_bin" ./cmd/hm
  )

  if ! file -b "$raw" | grep -q "$elf_needle"; then
    echo "$raw is not a $elf_needle ELF" >&2
    file -b "$raw" >&2
    exit 1
  fi

  deb="dist/hypermesh-visor-${asset_arch}.deb"
  rpm="dist/hypermesh-visor-${asset_arch}.rpm"
  VISOR_BIN="$raw" COMPANION_BIN="$companion" CLI_BIN="$cli_bin" HM_BIN="$hm_bin" \
    VERSION="$version" NFPM_ARCH="$deb_arch" \
    nfpm package --config packaging/nfpm.yaml --packager deb --target "$deb"
  VISOR_BIN="$raw" COMPANION_BIN="$companion" CLI_BIN="$cli_bin" HM_BIN="$hm_bin" \
    VERSION="$version" NFPM_ARCH="$deb_arch" \
    nfpm package --config packaging/nfpm.yaml --packager rpm --target "$rpm"

  got_deb_arch=$(dpkg-deb -f "$deb" Architecture)
  got_deb_name=$(dpkg-deb -f "$deb" Package)
  got_deb_home=$(dpkg-deb -f "$deb" Homepage)
  if [[ "$got_deb_arch" != "$deb_arch" || "$got_deb_name" != "hypermesh-visor" || "$got_deb_home" != "https://github.com/FyberLabs/hypermesh-visor" ]]; then
    echo "unexpected deb metadata for $deb: $got_deb_name $got_deb_arch $got_deb_home" >&2
    exit 1
  fi
  listing=$(dpkg-deb -c "$deb")
  for path in \
    './usr/bin/hypermesh-visor' \
    './usr/bin/hypermesh-companion' \
    './usr/bin/hypermesh' \
    './usr/bin/hm' \
    './etc/xdg/autostart/hypermesh-companion.desktop'
  do
    if ! grep -q " ${path}\$" <<<"$listing"; then
      echo "$deb does not install ${path#./}" >&2
      exit 1
    fi
  done

  if command -v rpm >/dev/null 2>&1; then
    got_rpm=$(rpm -qp --queryformat '%{NAME} %{ARCH} %{URL}' "$rpm")
    if [[ "$got_rpm" != "hypermesh-visor ${rpm_arch} https://github.com/FyberLabs/hypermesh-visor" ]]; then
      echo "unexpected rpm metadata for $rpm: $got_rpm" >&2
      exit 1
    fi
  fi
done

(
  cd dist
  mapfile -t files < <(find . -maxdepth 1 -type f ! -name checksums.txt -printf '%P\n' | sort)
  sha256sum -- "${files[@]}" > checksums.txt
)

echo "packed ${version} into dist/"
ls -1 dist
