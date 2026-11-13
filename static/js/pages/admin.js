// admin.html - tabs: Users, Sessions, Audit, Settings, Master data, Godowns. Panes load lazily; live events refresh the active tab only.
import { boot, ERP } from '/js/shell.js';
import * as ui from '/js/ui.js';
import { mountUsers, mountSessions } from '/js/pages/admin-users.js';
import { mountAudit } from '/js/pages/admin-audit.js';
import { mountSettings } from '/js/pages/admin-settings.js';
import { mountMaster, mountGodowns } from '/js/pages/admin-master.js';
import { clearOptionsCache } from '/js/crud.js';

const { el } = ui;
const TABS = [
  { id: 'users', label: 'Users', icon: 'users', mount: mountUsers },
  { id: 'sessions', label: 'Sessions', icon: 'phone', mount: mountSessions },
  { id: 'audit', label: 'Audit log', icon: 'history', mount: mountAudit },
  { id: 'settings', label: 'Settings', icon: 'sliders', mount: mountSettings },
  { id: 'master', label: 'Master data', icon: 'package', mount: mountMaster },
  { id: 'godowns', label: 'Godowns', icon: 'warehouse', mount: mountGodowns },
];

const { me } = await boot({ page: 'admin', title: 'Administration' });
const main = document.getElementById('main');
const panes = {}; const inst = {};
let active = (location.hash || '').slice(1);
if (!TABS.some((t) => t.id === active)) active = 'users';

const tabs = ui.tabs({ tabs: TABS, active, label: 'Administration sections', onChange: (id) => show(id) });
ui.mount(main, el('div.sticky-tabs', {}, tabs.el), ...TABS.map((t) => { const p = el('section', { id: `pane-${t.id}`, role: 'tabpanel', 'aria-labelledby': `tab-${t.id}`, hidden: true }); panes[t.id] = p; return p; }));

function show(id) {
  active = id; history.replaceState(null, '', `#${id}`);
  TABS.forEach((t) => { panes[t.id].hidden = t.id !== id; });
  if (!inst[id]) { const t = TABS.find((x) => x.id === id); inst[id] = t.mount(panes[id], { onSaved: () => ERP.live.refresh() }); }
  else if (inst[id].refresh) inst[id].refresh();
  tabs.select(id);
}
window.addEventListener('hashchange', () => { const h = location.hash.slice(1); if (TABS.some((t) => t.id === h) && h !== active) show(h); });
show(active);

// live: only the active tab refreshes (and never an open dialog or unsaved settings form)
const refreshActive = () => { if (inst[active] && inst[active].refresh) inst[active].refresh(); };
ERP.live.start('admin', {
  admin: refreshActive,
  settings: refreshActive,
  users: () => { if (active === 'users' || active === 'sessions') refreshActive(); },
  master: () => { clearOptionsCache(); if (active === 'master' || active === 'godowns') refreshActive(); },
});
