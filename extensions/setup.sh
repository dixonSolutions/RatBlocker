#!/usr/bin/env bash
#
# RatBlocker guided setup for Linux and macOS. Needs bash, and nothing else.
#
#   ./setup.sh                 find the browsers on this machine and ask
#   ./setup.sh --all --yes     every browser it can install into, no questions
#   ./setup.sh --select 1,3    pick without being asked
#   ./setup.sh --dry-run       say what would happen, change nothing
#   ./setup.sh --update        install only where what is there is older
#   ./setup.sh --uninstall     take it back out
#   ./setup.sh --list          just the inventory
#   ./setup.sh --json          the inventory as data, for scripting
#   ./setup.sh --xpi PATH|URL  install this XPI instead of looking for one
#
# No browser is named anywhere in this file, and that is the point. Browsers
# are found by asking the machine what is installed, and identified by the
# engine they actually ship:
#
#   Gecko     libxul.so / xul.dll / XUL / libxul.dylib
#   Chromium  resources.pak beside icudtl.dat or chrome_*_percent.pak
#
# A fork released after this was written ships those files too, so it is found
# on its own terms. Everything else is read out of the installation: the name
# and version from application.ini, the profile directory the way Gecko itself
# derives it, and a Chromium build's external-extension directory out of the
# strings in its own binary.
#
# Two rules hold throughout, and both matter:
#
#   Nothing found is ever executed. Asking a binary its version is how you open
#   half a dozen windows on someone's desktop, because plenty of things that
#   embed a browser engine are not browsers.
#
#   Embedding an engine is not being a browser. Every Electron application
#   ships Chromium's .pak files and a mail client ships the same libxul; both
#   are recognised and left alone.

set -uo pipefail

readonly GECKO_ID='ratblocker@ratblocker.github.io'
readonly PREF_MARKER='// added by RatBlocker setup'
readonly CRX_INSTALLED='/usr/share/ratblocker/ratblocker-chromium.crx'
# Where a signed build is published. Overridden by --xpi.
readonly AMO_SLUG='ratblocker'
readonly AMO_LATEST="https://addons.mozilla.org/firefox/downloads/latest/${AMO_SLUG}/latest.xpi"

# `CDPATH= cd` is deliberate: it empties CDPATH for that one command so cd
# cannot wander somewhere else entirely.
# shellcheck disable=SC1007
here=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC1007
repo=$(CDPATH= cd -- "$here/.." && pwd)
dist="$repo/dist"

MODE=install
DRY=0
ASSUME_YES=0
CHOOSE_ALL=0
LIST_ONLY=0
JSON_OUT=0
SELECTION=
XPI_SOURCE=
SCAN_HOME="$HOME"
SYSTEM_ROOTS=1

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) MODE=uninstall ;;
    --update) MODE=update ;;
    --dry-run) DRY=1 ;;
    --yes|-y) ASSUME_YES=1 ;;
    --all|-a) CHOOSE_ALL=1 ;;
    --list) LIST_ONLY=1 ;;
    --json) JSON_OUT=1 ;;
    --select) SELECTION="${2:-}"; shift ;;
    --xpi) XPI_SOURCE="${2:-}"; shift ;;
    --home) SCAN_HOME="${2:-}"; shift ;;
    --no-system-roots) SYSTEM_ROOTS=0 ;;
    -h|--help) sed -n '3,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

say() { [ "$JSON_OUT" -eq 1 ] || printf '%s\n' "$*"; }

# ---------------------------------------------------------------- INI files

# ini_get FILE SECTION KEY — the value, or empty. Gecko writes CRLF on some
# platforms, so the carriage return is stripped.
ini_get() {
  local file=$1 section=$2 key=$3
  [ -f "$file" ] || return 0
  awk -v section="[$section]" -v key="$key" '
    /^[[:space:]]*\[/ { in_section = ($0 ~ "^[[:space:]]*\\" section) ; next }
    in_section {
      line = $0
      sub(/\r$/, "", line)
      eq = index(line, "=")
      if (eq == 0) next
      k = substr(line, 1, eq - 1); v = substr(line, eq + 1)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", k)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", v)
      if (k == key) { print v; exit }
    }
  ' "$file"
}

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# ------------------------------------------------------------ engine probes

# engine_of DIR — "gecko", "chromium", or nothing.
engine_of() {
  local dir=$1
  if [ -e "$dir/libxul.so" ] || [ -e "$dir/xul.dll" ] || [ -e "$dir/XUL" ] \
     || [ -e "$dir/libxul.dylib" ]; then
    printf 'gecko'; return 0
  fi
  if [ -f "$dir/application.ini" ] && { [ -f "$dir/omni.ja" ] || [ -f "$dir/platform.ini" ]; }; then
    printf 'gecko'; return 0
  fi
  if [ -f "$dir/resources.pak" ] && { [ -f "$dir/icudtl.dat" ] \
     || [ -f "$dir/chrome_100_percent.pak" ] || [ -f "$dir/chrome.dll" ]; }; then
    printf 'chromium'; return 0
  fi
  return 0
}

# is_embedded DIR — true when this is an application that embeds an engine
# rather than a browser. Electron carries its own code in resources/app.asar;
# a browser's product code is the engine itself.
is_embedded() {
  local dir=$1
  [ -e "$dir/resources/app.asar" ] || [ -e "$dir/resources/app" ] \
    || [ -e "$dir/resources/default_app.asar" ] \
    || [ -e "$dir/Contents/Resources/app.asar" ]
}

# is_gecko_browser DIR — a Gecko engine is not necessarily a browser. The
# browser application code lives in browser/omni.ja; a mail client ships isp/
# and chrome/ and no browser/ at all.
is_gecko_browser() {
  local dir=$1
  [ -d "$dir/browser" ] || [ -d "$(dirname "$dir")/Resources/browser" ]
}

# ------------------------------------------------------------- the registry
#
# One record per browser, kept in parallel arrays. Profile roots are newline
# separated inside a single element, because a profile directory can and does
# contain spaces.

declare -a B_ENGINE=() B_NAME=() B_VERSION=() B_PACKAGING=() B_PKGID=()
declare -a B_INSTALL=() B_BINARY=() B_ROOTS=() B_SIGNING=() B_SIGNREASON=()
declare -a B_EXTDIR=() B_EVIDENCE=()
declare -a SKIPPED=()
declare -A SEEN=()

record() {
  local engine=$1 install=$2 packaging=$3 pkgid=$4 name=$5 version=$6 \
        binary=$7 roots=$8 signing=$9 reason=${10} extdir=${11} evidence=${12}
  [ -n "${SEEN[$install]:-}" ] && return 0
  SEEN[$install]=1
  B_ENGINE+=("$engine");      B_INSTALL+=("$install")
  B_PACKAGING+=("$packaging"); B_PKGID+=("$pkgid")
  B_NAME+=("$name");          B_VERSION+=("$version")
  B_BINARY+=("$binary");      B_ROOTS+=("$roots")
  B_SIGNING+=("$signing");    B_SIGNREASON+=("$reason")
  B_EXTDIR+=("$extdir");      B_EVIDENCE+=("$evidence")
}

# ------------------------------------------------------------ Gecko details

# gecko_ini DIR — the application.ini that this installation actually uses.
gecko_ini() {
  local dir=$1
  for candidate in "$dir/browser/application.ini" "$dir/application.ini" \
                   "$(dirname "$dir")/Resources/browser/application.ini" \
                   "$(dirname "$dir")/Resources/application.ini"; do
    [ -f "$candidate" ] && { printf '%s' "$candidate"; return 0; }
  done
}

# profile_relative INI — where Gecko will keep this application's profiles,
# relative to a home directory, derived the way Gecko derives it: [App] Profile
# wins if the build sets one, otherwise vendor-and-name on Unix and the name
# alone on macOS.
profile_relative() {
  local ini=$1 platform=$2
  local key vendor name
  key=$(ini_get "$ini" App Profile)
  vendor=$(ini_get "$ini" App Vendor)
  name=$(ini_get "$ini" App Name)
  if [ "$platform" = darwin ]; then
    if [ -n "$key" ]; then printf 'Library/Application Support/%s' "${key##*/}"
    else printf 'Library/Application Support/%s' "$name"; fi
    return 0
  fi
  if [ -n "$key" ]; then printf '.%s' "$(lower "$key")"
  elif [ -n "$vendor" ]; then printf '.%s/%s' "$(lower "$vendor")" "$(lower "$name")"
  else printf '.%s' "$(lower "$name")"; fi
}

# signing_of DIR INI — does this build enforce Mozilla's signature check?
#
# There is no way to ask a binary, and the answer decides whether an unsigned
# XPI installs or is silently rejected. MOZ_REQUIRE_SIGNING is compiled in for
# Mozilla's own release and beta builds and nothing else, so the build is asked
# what it is: a default preference that turns the check off settles it, then
# the update channel, then who built it.
#
# Prints "yes|no<TAB>reason".
signing_of() {
  local dir=$1 ini=$2
  local pref channel vendor source
  for prefdir in "$dir/browser/defaults/preferences" "$dir/defaults/pref" "$dir/defaults/preferences"; do
    [ -d "$prefdir" ] || continue
    if pref=$(grep -rlE 'xpinstall\.signatures\.required["'"'"']?[[:space:]]*,[[:space:]]*false' \
              "$prefdir" 2>/dev/null | head -1) && [ -n "$pref" ]; then
      printf 'no\tthe build ships a default that turns the signature check off'
      return 0
    fi
  done
  channel=$(grep -hoE 'app\.update\.channel["'"'"']?[[:space:]]*,[[:space:]]*["'"'"'][a-z-]+' \
            "$dir"/defaults/pref/channel-prefs.js \
            "$dir"/browser/defaults/preferences/channel-prefs.js 2>/dev/null \
            | head -1 | sed -E 's/.*["'"'"']([a-z-]+)$/\1/')
  vendor=$(ini_get "$ini" App Vendor)
  source=$(ini_get "$ini" App SourceRepository)
  if ! printf '%s' "$vendor" | grep -qi mozilla \
     || ! printf '%s' "$source" | grep -qiE 'hg\.mozilla\.org|mozilla-(release|beta|central|esr)'; then
    printf 'no\tnot a Mozilla build, so the signature check is almost certainly not compiled in'
    return 0
  fi
  case "$channel" in
    ''|release|beta)
      printf 'yes\ta Mozilla %s build enforces signing and would reject an unsigned XPI' "${channel:-release}" ;;
    *)
      printf 'no\ta Mozilla %s build honours xpinstall.signatures.required' "$channel" ;;
  esac
}

# --------------------------------------------------------- Chromium details

# chromium_extdir DIR BINARY — where this build looks for extensions it did not
# install itself. It is a compile-time constant left in the binary as a plain
# string, so the build is asked rather than guessed at.
chromium_extdir() {
  local dir=$1 binary=$2 found=
  if [ -n "$binary" ] && [ -r "$binary" ]; then
    found=$(grep -aoE '/(usr|opt)/[A-Za-z0-9._+-]+(/[A-Za-z0-9._+-]+)*/extensions' \
            "$binary" 2>/dev/null | sort -u | head -1)
  fi
  if [ -n "$found" ]; then printf '%s' "$found"
  else printf '/usr/share/%s/extensions' "$(basename "$dir")"; fi
}

# ------------------------------------------------------------- the scanning

platform=linux
case "$(uname -s)" in
  Darwin) platform=darwin ;;
  Linux) platform=linux ;;
  *) platform=$(lower "$(uname -s)") ;;
esac

# Locations, not browsers: anything installed tomorrow lands in one of them.
scan_roots() {
  if [ "$platform" = darwin ]; then
    [ "$SYSTEM_ROOTS" -eq 1 ] && printf '%s\n' /Applications /Applications/Utilities
    printf '%s\n' "$SCAN_HOME/Applications"
  else
    [ "$SYSTEM_ROOTS" -eq 1 ] && printf '%s\n' /usr/lib /usr/lib64 /usr/local/lib /opt /usr/share
    printf '%s\n' "$SCAN_HOME/.local/lib" "$SCAN_HOME/.local/share" \
                  "$SCAN_HOME/.tarball-installations" "$SCAN_HOME/Applications"
  fi
}

# candidate_dirs ROOT DEPTH — directories under ROOT that hold an engine,
# found by looking for the engine's own files.
candidate_dirs() {
  local root=$1 depth=$2
  [ -d "$root" ] || return 0
  find "$root" -maxdepth "$depth" -type f \
    \( -name libxul.so -o -name libxul.dylib -o -name XUL -o -name resources.pak \) \
    2>/dev/null | while IFS= read -r hit; do dirname "$hit"; done | sort -u
}

# The binary a user would launch, from inside an installation directory.
executable_in() {
  local dir=$1 hint=$2 candidate
  for candidate in "$hint" "$(basename "$dir")" "$(basename "$dir").sh"; do
    [ -n "$candidate" ] && [ -f "$dir/$candidate" ] && [ -x "$dir/$candidate" ] \
      && { printf '%s' "$dir/$candidate"; return 0; }
  done
  find "$dir" -maxdepth 1 -type f -perm -u+x \
    ! -name '*.so' ! -name '*.so.*' ! -name '*.pak' ! -name '*.bin' \
    ! -name '*.dat' ! -name '*.ja' ! -name '*.json' 2>/dev/null | head -1
}

# add_install DIR PACKAGING PKGID SANDBOX_HOME — classify one directory and
# record it if it turns out to be a browser.
add_install() {
  local dir=$1 packaging=$2 pkgid=$3 sandbox=$4
  local engine; engine=$(engine_of "$dir")
  [ -z "$engine" ] && return 0

  if is_embedded "$dir"; then
    SKIPPED+=("$dir — embeds a browser engine but ships its own application code, so it is an application, not a browser")
    return 0
  fi

  if [ "$engine" = gecko ]; then
    local ini name version binary relative roots='' signing reason home_dir
    ini=$(gecko_ini "$dir")
    if ! is_gecko_browser "$dir"; then
      SKIPPED+=("$dir — a Gecko application with no browser/ directory, so not a browser")
      return 0
    fi
    name=$(ini_get "$ini" App Name); [ -z "$name" ] && name=$(basename "$dir")
    version=$(ini_get "$ini" App Version)
    binary=$(executable_in "$dir" "$(ini_get "$ini" App RemotingName)")
    [ -n "$pkgid" ] && binary="$packaging run $pkgid"
    relative=$(profile_relative "$ini" "$platform")
    home_dir=${sandbox:-$SCAN_HOME}
    [ -d "$home_dir/$relative" ] && roots="$home_dir/$relative"
    IFS=$'\t' read -r signing reason < <(signing_of "$dir" "$ini")
    record gecko "$dir" "$packaging" "$pkgid" "$name" "$version" "$binary" \
      "$roots" "$signing" "$reason" '' "$(engine_evidence "$dir")"
    return 0
  fi

  local binary extdir version name
  binary=$(executable_in "$dir" chrome)
  extdir=$(chromium_extdir "$dir" "$binary")
  name=$(desktop_name "$dir" "$binary"); [ -z "$name" ] && name=$(basename "$dir")
  version=$(package_version "$binary")
  [ -n "$pkgid" ] && binary="$packaging run $pkgid"
  record chromium "$dir" "$packaging" "$pkgid" "$name" "$version" "$binary" \
    '' '' '' "$extdir" "$(engine_evidence "$dir")"
}

engine_evidence() {
  local dir=$1 out=''
  for marker in libxul.so libxul.dylib XUL xul.dll resources.pak icudtl.dat \
                chrome_100_percent.pak application.ini; do
    [ -e "$dir/$marker" ] && out="$out${out:+, }$marker"
  done
  printf '%s' "$out"
}

# The name the system itself gives this browser, from the desktop entry that
# claims to handle http — which is the system's own definition of a browser.
desktop_name() {
  local dir=$1 binary=$2 file name exec_line
  [ "$platform" = darwin ] && return 0
  for d in /usr/share/applications /usr/local/share/applications \
           /var/lib/flatpak/exports/share/applications \
           /var/lib/snapd/desktop/applications "$SCAN_HOME/.local/share/applications"; do
    [ -d "$d" ] || continue
    while IFS= read -r file; do
      grep -qE '^MimeType=.*x-scheme-handler/https?' "$file" 2>/dev/null || continue
      exec_line=$(ini_get "$file" 'Desktop Entry' Exec)
      exec_line=${exec_line%% *}
      [ -z "$exec_line" ] && continue
      if [ "$exec_line" = "$binary" ] \
         || [ "$(basename "$exec_line")" = "$(basename "${binary:-x}")" ] \
         || [ "$(basename "$exec_line")" = "$(basename "$dir")" ]; then
        name=$(ini_get "$file" 'Desktop Entry' Name)
        [ -n "$name" ] && { printf '%s' "$name"; return 0; }
      fi
    done < <(find "$d" -maxdepth 1 -name '*.desktop' 2>/dev/null)
  done
}

# A version without running anything: the package manager already knows.
package_version() {
  local binary=${1:-} out
  [ -n "$binary" ] && [ -e "$binary" ] || return 0
  if command -v dpkg-query >/dev/null 2>&1; then
    out=$(dpkg-query -S "$binary" 2>/dev/null | cut -d: -f1 | head -1)
    [ -n "$out" ] && dpkg-query -W -f='${Version}' "$out" 2>/dev/null | sed 's/^[0-9]*://' && return 0
  fi
  if command -v rpm >/dev/null 2>&1; then
    rpm -qf --queryformat='%{VERSION}' "$binary" 2>/dev/null | grep -E '^[0-9]' && return 0
  fi
  return 0
}

# --------------------------------------------------------------- discovery

discover() {
  local root
  while IFS= read -r root; do
    local depth=2
    case "$root" in
      /opt) depth=3 ;;
      /Applications|"$SCAN_HOME/Applications") depth=4 ;;
    esac
    while IFS= read -r dir; do
      [ -n "$dir" ] && add_install "$dir" \
        "$(case "$dir" in "$SCAN_HOME"*) printf user ;; *) printf system ;; esac)" '' ''
    done < <(candidate_dirs "$root" "$depth")
  done < <(scan_roots)

  [ "$SYSTEM_ROOTS" -eq 1 ] || return 0

  # Flatpak and snap are separate installations with separate homes, and are
  # asked directly rather than looked for.
  if command -v flatpak >/dev/null 2>&1; then
    local id location
    while IFS= read -r id; do
      [ -z "$id" ] && continue
      location=$(flatpak info --show-location "$id" 2>/dev/null)
      [ -z "$location" ] || [ ! -d "$location/files" ] && continue
      while IFS= read -r dir; do
        [ -n "$dir" ] && add_install "$dir" flatpak "$id" "$SCAN_HOME/.var/app/$id"
      done < <(candidate_dirs "$location/files" 4)
    done < <(flatpak list --app --columns=application 2>/dev/null)
  fi

  if command -v snap >/dev/null 2>&1; then
    local name
    while IFS= read -r name; do
      [ -z "$name" ] || [ ! -d "/snap/$name/current" ] && continue
      while IFS= read -r dir; do
        [ -n "$dir" ] && add_install "$dir" snap "$name" "$SCAN_HOME/snap/$name/current"
      done < <(candidate_dirs "/snap/$name/current" 5)
    done < <(snap list 2>/dev/null | tail -n +2 | awk '{print $1}')
  fi
}

# attach_loose_profiles — profile roots whose installation was not found, and
# profiles that live somewhere other than where the build says they should.
#
# Gecko writes the installation it last ran from into each profile's
# compatibility.ini, which turns "which browser does this profile belong to"
# from a guess into a fact. A Gecko install is per profile and needs no binary,
# so a profile whose browser is gone is still something that can be installed
# into — but a mail profile is not, and says so by holding mail.
attach_loose_profiles() {
  local base root owner i matched
  local -a bases=("$SCAN_HOME")
  [ "$SYSTEM_ROOTS" -eq 1 ] && [ -d "$SCAN_HOME/.var/app" ] && \
    while IFS= read -r base; do bases+=("$base"); done < <(find "$SCAN_HOME/.var/app" -maxdepth 1 -mindepth 1 -type d 2>/dev/null)
  [ "$SYSTEM_ROOTS" -eq 1 ] && [ -d "$SCAN_HOME/snap" ] && \
    while IFS= read -r base; do bases+=("$base"); done < <(find "$SCAN_HOME/snap" -maxdepth 2 -mindepth 2 -type d \( -name current -o -name common \) 2>/dev/null)

  for base in "${bases[@]}"; do
    [ -d "$base" ] || continue
    while IFS= read -r ini; do
      root=$(dirname "$ini")
      matched=0
      for i in "${!B_ROOTS[@]}"; do
        case $'\n'"${B_ROOTS[$i]}"$'\n' in *$'\n'"$root"$'\n'*) matched=1; break ;; esac
      done
      [ "$matched" -eq 1 ] && continue

      owner=$(find "$root" -maxdepth 2 -name compatibility.ini 2>/dev/null | head -1)
      [ -n "$owner" ] && owner=$(ini_get "$owner" Compatibility LastPlatformDir)
      owner=${owner%/browser}

      # Does it belong to something already found?
      if [ -n "$owner" ]; then
        for i in "${!B_INSTALL[@]}"; do
          if [ "${B_INSTALL[$i]}" = "$owner" ]; then
            B_ROOTS[$i]="${B_ROOTS[$i]:+${B_ROOTS[$i]}$'\n'}$root"
            matched=1; break
          fi
        done
      fi
      [ "$matched" -eq 1 ] && continue

      # A mail profile keeps mail; a browser profile never does.
      if find "$root" -maxdepth 2 \( -name ImapMail -o -name abook.sqlite -o -name Mail \) \
           2>/dev/null | head -1 | grep -q .; then
        SKIPPED+=("$root — a profile holding mail, not a browser profile")
        continue
      fi
      record gecko "$root" profile-only '' "$(basename "$root" | sed 's/^\.//')" '' '' \
        "$root" 'unknown' "the installation this profile belongs to was not found${owner:+ (it last ran from $owner)}" \
        '' "$root/profiles.ini"
    done < <(find "$base" -maxdepth 3 -name profiles.ini 2>/dev/null)
  done
}

# profiles_in ROOT — the profile directories listed in profiles.ini.
profiles_in() {
  local root=$1 ini="$1/profiles.ini"
  [ -f "$ini" ] || return 0
  awk -v root="$root" '
    /^[[:space:]]*\[/ { section = $0; path = ""; relative = 1; next }
    section ~ /^\[Profile/ {
      line = $0; sub(/\r$/, "", line)
      if (line ~ /^Path=/) { path = substr(line, 6) }
      if (line ~ /^IsRelative=0/) { relative = 0 }
      if (path != "") { print (relative ? root "/" path : path); path = "" }
    }
  ' "$ini" | while IFS= read -r dir; do [ -d "$dir" ] && printf '%s\n' "$dir"; done
}

# ---------------------------------------------------------------- artefacts

xpi=
xpi_version=
crx=
descriptor=
chromium_id=

find_artefacts() {
  if [ -n "$XPI_SOURCE" ]; then
    case "$XPI_SOURCE" in
      http://*|https://*) xpi=$(download "$XPI_SOURCE") ;;
      *) xpi=$XPI_SOURCE ;;
    esac
  else
    xpi=$(find "$dist" -maxdepth 1 -name 'ratblocker-firefox-*.xpi' 2>/dev/null | sort | tail -1)
  fi
  [ -n "$xpi" ] && xpi_version=$(printf '%s' "$(basename "$xpi")" | sed -E 's/ratblocker-firefox-(.*)\.xpi/\1/')
  [ -f "$dist/ratblocker-chromium.crx" ] && crx="$dist/ratblocker-chromium.crx"
  if [ -n "$crx" ]; then
    # Matched with grep rather than `find -regex`: the two find dialects
    # disagree about intervals, and this script runs on macOS as well.
    descriptor=$(find "$dist" -maxdepth 1 -name '*.json' 2>/dev/null \
      | grep -E '/[a-p]{32}\.json$' | head -1)
    [ -n "$descriptor" ] && chromium_id=$(basename "$descriptor" .json)
  fi
}

download() {
  local url=$1 out
  out=$(mktemp -t ratblocker-XXXXXX.xpi) || return 1
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out" || return 1
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$out" "$url" || return 1
  else
    printf 'neither curl nor wget is available to fetch %s\n' "$url" >&2
    return 1
  fi
  printf '%s' "$out"
}

# True when the XPI carries a Mozilla signature.
xpi_is_signed() {
  [ -n "$xpi" ] && [ -f "$xpi" ] && grep -qa 'META-INF/mozilla.rsa' "$xpi"
}

# ------------------------------------------------------------------ actions

# The version of RatBlocker a profile already has, according to the browser's
# own database.
installed_version() {
  local profile=$1 db="$1/extensions.json"
  [ -f "$db" ] || return 0
  tr ',' '\n' < "$db" | grep -A2 -F "\"$GECKO_ID\"" | grep -oE '"version":"[^"]+"' \
    | head -1 | sed 's/.*:"//; s/"//'
}

# newer A B — true when A is a strictly newer dotted version than B. Compared
# component by component, because `sort -V` is GNU-only and this has to give
# the same answer on macOS.
newer() {
  awk -v a="$1" -v b="$2" '
    BEGIN {
      n = split(a, x, "."); m = split(b, y, ".")
      for (i = 1; i <= (n > m ? n : m); i++) {
        u = (i <= n ? x[i] + 0 : 0); v = (i <= m ? y[i] + 0 : 0)
        if (u > v) exit 0
        if (u < v) exit 1
      }
      exit 1
    }'
}

set_user_prefs() {
  local profile=$1 remove=${2:-0} file="$1/user.js" tmp
  tmp=$(mktemp) || return 1
  [ -f "$file" ] && grep -vF "$PREF_MARKER" "$file" > "$tmp"
  if [ "$remove" -eq 1 ]; then
    if [ -s "$tmp" ]; then mv "$tmp" "$file"; else rm -f "$tmp" "$file"; fi
    return 0
  fi
  {
    printf 'user_pref("xpinstall.signatures.required", false); %s\n' "$PREF_MARKER"
    printf 'user_pref("extensions.autoDisableScopes", 0); %s\n' "$PREF_MARKER"
  } >> "$tmp"
  mv "$tmp" "$file"
}

# install_into PROFILE — the whole of the Gecko install, and of the update:
# the browser reads the version out of the file at startup, so replacing the
# file is the update. The browser still performs the install, and still
# verifies it.
install_into() {
  local profile=$1 target="$1/extensions/$GECKO_ID.xpi" current
  case "$MODE" in
    uninstall)
      if [ "$DRY" -eq 1 ]; then printf 'would remove %s\n' "$target"; return 0; fi
      rm -f "$target"; set_user_prefs "$profile" 1
      printf 'removed %s\n' "$target" ;;
    update)
      current=$(installed_version "$profile")
      if [ -n "$current" ] && ! newer "$xpi_version" "$current"; then
        printf 'up to date (%s) in %s\n' "$current" "$profile"; return 0
      fi
      printf '%s -> %s in %s\n' "${current:-nothing}" "$xpi_version" "$profile"
      [ "$DRY" -eq 1 ] && return 0
      write_xpi "$profile" "$target" ;;
    *)
      if [ "$DRY" -eq 1 ]; then printf 'would install %s\n' "$target"; return 0; fi
      write_xpi "$profile" "$target"
      printf 'installed %s\n' "$target" ;;
  esac
}

write_xpi() {
  local profile=$1 target=$2
  mkdir -p "$(dirname "$target")" || return 1
  cp "$xpi" "$target" && chmod 644 "$target" || return 1
  xpi_is_signed || set_user_prefs "$profile"
}

# ------------------------------------------------------------------ the run

discover
attach_loose_profiles
find_artefacts

# plan_of INDEX — prints "status<TAB>detail". Status decides the marker shown
# and whether "all" will act on it.
plan_of() {
  local i=$1
  if [ "${B_ENGINE[$i]}" = gecko ]; then
    [ -z "$xpi" ] && { printf 'blocked\tno XPI available; build one, or pass --xpi <path|url>'; return 0; }
    if [ "${B_SIGNING[$i]}" = yes ] && ! xpi_is_signed && [ "$MODE" != uninstall ]; then
      printf 'refused\t%s' "${B_SIGNREASON[$i]}"; return 0
    fi
    local count=0
    while IFS= read -r root; do
      [ -z "$root" ] && continue
      while IFS= read -r _; do count=$((count + 1)); done < <(profiles_in "$root")
    done <<< "${B_ROOTS[$i]}"
    [ "$count" -eq 0 ] && { printf 'no-profile\tno profile yet — start the browser once'; return 0; }
    printf 'ok\t%d profile%s' "$count" "$([ "$count" -eq 1 ] || printf s)"
    return 0
  fi

  [ -z "$descriptor" ] && { printf 'blocked\tno CRX packaged; run node build.mjs && node package.mjs'; return 0; }
  case "${B_PACKAGING[$i]}" in
    flatpak|snap)
      printf 'manual\ta sandboxed Chromium reads extensions from inside its own sandbox, not from %s' "${B_EXTDIR[$i]}"
      return 0 ;;
  esac
  if [ "$platform" = darwin ]; then
    printf 'needs-root\tmacOS Chrome refuses off-store external installs, so this goes through policy'
    return 0
  fi
  printf 'needs-root\t%s' "${B_EXTDIR[$i]}"
}

mark_for() {
  case "$1" in
    ok) printf '✓' ;; needs-root) printf '!' ;; manual) printf '!' ;;
    refused) printf '✗' ;; blocked) printf '✗' ;; no-profile) printf '-' ;; *) printf '?' ;;
  esac
}

declare -a P_STATUS=() P_DETAIL=()
for i in "${!B_ENGINE[@]}"; do
  IFS=$'\t' read -r status detail < <(plan_of "$i")
  P_STATUS+=("$status"); P_DETAIL+=("$detail")
done

if [ "$JSON_OUT" -eq 1 ]; then
  printf '{\n  "platform": "%s",\n  "mode": "%s",\n' "$platform" "$MODE"
  printf '  "xpi": "%s",\n  "xpiVersion": "%s",\n  "chromiumId": "%s",\n' \
    "${xpi:-}" "${xpi_version:-}" "${chromium_id:-}"
  printf '  "browsers": [\n'
  for i in "${!B_ENGINE[@]}"; do
    [ "$i" -gt 0 ] && printf ',\n'
    printf '    {"name": "%s", "engine": "%s", "version": "%s", "packaging": "%s",' \
      "${B_NAME[$i]}" "${B_ENGINE[$i]}" "${B_VERSION[$i]}" "${B_PACKAGING[$i]}"
    printf ' "installDir": "%s", "binary": "%s", "signing": "%s",' \
      "${B_INSTALL[$i]}" "${B_BINARY[$i]}" "${B_SIGNING[$i]}"
    printf ' "externalDir": "%s", "status": "%s", "detail": "%s",' \
      "${B_EXTDIR[$i]}" "${P_STATUS[$i]}" "${P_DETAIL[$i]}"
    printf ' "profileRoots": ['
    local_first=1
    while IFS= read -r root; do
      [ -z "$root" ] && continue
      [ "$local_first" -eq 0 ] && printf ', '
      printf '"%s"' "$root"; local_first=0
    done <<< "${B_ROOTS[$i]}"
    printf ']}'
  done
  printf '\n  ],\n  "skipped": %d\n}\n' "${#SKIPPED[@]}"
  exit 0
fi

say ""
say "RatBlocker setup — $MODE$([ "$DRY" -eq 1 ] && printf ' (dry run)')"
say "  packaged   ${xpi:-no XPI}$( [ -n "$xpi" ] && { xpi_is_signed && printf ' (signed)' || printf ' (UNSIGNED)'; } )"
say "  platform   $platform"
say ""

if [ "${#B_ENGINE[@]}" -eq 0 ]; then
  say "No browser was found on this machine."
  exit 0
fi

say "Found:"
say ""
for i in "${!B_ENGINE[@]}"; do
  printf '%3d %s %-30s %-9s %s\n' "$((i + 1))" "$(mark_for "${P_STATUS[$i]}")" \
    "$(printf '%s %s' "${B_NAME[$i]}" "${B_VERSION[$i]}" | cut -c1-30)" \
    "${B_ENGINE[$i]}" "${B_PACKAGING[$i]}"
  printf '        %s\n' "${B_PKGID[$i]:-${B_INSTALL[$i]}}"
  printf '        %s\n' "${P_DETAIL[$i]}"
done

[ "$LIST_ONLY" -eq 1 ] && exit 0

# ---------------------------------------------------------------- selecting

actionable=()
for i in "${!P_STATUS[@]}"; do
  case "${P_STATUS[$i]}" in ok|needs-root) actionable+=("$i") ;; esac
done

chosen=()
parse_selection() {
  local input=$1 part start end n
  input=$(printf '%s' "$input" | tr '[:upper:]' '[:lower:]')
  case "$input" in
    q|quit) return 1 ;;
    a|all) chosen=("${actionable[@]}"); return 0 ;;
  esac
  for part in $(printf '%s' "$input" | tr ',' ' '); do
    case "$part" in
      *-*) start=${part%%-*}; end=${part##*-}
           for ((n = start; n <= end; n++)); do
             [ "$n" -ge 1 ] && [ "$n" -le "${#B_ENGINE[@]}" ] && chosen+=("$((n - 1))")
           done ;;
      *[!0-9]*|'') ;;
      *) [ "$part" -ge 1 ] && [ "$part" -le "${#B_ENGINE[@]}" ] && chosen+=("$((part - 1))") ;;
    esac
  done
  return 0
}

if [ -n "$SELECTION" ]; then
  parse_selection "$SELECTION" || { say "Nothing selected."; exit 0; }
elif [ "$CHOOSE_ALL" -eq 1 ] || [ "$ASSUME_YES" -eq 1 ]; then
  chosen=("${actionable[@]}")
else
  say ""
  say 'Select browsers: numbers (1 3), a range (1-3), "a" for all, "q" to quit.'
  printf '> '
  IFS= read -r answer || answer=
  printf '%s\n' "$answer"
  parse_selection "$answer" || { say "Nothing selected."; exit 0; }
  [ "${#chosen[@]}" -eq 0 ] && { say "Nothing selected."; exit 0; }
  if [ "$DRY" -eq 0 ]; then
    printf 'Proceed with %s? [y/N] ' "$MODE"
    IFS= read -r confirm || confirm=
    printf '%s\n' "$confirm"
    case "$confirm" in y|Y|yes|Yes) ;; *) say "Nothing done."; exit 0 ;; esac
  fi
fi

[ "${#chosen[@]}" -eq 0 ] && { say "Nothing selected."; exit 0; }

# ---------------------------------------------------------------- executing

say ""
say "$([ "$DRY" -eq 1 ] && printf 'Would do' || printf 'Doing'):"
say ""
privileged=()
for i in "${chosen[@]}"; do
  say "$(mark_for "${P_STATUS[$i]}") ${B_NAME[$i]}"
  case "${P_STATUS[$i]}" in
    ok)
      if [ "${B_ENGINE[$i]}" = gecko ]; then
        while IFS= read -r root; do
          [ -z "$root" ] && continue
          while IFS= read -r profile; do
            say "    $(install_into "$profile")"
          done < <(profiles_in "$root")
        done <<< "${B_ROOTS[$i]}"
        [ "$MODE" != uninstall ] && say "    restart the browser to pick it up"
      fi ;;
    needs-root)
      if [ "$MODE" = uninstall ]; then
        privileged+=("rm -f ${B_EXTDIR[$i]}/$chromium_id.json" "rm -f $CRX_INSTALLED")
      else
        privileged+=("install -Dm644 $crx $CRX_INSTALLED" \
                     "install -Dm644 $descriptor ${B_EXTDIR[$i]}/$chromium_id.json")
      fi
      say "    needs root; the commands are collected below" ;;
    *) say "    ${P_DETAIL[$i]}" ;;
  esac
done

if [ "${#privileged[@]}" -gt 0 ]; then
  say ""
  say "A store-free Chromium install writes to a system-wide location, so these need"
  say "privileges this script does not have:"
  say ""
  for command in "${privileged[@]}"; do say "  sudo $command"; done
fi

for i in "${!P_STATUS[@]}"; do
  if [ "${P_STATUS[$i]}" = refused ]; then
    say ""
    say "A refused browser enforces Mozilla signing. Install the signed build instead:"
    say "  ./setup.sh --xpi $AMO_LATEST"
    break
  fi
done

if [ "${#SKIPPED[@]}" -gt 0 ]; then
  say ""
  say "note: ${#SKIPPED[@]} installation(s) embedding a browser engine were skipped for"
  say "      not being browsers. Run with --list to see everything that was considered."
fi
