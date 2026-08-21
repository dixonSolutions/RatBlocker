#!/usr/bin/env bash
#
# Remove everything install.sh added, and put system DNS back first.

set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "run this with sudo" >&2; exit 1; }

echo "==> restoring DNS and stopping services"
systemctl disable --now ratblocker-dns.service 2>/dev/null || true
systemctl disable --now ratblockerd.service 2>/dev/null || true
# Belt and braces: undo the DNS redirection even if the unit is already gone.
/usr/libexec/ratblocker/ratblocker-helper dns-restore 2>/dev/null || true

echo "==> removing files"
rm -f /usr/bin/ratblockerd /usr/bin/ratblocker
rm -rf /usr/libexec/ratblocker
rm -rf /usr/share/ratblocker
rm -f /usr/share/dbus-1/system.d/io.github.ratblocker.Service.conf
rm -f /usr/share/polkit-1/actions/io.github.ratblocker.policy
rm -f /usr/lib/systemd/system/ratblockerd.service
rm -f /usr/lib/systemd/system/ratblocker-dns.service
rm -f /usr/lib/sysusers.d/ratblocker.conf
rm -f /usr/lib/tmpfiles.d/ratblocker.conf
systemctl daemon-reload

cat <<'NOTICE'
Removed.

Left in place on purpose, so an accidental uninstall does not lose settings:
  /etc/ratblocker      configuration
  /var/lib/ratblocker  rule database and state
  the ratblocker system account

Remove those yourself if you want them gone:
  sudo rm -rf /etc/ratblocker /var/lib/ratblocker && sudo userdel ratblocker
NOTICE
