import { Component } from '@angular/core';
import { Accordion, AccordionContent, AccordionHeader, AccordionPanel } from '@openng/optimus-ui/accordion';
import { Card } from '@openng/optimus-ui/card';
import { Message } from '@openng/optimus-ui/message';

import { BUILD_FACTS } from '../../data/build-facts';
import { LIMITATIONS } from '../../data/project';

interface Step {
  title: string;
  detail: string;
}

@Component({
  selector: 'app-how-it-works',
  imports: [Accordion, AccordionPanel, AccordionHeader, AccordionContent, Card, Message],
  templateUrl: './how-it-works.html',
  styleUrl: './how-it-works.scss',
})
export class HowItWorks {
  readonly facts = BUILD_FACTS;
  readonly limitations = LIMITATIONS;

  /** The decision pipeline, in the order the engine actually runs it. */
  readonly pipeline: Step[] = [
    { title: 'Normalize', detail: 'Parse and canonicalize the URL. Anything unparseable is allowed rather than guessed at.' },
    { title: 'Allowlist', detail: 'If you have allowed this site, stop here. Nothing outranks that.' },
    { title: 'Application policy', detail: 'An excluded application bypasses filtering entirely.' },
    { title: 'Your own rules', detail: 'Rules you wrote are consulted before any subscription.' },
    { title: 'Blocklists', detail: 'Hostname index first, then a token index. Never a scan of every rule.' },
    { title: 'Exceptions', detail: 'Exception rules can undo a block, unless the block is marked important.' },
    { title: 'Parameter removal', detail: 'Surviving requests get tracking parameters stripped from the query.' },
  ];

  readonly platforms = [
    {
      platform: 'Chromium',
      mechanism: 'declarativeNetRequest',
      engine: 'WebAssembly (cosmetic only)',
      note: 'MV3 forbids blocking webRequest, so rules are handed to the browser ahead of time.',
    },
    {
      platform: 'Firefox & Gecko forks',
      mechanism: 'Blocking webRequest',
      engine: 'WebAssembly (full)',
      note: 'Gecko still allows blocking interception, so the whole engine runs per request.',
    },
    {
      platform: 'Linux',
      mechanism: 'DNS proxy',
      engine: 'Native Rust',
      note: 'Covers every application, but only ever sees a hostname.',
    },
  ];
}
