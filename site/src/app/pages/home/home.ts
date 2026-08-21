import { Component } from '@angular/core';
import { RouterLink } from '@angular/router';
import { Button } from '@openng/optimus-ui/button';
import { Card } from '@openng/optimus-ui/card';
import { Tag } from '@openng/optimus-ui/tag';

import { BUILD_FACTS } from '../../data/build-facts';
import { PRIVACY_PROMISES, PROJECT } from '../../data/project';

@Component({
  selector: 'app-home',
  imports: [RouterLink, Button, Card, Tag],
  templateUrl: './home.html',
  styleUrl: './home.scss',
})
export class Home {
  readonly project = PROJECT;
  readonly facts = BUILD_FACTS;
  readonly promises = PRIVACY_PROMISES;

  /** Headline figures, all taken from the compiler's own output. */
  readonly stats = [
    {
      value: (BUILD_FACTS.rules.network + BUILD_FACTS.rules.exceptions).toLocaleString(),
      label: 'network rules compiled',
    },
    { value: BUILD_FACTS.rules.cosmetic.toLocaleString(), label: 'cosmetic rules' },
    { value: '9 µs', label: 'per filtering decision' },
    { value: '0', label: 'bytes of telemetry' },
  ];

  readonly layers = [
    {
      title: 'In the browser',
      body:
        'Full URL rules, resource types, tracking-parameter removal and element hiding. ' +
        'This is the layer that can tell a script from an image, and that can hide the gap ' +
        'a blocked ad leaves behind.',
      tag: 'Chromium & Firefox',
    },
    {
      title: 'In the player',
      body:
        'Ad breaks are pruned out of the video player response before the player reads them, ' +
        'so streaming ads never start. The pruning is first-party code in the extension, ' +
        'never executable code arriving from a filter list.',
      tag: 'Streaming',
    },
  ];
}
