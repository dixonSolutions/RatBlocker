# RatBlocker and VPNs

A VPN and RatBlocker both want to decide where the machine's DNS goes. This
page describes how they share it, what RatBlocker does when a tunnel comes up
or goes away, and how to tell the two apart when a name will not resolve.

Nothing here applies unless `ratblocker-dns` is running. Installing the daemon
does not redirect the system's DNS; without that unit RatBlocker answers on
`127.0.0.2:53` and only what you point at it goes through it.

## What the two are each doing

`ratblocker-dns` writes a transient drop-in at
`/run/systemd/resolved.conf.d/ratblocker.conf` containing `DNS=127.0.0.2` and
`Domains=~.`, which routes every name systemd-resolved is asked for through
RatBlocker. RatBlocker filters the name, then forwards what survives to an
upstream resolver.

The default upstream is `system`: whatever the machine itself is configured to
use, read from `/run/systemd/resolve/resolv.conf`. RatBlocker sits *in front of*
the system resolver rather than replacing it, which is what keeps internal
names, split-horizon answers and captive portals working.

A VPN changes exactly that file. Connecting Proton VPN, WireGuard, OpenVPN or
anything driven through NetworkManager replaces the machine's resolvers with
the tunnel's own, so `system` resolves to the tunnel's resolver and filtered
queries go through the tunnel. That is the intended arrangement: RatBlocker
decides *whether* a name resolves, the VPN decides *where* the question is
asked.

## Following the network

The machine's resolvers are re-read whenever they change, on a two-second poll
and again at the moment a query finds no upstream answering. A change is not a
rare event — it is every VPN connect, every VPN disconnect and every change of
Wi-Fi network.

This matters because the previous resolvers do not merely become
less appropriate when a tunnel comes up; they usually stop working. A VPN with
a kill switch firewalls off everything outside the tunnel, the old resolver
included. A resolver still pointed at it answers nothing, and because
`Domains=~.` sends *all* names through RatBlocker, that is the whole machine's
DNS rather than one application's — including the names the VPN itself needs
to stay connected. It is also a leak: a query that does escape to the previous
network's resolver has bypassed the tunnel.

When the resolvers change, the response cache is dropped with them. An answer
learned before the tunnel came up may be a split-horizon answer, a captive
portal's answer, or an address that is simply not reachable from the new
network.

If no resolver can be read at all — briefly true between networks — RatBlocker
keeps the last set it knew rather than holding none — and rather than dropping
to the public resolvers configured behind `system`, which would take names
outside a tunnel that is still up.

## Filter rules do not target VPNs

The bundled EasyList and EasyPrivacy snapshots contain no rule that blocks a
VPN provider's API, account or gateway hostnames, and RatBlocker adds none of
its own. A VPN failing to connect is not the filter layer refusing it.

The one thing to know about the DNS layer is what it is capable of matching at
all. It sees a hostname and nothing else, so it evaluates the synthetic URL
`https://<hostname>/`. A rule that depends on a path, a resource type, or the
page that made the request cannot match there; only a rule that blocks a
hostname outright can. `ratblocker status` reports how many of the loaded rules
that leaves as enforceable from a hostname alone.

## Working out which layer refused

Start with what RatBlocker is actually doing:

```sh
ratblocker status
```

`Upstream` lists the resolvers in use right now and says whether they are being
followed from the machine or were configured explicitly. After connecting a
VPN, an upstream inside the tunnel is what you want to see. An upstream on the
old network's LAN means the change has not been picked up.

Then separate a block from a failure to resolve. Ask RatBlocker directly, and
compare against a resolver that is not RatBlocker:

```sh
dig +short @127.0.0.2 api.protonvpn.ch
dig +short @9.9.9.9   api.protonvpn.ch
```

- **`NXDOMAIN` from RatBlocker, an address from the other** — a filter rule
  matched. `ratblocker allow add <domain>` exempts it. Blocked names are
  answered `NXDOMAIN` by default (`block_response` in
  `/etc/ratblocker/daemon.yaml`).
- **`SERVFAIL`, or nothing, from RatBlocker** — no upstream answered. This is a
  forwarding problem, not a filtering one; check `Upstream` in the status
  output against the network you are on.
- **Both fail the same way** — the name genuinely does not resolve, and
  RatBlocker is not involved.

`RATBLOCKER_LOG=debug` on the daemon logs the matched rule for each blocked
name and each upstream failure. It logs names, so turn it off afterwards.

## Turning it off

To confirm RatBlocker is or is not the cause, take it out of the path. This
restores whatever DNS configuration it replaced and leaves the VPN untouched:

```sh
sudo systemctl stop ratblocker-dns
```

`ratblocker pause 10m` is the lighter alternative: it keeps the proxy in the
path, so names still resolve through it, but stops it filtering. That
distinguishes a filtering decision from a forwarding problem — if pausing fixes
it, a rule matched.
