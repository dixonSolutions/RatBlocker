import { Component } from '@angular/core';
import { RouterLink } from '@angular/router';
import { Message } from '@openng/optimus-ui/message';
import { Tag } from '@openng/optimus-ui/tag';

import { BUILD_FACTS } from '../../data/build-facts';
import { PROJECT } from '../../data/project';

interface Artifact {
  file: string;
  what: string;
  bytes: number;
  checksum?: string | null;
}

@Component({
  selector: 'app-releases',
  imports: [Tag, Message, RouterLink],
  templateUrl: './releases.html',
  styleUrl: './releases.scss',
})
export class Releases {
  readonly project = PROJECT;
  readonly facts = BUILD_FACTS;

  /**
   * What a build currently produces. Sizes and checksums come from the last
   * compile rather than being maintained by hand, so a stale number here means
   * the site was built from a stale `dist/`.
   */
  readonly artifacts: Artifact[] = [
    {
      file: 'ratblocker-chromium.crx',
      what: 'Chromium extension, signed with the project key',
      bytes: BUILD_FACTS.artifacts.crxBytes,
    },
    {
      file: 'ratblocker-firefox-0.1.0.xpi',
      what: 'Firefox extension, unsigned',
      bytes: BUILD_FACTS.artifacts.xpiBytes,
    },
    {
      file: 'rules.rbdb',
      what: 'Compiled rule database, full',
      bytes: BUILD_FACTS.database.bytes,
      checksum: BUILD_FACTS.database.sha256,
    },
    {
      file: 'chromium/cosmetic.rbdb',
      what: 'Compiled rule database, cosmetic rules only',
      bytes: BUILD_FACTS.database.cosmeticBytes,
    },
  ];

  readonly changes = [
    'Shared Rust filtering engine, compiled natively and to WebAssembly.',
    'Filter compiler producing a versioned binary database plus Chromium rulesets.',
    'Chromium MV3 extension with near-complete EasyList coverage under the rule cap.',
    'Firefox extension running the full engine through blocking webRequest.',
    'Linux daemon: caching DNS proxy, D-Bus API, Polkit authorization, atomic filter updates with rollback.',
    'Command-line client for the daemon.',
  ];

  readonly known = [
    'No GNOME Shell extension or GTK settings application yet.',
    'No Android application yet.',
    'Filter-update signing is implemented but no signing key is published.',
    'No security audit has been carried out.',
  ];

  size(bytes: number): string {
    if (bytes === 0) return '—';
    const mib = bytes / 1024 / 1024;
    return mib >= 1 ? `${mib.toFixed(1)} MiB` : `${Math.round(bytes / 1024)} KiB`;
  }

  short(checksum: string | null | undefined): string {
    return checksum ? `${checksum.slice(0, 16)}…` : '—';
  }
}
