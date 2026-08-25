import { Component } from '@angular/core';
import { Button } from '@openng/optimus-ui/button';
import { Message } from '@openng/optimus-ui/message';
import { Tab, TabList, TabPanel, TabPanels, Tabs } from '@openng/optimus-ui/tabs';
import { Tag } from '@openng/optimus-ui/tag';

import { AMO, PROJECT } from '../../data/project';

@Component({
  selector: 'app-install',
  imports: [Tabs, TabList, Tab, TabPanels, TabPanel, Message, Tag, Button],
  templateUrl: './install.html',
  styleUrl: './install.scss',
})
export class Install {
  readonly project = PROJECT;
  readonly amo = AMO;
}
