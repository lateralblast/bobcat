#!/usr/bin/env bash
# Build Bobcat installers.
#   ./build.sh --target mac [--arch arm64|x64|universal]
#   ./build.sh --target linux [--package deb|rpm|appimage|all]   (comma list allowed, e.g. deb,rpm; default: the distribution's own
#                                                                 package, deb on Debian/Ubuntu, rpm on RHEL/Fedora/Alma/Rocky/SUSE, else appimage;
#                                                                 --target deb / --target rpm = linux with that package only)
#   ./build.sh --target windows      (run on Windows, e.g. in Git Bash)
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
  echo "usage: $0 --target mac|linux|deb|rpm|windows [--arch arm64|x64|universal] [--package deb|rpm|appimage|all] [--no-cli]"
  echo "       $0 [--target <t>] --release [--draft] [--yes] [--dry-run]"
  echo "       $0 --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]"
  exit "${1:-1}"
}

target="" arch="" package="" cli=1 delete=0 del_version="" keep_tag=0 release=0 draft=0 yes=0 dry=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --arch) arch="${2:-}"; shift 2 ;;
    --package|--format) package="${2:-}"; shift 2 ;;
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
  if [ "$target" = linux ]; then
    case ",$bundles," in
      *,rpm,*) command -v rpmbuild >/dev/null || { echo "rpmbuild not found: install it (Debian/Ubuntu: apt install rpm; Fedora: dnf install rpm-build)" >&2; exit 1; } ;;
    esac
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
    echo "--arch only applies to --target mac" >&2; exit 1
  fi

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
      cp "$bin" "$dist/bobcat-cli-$version-$target"
      echo "  $dist/bobcat-cli-$version-$target"
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
