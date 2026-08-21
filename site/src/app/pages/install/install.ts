import { Component } from '@angular/core';
import { Message } from '@openng/optimus-ui/message';
import { Tab, TabList, TabPanel, TabPanels, Tabs } from '@openng/optimus-ui/tabs';
import { Tag } from '@openng/optimus-ui/tag';

import { PROJECT } from '../../data/project';

@Component({
  selector: 'app-install',
  imports: [Tabs, TabList, Tab, TabPanels, TabPanel, Message, Tag],
  templateUrl: './install.html',
  styleUrl: './install.scss',
})
export class Install {
  readonly project = PROJECT;
}
