#!/usr/bin/env bash
#
# Install RatBlocker's Linux components from a release build.
#
# Deliberately boring and reversible: every file it writes is listed in
# `uninstall.sh`, and it never touches DNS configuration itself — that is
# `ratblocker-dns.service`'s job, and it is not enabled here.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="${REPO}/target/release"
DIST="${REPO}/dist"

PREFIX="${PREFIX:-/usr}"
LIBEXEC="${PREFIX}/libexec/ratblocker"
SHARE="${PREFIX}/share/ratblocker"
ETC="/etc/ratblocker"
STATE="/var/lib/ratblocker"

die() { echo "error: $*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run this with sudo"
[[ -x "${BUILD}/ratblockerd" ]] || die "build first: cargo build --release"
[[ -f "${DIST}/rules.rbdb" ]] || die "compile filters first: ratblocker-compile build ... --out dist"

echo "==> creating the ratblocker system account"
install -Dm644 "${REPO}/linux/packaging/sysusers/ratblocker.conf" \
  /usr/lib/sysusers.d/ratblocker.conf
systemd-sysusers

echo "==> installing binaries"
install -Dm755 "${BUILD}/ratblockerd" "${PREFIX}/bin/ratblockerd"
install -Dm755 "${BUILD}/ratblocker"  "${PREFIX}/bin/ratblocker"
install -Dm755 "${BUILD}/ratblocker-helper" "${LIBEXEC}/ratblocker-helper"

echo "==> installing filter data"
install -d "${SHARE}/filters"
install -m644 -t "${SHARE}/filters" "${REPO}"/filter-lists/bundled/*.txt
install -Dm644 "${DIST}/ATTRIBUTION.txt" "${SHARE}/ATTRIBUTION.txt"

echo "==> installing configuration"
install -d "${ETC}"
if [[ -f "${ETC}/daemon.yaml" ]]; then
  echo "    keeping the existing ${ETC}/daemon.yaml"
else
  install -Dm644 "${REPO}/linux/packaging/daemon.yaml" "${ETC}/daemon.yaml"
fi

echo "==> installing state directory"
install -Dm644 "${REPO}/linux/packaging/tmpfiles/ratblocker.conf" \
  /usr/lib/tmpfiles.d/ratblocker.conf
systemd-tmpfiles --create /usr/lib/tmpfiles.d/ratblocker.conf
install -o ratblocker -g ratblocker -m 0640 "${DIST}/rules.rbdb" "${STATE}/rules.rbdb"

echo "==> installing D-Bus and Polkit policy"
install -Dm644 "${REPO}/linux/packaging/dbus/io.github.ratblocker.Service.conf" \
  /usr/share/dbus-1/system.d/io.github.ratblocker.Service.conf
install -Dm644 "${REPO}/linux/packaging/polkit/io.github.ratblocker.policy" \
  /usr/share/polkit-1/actions/io.github.ratblocker.policy

echo "==> installing systemd units"
install -Dm644 "${REPO}/linux/packaging/systemd/ratblockerd.service" \
  /usr/lib/systemd/system/ratblockerd.service
install -Dm644 "${REPO}/linux/packaging/systemd/ratblocker-dns.service" \
  /usr/lib/systemd/system/ratblocker-dns.service
systemctl daemon-reload

cat <<'NOTICE'

Installed.

  systemctl enable --now ratblockerd        start filtering
  ratblocker status                         check on it

The daemon answers DNS on its own address but nothing sends it queries yet.
To route the whole system through it:

  systemctl enable --now ratblocker-dns

That is a separate unit on purpose, so system DNS is only touched when you ask
for it, and stopping it puts your resolver configuration back.
NOTICE
