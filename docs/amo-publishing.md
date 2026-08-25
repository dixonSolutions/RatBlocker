# Publishing RatBlocker to addons.mozilla.org

RatBlocker is published to the Firefox add-on catalogue (AMO) from CI. This
document records how the pipeline is wired, so the moving parts — credentials,
versioning, the two distribution channels, and the store listing — can be
reasoned about without re-reading the workflow.

## The release trigger

A push to `main` is a release. `.github/workflows/publish-amo.yml` bumps the
extension's patch version, builds the filter database and the WebAssembly
core, packages the Firefox extension, lints it with `web-ext`, and submits the
new version to AMO's **listed** channel — the public catalogue that gives the
add-on a `addons.mozilla.org` page and one-click install.

The trigger is path-filtered so only changes that affect the extension cause a
release: `core/`, `filter-compiler/`, `filter-lists/`, `extensions/`,
`design/`, `Cargo.toml`, `Cargo.lock`, and the workflow file itself. Editing
the site or docs does not publish.

The version is bumped in the working tree before the build (so the XPI that
reaches AMO carries a version no previous release used), but the bump is only
committed back to `main` and tagged `v<version>` **after AMO accepts the
upload**. Committing the tag only on success means a failed (or throttled)
submit leaves no dangling tag, so re-running the job recomputes the same
version and retries cleanly instead of colliding on an existing tag. Pushes
made with the auto-generated `GITHUB_TOKEN` do not re-trigger workflows, so
the release commit cannot loop.

A manual run is still available through `workflow_dispatch`, with three inputs:

- `channel` — `listed` (default, the public catalogue) or `unlisted` (Mozilla
  signs the file for self-hosting; see `docs/scope.md`).
- `dry_run` — build and lint without submitting or bumping.
- `skip_bump` — publish the current version as-is instead of bumping first.
  AMO rejects a duplicate version, so this is only useful for re-running a
  failed submission of an already-bumped version.

## Credentials

AMO authenticates each request with a short-lived HS256 JWT minted from an
issuer/secret pair. Generate a pair at
<https://addons.mozilla.org/developers/addon/api/key/>.

- **Locally**, put them in a gitignored `.env` at the repository root:

  ```
  AMO_JWT_ISSUER=user:12345:67
  AMO_JWT_SECRET=...
  ```

  The scripts also accept the legacy `.amo-credentials` file or real
  environment variables, but `.env` is the supported path.

- **In CI**, store them as repository secrets `AMO_JWT_ISSUER` and
  `AMO_JWT_SECRET`:

  ```sh
  printf '%s' 'user:12345:67' | gh secret set AMO_JWT_ISSUER
  printf '%s' '...'      | gh secret set AMO_JWT_SECRET
  ```

  The workflow fails with a pointer to this document if either is missing.

The credential loader, the JWT mint, and the request wrapper live in
`extensions/amo.mjs` and are shared by `sign-firefox.mjs` (submit a version)
and `update-amo-listing.mjs` (decorate the listing), so the two cannot drift
on how they authenticate.

## Versioning

`extensions/bump-version.mjs` bumps the patch component of the version and
writes it back to every place that records it, so they never drift:

- `extensions/firefox/manifest.json` — the version AMO sees.
- `extensions/chromium/manifest.json` — kept in sync so the two stores never
  disagree.
- `extensions/package.json` — informational, but drift here is confusing.
- `Cargo.toml` — the `[workspace.package]` version; dependency versions inside
  `{ }` are left untouched.

Run it by hand with `node extensions/bump-version.mjs` (bump patch) or
`node extensions/bump-version.mjs 0.2.0` (set an explicit version).

## Manifest compliance

Two manifest fields are shaped by what AMO accepts; both are in
`extensions/firefox/manifest.json`.

- **`data_collection_permissions`** is mandatory for new Firefox extensions
  since November 2025. RatBlocker collects nothing, so it declares
  `required: ["none"]` and `optional: []`. The field lives under
  `browser_specific_settings.gecko`. `strict_min_version` stays at `128.0`
  even though the consent UI needs 140 — because nothing is collected, there
  is nothing to consent to on older Firefox, and dropping ESR 128 users would
  be a regression. `web-ext` warns about this; the warning is benign.
- **`update_url`** is forbidden for listed (Mozilla-hosted) add-ons, because
  AMO serves updates itself. `extensions/package.mjs` injects `update_url` only
  for self-hosted (unlisted) packaging and deletes it for listed packaging,  so
  the same source manifest produces a compliant XPI for either channel.

## The store listing

The first listed submission **creates** the add-on entry on AMO. The listing
metadata sent at creation time (slug, name, summary, description, categories,
license) lives in the `LISTING` constant in `sign-firefox.mjs` and is sent
only on creation — afterwards the listing is edited on AMO itself, and the
script must not quietly overwrite it.

- **Icon.** The XPI manifest `icons` field (16/32/48/128 in
  `extensions/firefox/manifest.json`) only covers the in-browser toolbar and
  add-on management page. The **AMO listing icon** — the square mark AMO shows
  in search results and on the add-on page — is a *separate* upload, exposed as
  the `icon` field on the add-on (`PATCH /addons/addon/<id>/`), which AMO resizes
  to 32/64/128. Without that upload the listing falls back to the generic
  puzzle-piece icon, even though the XPI carries perfectly good icons.
  `extensions/update-amo-listing.mjs` uploads `design/icon-512.png` (the
  highest-resolution square source; the assets in `extensions/shared/icons/` are
  generated from `design/icon.svg` — see `design/README.md`).
- **Banner.** AMO has no banner slot, but it has preview screenshots.
  `extensions/update-amo-listing.mjs` uploads `design/preview-1280x800.png` (the
  banner centred on its own dark background, sized to AMO's preferred 1280x800)
  as the first preview, which is what gives the listing a hero image.

`update-amo-listing.mjs` does both uploads, and both are idempotent — the icon
`PATCH` replaces, and the preview step deletes any existing previews before
uploading the current one — so the workflow runs it on every listed release
(the "Decorate the listing" step) without stacking duplicate screenshots. By
hand:

```sh
node extensions/update-amo-listing.mjs
```
- **License.** The version's license is sent as a slug under the `version`
  object at creation time (see `VERSION_META` in `sign-firefox.mjs`). AMO exposes
  GPLv3 only as the builtin slug `GPL-3.0-only` — it has no `GPL-3.0-or-later`,
  so that slug is used even though the repo's `LICENSE` is the "or-later" terms.
  uBlock Origin and other GPL-3.0-or-later add-ons do the same. The `version`
  object (holding `upload` + `license`) is mandatory at creation: AMO rejects a
  create call that puts `upload` at the top level with
  `{"version":["This field is required."]}`.

After a listed submission, `sign-firefox.mjs` writes `dist/amo-listing.json`
with the listing URL and slug, and the workflow commits a follow-up that
points the site's install button at the real add-on. The live listing is at
<https://addons.mozilla.org/firefox/addon/ratblocker/>. A brand-new add-on
sits in the `nominated` state until AMO review approves it; while nominated
the public detail endpoint returns `{}` and only the authenticated endpoint
shows the entry, so do not be alarmed if `curl`-ing the public URL comes back
empty before approval.

## Two channels

- **Listed** puts the add-on in the public catalogue. It enters a review queue,
  so there is no signed file to collect; what the script writes instead is
  `dist/amo-listing.json`. Once approved, Firefox updates every install from
  Mozilla with nothing to host.
- **Unlisted** keeps distribution in your hands. Mozilla signs the file, you
  host it and its `firefox-updates.json` update manifest yourself. The signed
  XPI is downloaded to `dist/ratblocker-firefox-<version>-signed.xpi`.

## Troubleshooting

- **`AMO rejected the package`** — the `web-ext lint` step in CI mirrors AMO's
  validator, so a failure there names the same problem AMO will raise. Fix the
  manifest and push to `main` again.
- **Duplicate version** — AMO rejects a version equal to one already uploaded.
  The bump step prevents this; if you used `skip_bump` against a version AMO
  already has, run a normal push to bump past it.
- **`update_url` is not allowed`** — only happens for listed builds; ensure
  `RATBLOCKER_CHANNEL=listed` is set at packaging (the workflow sets it).
- **Listing shows the generic puzzle-piece icon** — the XPI manifest `icons`
  only feed the in-browser toolbar, not the AMO listing. The listing icon is a
  separate `icon` upload (`PATCH /addons/addon/<id>/`). The "Decorate the
  listing" step does it; if it was skipped or failed, run
  `node extensions/update-amo-listing.mjs` by hand. The resized 32/64/128 URLs
  appear asynchronously, so the authenticated `icon_url` may lag a moment.
- **`License with slug=… does not exist`** — AMO's license slugs are a fixed
  builtin set, not raw SPDX. GPLv3 is `GPL-3.0-only` (there is no `or-later`);
  LGPLv3 is `LGPL-3.0-only`; MPL is `MPL-2.0`. Inspect a comparable public
  add-on's `current_version.license.slug` to confirm a slug before sending it.
- **`version: This field is required`** on create — the create call must nest
  `upload` and `license` inside a top-level `version` object, not at the root.
- **Bump commit did not land** — the release commit + tag are pushed with
  `GITHUB_TOKEN` only after a successful submit. If the submit failed, no tag
  was created; just re-run the workflow (it recomputes the same version and
  retries). If the submit succeeded but the commit failed, the version is on
  AMO but not on `main`; run `node extensions/bump-version.mjs <version>`
  with the published version and commit it by hand, then the next run bumps
  past it.
