#!/usr/bin/env bash
# Build Bobcat installers.
#   ./build.sh --target mac [--arch arm64|x64|universal]
#   ./build.sh --target linux [--package deb|rpm|appimage|all]   (comma list allowed, e.g. deb,rpm; default: the distribution's own
#                                                                 package, deb on Debian/Ubuntu, rpm on RHEL/Fedora/Alma/Rocky/SUSE, else appimage;
#                                                                 --target deb / --target rpm = linux with that package only)
#   ./build.sh --target linux --docker [--arch x64|arm64] [--package ...]   (Linux packages in a Docker container, e.g. on a Mac: the
#                                                                 image is docker/linux/Dockerfile (Ubuntu 24.04), --arch picks the
#                                                                 container platform, default x64 = linux/amd64; this is the default
#                                                                 for --target linux on a host that is not Linux)
#   ./build.sh --target windows      (on Windows, e.g. in Git Bash; on Linux it builds in a Windows 11 VM, see windows/vm.py:
#                                     needs docker and /dev/kvm, run `python3 windows/vm.py up` once first)
#   ./build.sh [--target <t>] --release [--draft] [--yes] [--dry-run]
#   ./build.sh --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]
# A target can only be built on its own platform (the GUI links the system web view).
# Results are copied to ./dist/. Needs the Rust toolchain and tauri-cli
# (cargo install tauri-cli --version '^2'). License: CC BY-NC-SA 4.0 (LICENSE.md).
#
# --release tags the commit v<version> (version from gui/src-tauri/Cargo.toml), pushes the tag and
# creates the GitHub release with the installers in dist/ for that version (needs `gh`, logged in).
# The release notes are that version's section of CHANGELOG.md. If the release already exists the
# files are added to it, so each platform can be built and added on its own machine. It never
# commits: the working tree must already be committed and pushed. Without --target it only releases
# what is already in dist/. --draft creates a draft release, --yes skips the question,
# --dry-run prints what it would do and changes nothing.
#
# --delete-release deletes the GitHub release v<version> (default: the current version) with its uploaded
# files, and the tag v<version> on GitHub and locally unless --keep-tag is given. It asks first (--yes skips
# the question). It never touches commits or dist/.
set -euo pipefail

usage() {
  echo "usage: $0 --target mac|linux|deb|rpm|windows [--arch arm64|x64|universal] [--package deb|rpm|appimage|all] [--docker] [--no-cli]"
  echo "       $0 [--target <t>] --release [--draft] [--yes] [--dry-run]"
  echo "       $0 --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]"
  exit "${1:-1}"
}

target="" arch="" package="" docker=0 cli=1 delete=0 del_version="" keep_tag=0 release=0 draft=0 yes=0 dry=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --arch) arch="${2:-}"; shift 2 ;;
    --package|--format) package="${2:-}"; shift 2 ;;
    --docker) docker=1; shift ;;
    --no-cli) cli=0; shift ;;
    --release) release=1; shift ;;
    --delete-release)
      delete=1; shift
      case "${1:-}" in -*|"") ;; *) del_version="${1#v}"; shift ;;
      esac ;;
    --keep-tag) keep_tag=1; shift ;;
    --draft) draft=1; shift ;;
    --yes) yes=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[ -n "$target" ] || [ "$release" = 1 ] || [ "$delete" = 1 ] || usage
[ "$delete" = 0 ] || { [ "$release" = 0 ] && [ -z "$target" ]; } || { echo "--delete-release cannot be combined with --release or --target" >&2; exit 1; }

root=$(cd "$(dirname "$0")" && pwd)
dist="$root/dist"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/gui/src-tauri/Cargo.toml" | head -1)

# The package format of the Linux distribution this runs on (/etc/os-release ID and ID_LIKE).
native_package() {
  local ids=""
  [ -r /etc/os-release ] && ids=$(. /etc/os-release; echo "${ID:-} ${ID_LIKE:-}")
  case " $ids " in
    *" debian "*|*" ubuntu "*) echo deb ;;
    *" rhel "*|*" fedora "*|*" centos "*|*" almalinux "*|*" rocky "*|*" suse "*|*" opensuse "*|*" opensuse-leap "*|*" sles "*|*" amzn "*|*" ol "*) echo rpm ;;
    *) echo appimage ;;
  esac
}

# Windows from a Linux host: the installer and bobcat-cli are built in the Windows 11 VM (windows/vm.py)
windows_vm() {
  command -v python3 >/dev/null || { echo "python3 not found (windows/vm.py needs it)" >&2; exit 1; }
  vmrc=0
  python3 -I "$root/windows/vm.py" build || vmrc=$?
  mkdir -p "$dist"
  found=0
  for f in "$root"/windows/out/bundle/nsis/*-setup.exe; do
    [ -f "$f" ] || continue
    cp "$f" "$dist/"; echo "  $dist/$(basename "$f")"; found=1
  done
  [ "$found" = 1 ] || { echo "no installer found under windows/out/bundle/nsis" >&2; exit 1; }
  if [ "$cli" = 1 ] && [ -f "$root/windows/out/bundle/cli/bobcat-cli.exe" ]; then
    cp "$root/windows/out/bundle/cli/bobcat-cli.exe" "$dist/bobcat-cli-$version-windows.exe"
    echo "  $dist/bobcat-cli-$version-windows.exe"
  fi
  if [ "$vmrc" != 0 ]; then
    echo "the Windows build failed (see windows/out/build.log); the files above are what was built before it failed" >&2
    exit "$vmrc"
  fi
  echo "done: $dist"
}

# Linux packages in a container (a Mac, or any host with Docker). The repo is mounted at /src; target
# directories live in Docker volumes so they never mix with the host's own builds; `build.sh` runs inside
# on Linux and writes to dist/.
linux_docker() {
  command -v docker >/dev/null || { echo "docker not found (needed for --docker, and for --target linux on a host that is not Linux)" >&2; exit 1; }
  docker info >/dev/null 2>&1 || { echo "the Docker daemon is not running" >&2; exit 1; }
  local plat tag
  case "${arch:-x64}" in
    x64|amd64|x86_64) plat=linux/amd64; tag=amd64 ;;
    arm64|aarch64) plat=linux/arm64; tag=arm64 ;;
    *) echo "unknown --arch for Linux: $arch (x64, arm64)" >&2; exit 1 ;;
  esac
  echo "== Linux build in Docker ($plat) =="
  docker build --platform "$plat" -t "bobcat-linux-build:$tag" "$root/docker/linux"
  mkdir -p "$dist"
  local inner=(./build.sh --target linux)
  [ -z "$package" ] || inner+=(--package "$package")
  [ "$cli" = 1 ] || inner+=(--no-cli)
  docker run --rm --platform "$plat" \
    -v "$root":/src \
    -v "bobcat-target-$tag":/src/target \
    -v "bobcat-gui-target-$tag":/src/gui/src-tauri/target \
    -v "bobcat-cargo-$tag":/opt/cargo/registry \
    -e BOBCAT_LINUX_ARCH="$tag" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    "bobcat-linux-build:$tag" \
    sh -c '"$@"; rc=$?; chown -R "$HOST_UID:$HOST_GID" /src/dist /src/Cargo.lock /src/gui/src-tauri/Cargo.lock 2>/dev/null; exit $rc' sh "${inner[@]}"
  echo "done: $dist"
}

build_target() {
  host=$(uname -s)
  case "$target" in deb|rpm) package="$target"; target=linux ;; esac
  case "$target" in
    mac|macos)   want=Darwin; bundles=dmg ;;
    linux)       want=Linux;  bundles=$(native_package) ;;
    windows|win) want=MINGW;  bundles=nsis ;;
    *) echo "unknown target: $target (mac, linux, deb, rpm, windows)" >&2; exit 1 ;;
  esac
  if [ -n "$package" ] && [ "$target" != linux ]; then
    echo "--package only applies to --target linux" >&2; exit 1
  fi
  if [ "$target" = linux ] && [ -n "$package" ]; then
    bundles=""
    for p in ${package//,/ }; do
      case "$p" in
        all) bundles="deb,rpm,appimage"; break ;;
        deb|rpm|appimage) bundles="${bundles:+$bundles,}$p" ;;
        *) echo "unknown --package: $p (deb, rpm, appimage, all)" >&2; exit 1 ;;
      esac
    done
  fi
  if [ "$target" = linux ] && { [ "$docker" = 1 ] || [ "$host" != Linux ]; }; then
    linux_docker; return
  fi
  if [ "$target" = linux ]; then
    case ",$bundles," in
      *,rpm,*) command -v rpmbuild >/dev/null || { echo "rpmbuild not found: install it (Debian/Ubuntu: apt install rpm; Fedora: dnf install rpm-build)" >&2; exit 1; } ;;
    esac
  fi
  if [ "$target" = windows ] || [ "$target" = win ]; then
    if [ "$host" = Linux ]; then windows_vm; return; fi
  fi
  case "$host" in
    $want*) ;;
    *) echo "the $target installer must be built on $target (this is $host)" >&2; exit 1 ;;
  esac

  command -v cargo >/dev/null || { echo "cargo not found: install Rust (https://rustup.rs)" >&2; exit 1; }
  cargo tauri --version >/dev/null 2>&1 || { echo "tauri-cli not found: cargo install tauri-cli --version '^2'" >&2; exit 1; }

  triple_args=()
  if [ "$target" = mac ] || [ "$target" = macos ]; then
    case "${arch:-native}" in
      native) ;;
      arm64) triple_args=(--target aarch64-apple-darwin) ;;
      x64) triple_args=(--target x86_64-apple-darwin) ;;
      universal) triple_args=(--target universal-apple-darwin) ;;
      *) echo "unknown --arch: $arch" >&2; exit 1 ;;
    esac
    if [ -n "${triple_args[1]:-}" ]; then
      # make sure the Rust standard library for the requested architecture is installed
      case "${triple_args[1]}" in
        universal-apple-darwin) rustup target add aarch64-apple-darwin x86_64-apple-darwin ;;
        *) rustup target add "${triple_args[1]}" ;;
      esac
    fi
  elif [ -n "$arch" ]; then
    echo "--arch only applies to --target mac (or --target linux with --docker)" >&2; exit 1
  fi
  [ "$docker" = 0 ] || [ "$target" = linux ] || { echo "--docker only applies to --target linux" >&2; exit 1; }

  mkdir -p "$dist"
  echo "== building the GUI installer ($target, $bundles) =="
  (cd "$root/gui/src-tauri" && cargo tauri build --bundles "$bundles" --features vendored ${triple_args[@]+"${triple_args[@]}"})

  # Tauri puts bundles under target/[<triple>/]release/bundle/<kind>/ (the GUI crate has its own target dir)
  found=0
  while IFS= read -r f; do
    cp -R "$f" "$dist/"
    echo "  $dist/$(basename "$f")"
    found=1
  done < <(find "$root/gui/src-tauri/target" -path '*/release/bundle/*' -type f \
    \( -name '*.dmg' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' -o -name '*-setup.exe' \) -newer "$root/gui/src-tauri/Cargo.toml" 2>/dev/null)
  [ "$found" = 1 ] || { echo "no installer found under gui/src-tauri/target" >&2; exit 1; }

  if [ "$cli" = 1 ]; then
    echo "== building bobcat-cli =="
    (cd "$root" && cargo build --release --features vendored ${triple_args[@]+"${triple_args[@]}"})
    bin=$(find "$root/target" -path '*/release/bobcat-cli*' -type f ! -name '*.d' -perm -u+x -newer "$root/Cargo.toml" | head -1)
    if [ -n "$bin" ]; then
      # an arm64 Docker build is named apart from the x64 one (x64 keeps the plain name)
      suffix=""; [ "${BOBCAT_LINUX_ARCH:-}" != arm64 ] || suffix="-arm64"
      cp "$bin" "$dist/bobcat-cli-$version-$target$suffix"
      echo "  $dist/bobcat-cli-$version-$target$suffix"
    fi
  fi
  echo "done: $dist"
}

release_it() {
  die() { echo "release: $*" >&2; exit 1; }
  # run a command, or only show it with --dry-run
  run() { if [ "$dry" = 1 ]; then echo "  [dry-run] $*"; else "$@"; fi; }
  # a problem stops a real release; a dry run only reports it
  problem() { if [ "$dry" = 1 ]; then echo "  [dry-run] would stop: $*"; else die "$*"; fi; }

  command -v gh >/dev/null || die "gh (the GitHub CLI) is not installed"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
  local tag="v$version"
  shopt -s nullglob
  local assets=("$dist"/*_"$version"_* "$dist"/*-"$version"-*)
  shopt -u nullglob
  [ "${#assets[@]}" -gt 0 ] || die "no installers for $version in $dist (build first: $0 --target <t>)"

  cd "$root"
  [ -z "$(git status --porcelain)" ] || problem "uncommitted changes: commit them first (this script never commits)"
  git fetch -q origin 2>/dev/null || true
  local upstream
  upstream=$(git rev-parse --abbrev-ref '@{u}' 2>/dev/null) || upstream=""
  if [ -z "$upstream" ]; then
    problem "this branch has no upstream: push it first"
  elif [ "$(git rev-parse HEAD)" != "$(git rev-parse '@{u}')" ]; then
    problem "HEAD is not the same as $upstream: push (or pull) first"
  fi
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null && [ "$(git rev-list -n1 "$tag")" != "$(git rev-parse HEAD)" ]; then
    problem "tag $tag already exists on another commit"
  fi

  local notes
  notes=$(mktemp)
  awk -v ver="$version" 'index($0, "## [" ver "]") == 1 { p = 1; next } /^## \[/ { p = 0 } p' "$root/CHANGELOG.md" > "$notes"
  local notes_args=(--notes-file "$notes")
  if [ ! -s "$notes" ]; then
    echo "  CHANGELOG.md has no section for $version: GitHub will generate the notes"
    notes_args=(--generate-notes)
  fi

  local exists=0
  gh release view "$tag" >/dev/null 2>&1 && exists=1
  echo "== release $tag =="
  echo "  commit:  $(git rev-parse --short HEAD) on $(git rev-parse --abbrev-ref HEAD)"
  echo "  files:"
  printf '    %s\n' "${assets[@]##*/}"
  if [ "$exists" = 1 ]; then echo "  the release exists: the files above are added to it"; else echo "  a new release will be created$([ "$draft" = 1 ] && echo ' (draft)')"; fi
  if [ "$yes" != 1 ] && [ "$dry" != 1 ]; then
    read -r -p "Tag, push and publish? [y/N] " ans
    case "$ans" in y|Y|yes) ;; *) echo "stopped"; exit 1 ;; esac
  fi

  git rev-parse -q --verify "refs/tags/$tag" >/dev/null || run git tag -a "$tag" -m "Bobcat $version"
  run git push origin "$tag"
  if [ "$exists" = 1 ]; then
    run gh release upload "$tag" "${assets[@]}" --clobber
  else
    local draft_args=()
    [ "$draft" = 1 ] && draft_args=(--draft)
    run gh release create "$tag" "${assets[@]}" --title "Bobcat $version" "${notes_args[@]}" ${draft_args[@]+"${draft_args[@]}"}
  fi
  rm -f "$notes"
  echo "release done: $(gh release view "$tag" --json url -q .url 2>/dev/null || echo "$tag")"
}

delete_release() {
  die() { echo "delete-release: $*" >&2; exit 1; }
  run() { if [ "$dry" = 1 ]; then echo "  [dry-run] $*"; else "$@"; fi; }
  command -v gh >/dev/null || die "gh (the GitHub CLI) is not installed"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
  local tag="v${del_version:-$version}"
  cd "$root"
  local exists=0 remote_tag=0 local_tag=0
  gh release view "$tag" >/dev/null 2>&1 && exists=1
  git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 && remote_tag=1
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null && local_tag=1
  [ "$exists" = 1 ] || [ "$remote_tag" = 1 ] || [ "$local_tag" = 1 ] || die "no release or tag $tag found"
  echo "== delete release $tag =="
  [ "$exists" = 1 ] && echo "  GitHub release: $(gh release view "$tag" --json url -q .url) (with its files)" || echo "  no GitHub release"
  if [ "$keep_tag" = 1 ]; then
    echo "  the tag is kept"
  else
    echo "  tag on GitHub: $([ "$remote_tag" = 1 ] && echo yes || echo no), local tag: $([ "$local_tag" = 1 ] && echo yes || echo no)"
  fi
  if [ "$yes" != 1 ] && [ "$dry" != 1 ]; then
    read -r -p "Delete? This cannot be undone. [y/N] " ans
    case "$ans" in y|Y|yes) ;; *) echo "stopped"; exit 1 ;; esac
  fi
  if [ "$exists" = 1 ]; then
    if [ "$keep_tag" = 1 ]; then run gh release delete "$tag" --yes; else run gh release delete "$tag" --yes --cleanup-tag; fi
  fi
  if [ "$keep_tag" != 1 ]; then
    [ "$exists" = 1 ] || [ "$remote_tag" = 0 ] || run git push origin ":refs/tags/$tag"
    [ "$local_tag" = 0 ] || run git tag -d "$tag"
  fi
  echo "delete done: $tag"
}

if [ "$delete" = 1 ]; then delete_release; exit 0; fi
if [ -n "$target" ]; then build_target; fi
if [ "$release" = 1 ]; then release_it; fi
