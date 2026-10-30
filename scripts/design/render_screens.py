"""Рендер Window.dc.html (холст Design) в PNG без рантайма холста:
мини-интерпретатор {{holes}}, <sc-for>, <sc-if> + Playwright.

python scripts/design/render_screens.py docs/design/window.dc.html docs/design/screens [имя ...]
Имена (без .png) — перерисовать только эти снимки; без них — все."""
import json
import os
import sys

from playwright.sync_api import sync_playwright

SRC, OUT = sys.argv[1], sys.argv[2]
ONLY = set(sys.argv[3:])
os.makedirs(OUT, exist_ok=True)
template = open(SRC, encoding="utf-8").read()

JS = r"""
([src, props]) => {
  const doc = new DOMParser().parseFromString(src, 'text/html');
  const code = doc.querySelector('script[data-dc-script]').textContent;
  class DCLogic { constructor(p) { this.props = p; this.state = {}; } setState() {} }
  const Component = new Function('DCLogic', code + '\nreturn Component;')(DCLogic);
  const vals = new Component(props).renderVals();
  const look = (scope, path) => {
    path = path.trim();
    if (path === 'true') return true; if (path === 'false') return false;
    if (/^-?\d+(\.\d+)?$/.test(path)) return Number(path);
    return path.split('.').reduce((o, k) => (o == null ? undefined : o[k]), scope);
  };
  const whole = /^\{\{([^}]+)\}\}$/;
  const interp = (s, scope) => s.replace(/\{\{([^}]+)\}\}/g, (_, p) => { const v = look(scope, p); return v == null ? '' : String(v); });
  const SVG = 'http://www.w3.org/2000/svg';
  const render = (node, scope, parent, inSvg) => {
    if (node.nodeType === 3) { parent.appendChild(document.createTextNode(interp(node.textContent, scope))); return; }
    if (node.nodeType !== 1) return;
    const tag = node.localName;
    if (tag === 'helmet' || tag === 'script') return;
    if (tag === 'sc-for') {
      const list = look(scope, node.getAttribute('list').match(whole)[1]) || [];
      const as = node.getAttribute('as');
      list.forEach((it, i) => { const sc = Object.assign({}, scope, { [as]: it, $index: i }); node.childNodes.forEach((c) => render(c, sc, parent, inSvg)); });
      return;
    }
    if (tag === 'sc-if') {
      if (look(scope, node.getAttribute('value').match(whole)[1])) node.childNodes.forEach((c) => render(c, scope, parent, inSvg));
      return;
    }
    const svg = inSvg || tag === 'svg';
    const el = svg ? document.createElementNS(SVG, tag) : document.createElement(tag);
    for (const a of node.attributes) {
      if (a.name.startsWith('hint-') || a.name.startsWith('on')) continue;
      const m = a.value.match(whole);
      if (m) {
        const v = look(scope, m[1]);
        if (v === undefined || v === null || v === false || typeof v === 'function') continue;
        el.setAttribute(a.name, v === true ? '' : String(v));
      } else el.setAttribute(a.name, interp(a.value, scope));
    }
    node.childNodes.forEach((c) => render(c, scope, el, svg));
    parent.appendChild(el);
  };
  document.body.style.margin = '0';
  doc.querySelector('x-dc').childNodes.forEach((c) => render(c, vals, document.body, false));
}
"""

SHOTS = [
    ("theme-system-light", {"theme": "light"}),
    ("theme-system-dark", {"theme": "dark"}),
    ("theme-blue", {"theme": "blue"}),
    ("theme-turquoise", {"theme": "turquoise"}),
    ("theme-grass", {"theme": "grass"}),
    ("theme-sepia", {"theme": "sepia"}),
    ("screen-menu", {"screen": "menu"}),
    ("screen-quick", {"screen": "quick"}),
    ("screen-brief", {"screen": "brief"}),
    ("screen-tree", {"screen": "tree"}),
    ("screen-info", {"screen": "info"}),
    ("screen-quickview", {"screen": "quickview"}),
    ("screen-drives", {"screen": "drives"}),
    ("screen-archive", {"screen": "archive"}),
    ("screen-errors", {"screen": "errors"}),
    ("screen-terminal", {"screen": "terminal"}),
    ("screen-termhalf", {"screen": "termhalf"}),
    ("screen-context", {"screen": "context"}),
    ("screen-select", {"screen": "select"}),
    ("screen-journal", {"screen": "journal"}),
    ("screen-multirename", {"screen": "multirename"}),
    ("screen-compare", {"screen": "compare"}),
    ("screen-copy", {"screen": "copy"}),
    ("screen-delete", {"screen": "delete"}),
    ("screen-conflict", {"screen": "conflict"}),
    ("screen-mkdir", {"screen": "mkdir"}),
    ("screen-mkdirtree", {"screen": "mkdirtree"}),
    ("screen-move", {"screen": "move"}),
    ("screen-rename", {"screen": "rename"}),
    ("screen-attrs", {"screen": "attrs"}),
    ("screen-sort", {"screen": "sort"}),
    ("screen-filter", {"screen": "filter"}),
    ("screen-filtered", {"screen": "filtered"}),
    ("screen-bookmarks", {"screen": "bookmarks"}),
    ("screen-history", {"screen": "history"}),
    ("screen-help", {"screen": "help"}),
    ("screen-about", {"screen": "about"}),
    ("screen-usermenu", {"screen": "usermenu"}),
    ("screen-usermenuedit", {"screen": "usermenuedit"}),
    ("screen-pack", {"screen": "pack"}),
    ("screen-pack-pw", {"screen": "pack-pw"}),
    ("screen-columns", {"screen": "columns"}),
    ("screen-columnswide", {"screen": "columnswide"}),
    ("screen-jobslist", {"screen": "jobslist"}),
    ("screen-dragpanel", {"screen": "dragpanel"}),
    ("screen-dragfolder", {"screen": "dragfolder"}),
    ("screen-cmdcomplete", {"screen": "cmdcomplete"}),
    ("screen-cmdhistory", {"screen": "cmdhistory"}),
    ("screen-termsearch", {"screen": "termsearch"}),
    ("screen-termshell", {"screen": "termshell"}),
    ("screen-viewerhex", {"screen": "viewerhex"}),
    ("screen-viewerenc", {"screen": "viewerenc"}),
    ("screen-editreplace", {"screen": "editreplace"}),
    ("screen-editgoto", {"screen": "editgoto"}),
    ("screen-searchpanel", {"screen": "searchpanel"}),
    ("screen-searchgrep", {"screen": "searchgrep"}),
    ("screen-firstrun", {"screen": "firstrun"}),
    ("screen-whatsnew", {"screen": "whatsnew"}),
    ("screen-settings-viewer", {"screen": "settings-viewer"}),
    ("screen-settings-terminal", {"screen": "settings-terminal"}),
    ("screen-settings-ops", {"screen": "settings-ops"}),
    ("screen-settings-policies", {"screen": "settings-policies"}),
    ("screen-operror", {"screen": "operror"}),
    ("screen-opbusy", {"screen": "opbusy"}),
    ("screen-toasterror", {"screen": "toasterror"}),
    ("screen-tabsmany", {"screen": "tabsmany"}),
    ("screen-tabsdrag", {"screen": "tabsdrag"}),
    ("screen-kit", {"screen": "kit"}),
    ("screen-motion", {"screen": "motion"}),
    ("screen-a11yfocus", {"screen": "a11yfocus"}),
    ("screen-a11ysheet", {"screen": "a11ysheet"}),
    ("screen-settings-a11y", {"screen": "settings-a11y"}),
    ("screen-utilmenu", {"screen": "utilmenu"}),
    ("screen-duscan", {"screen": "duscan"}),
    ("screen-dulist", {"screen": "dulist"}),
    ("screen-ducached", {"screen": "ducached"}),
    ("screen-dunoaccess", {"screen": "dunoaccess"}),
    ("screen-dumap", {"screen": "dumap"}),
    ("screen-dutypes", {"screen": "dutypes"}),
    ("screen-dudups", {"screen": "dudups"}),
    ("screen-dutemp", {"screen": "dutemp"}),
    ("screen-pkginstalled", {"screen": "pkginstalled"}),
    ("screen-pkgupdates", {"screen": "pkgupdates"}),
    ("screen-pkgbanner", {"screen": "pkgbanner"}),
    ("screen-settings-pkg", {"screen": "settings-pkg"}),
    ("screen-pkgsearch", {"screen": "pkgsearch"}),
    ("screen-pkgmgrpick", {"screen": "pkgmgrpick"}),
    ("screen-pkginstall", {"screen": "pkginstall"}),
    ("screen-sshdrives", {"screen": "sshdrives"}),
    ("screen-sshnew", {"screen": "sshnew"}),
    ("screen-sshhostkey", {"screen": "sshhostkey"}),
    ("screen-sshhostchanged", {"screen": "sshhostchanged"}),
    ("screen-sshpass", {"screen": "sshpass"}),
    ("screen-sftppanel", {"screen": "sftppanel"}),
    ("screen-sftpcopy", {"screen": "sftpcopy"}),
    ("screen-sftpedit", {"screen": "sftpedit"}),
    ("screen-sftplost", {"screen": "sftplost"}),
    ("screen-sftpsync", {"screen": "sftpsync"}),
    ("screen-sshpair", {"screen": "sshpair"}),
    ("screen-sshmulti", {"screen": "sshmulti"}),
    ("screen-sshrec", {"screen": "sshrec"}),
    ("screen-sshrecview", {"screen": "sshrecview"}),
    ("screen-termhl", {"screen": "termhl"}),
    ("screen-settings-ssh", {"screen": "settings-ssh"}),
    ("screen-settings-conn", {"screen": "settings-conn"}),
    ("screen-extract", {"screen": "extract"}),
    ("screen-extractpw", {"screen": "extractpw"}),
    ("screen-link", {"screen": "link"}),
    ("screen-touch", {"screen": "touch"}),
    ("screen-linkview", {"screen": "linkview"}),
    ("screen-syncplan", {"screen": "syncplan"}),
    ("screen-syncmirror", {"screen": "syncmirror"}),
    ("screen-syncrun", {"screen": "syncrun"}),
    ("screen-clipcut", {"screen": "clipcut"}),
    ("screen-clippaste", {"screen": "clippaste"}),
    ("screen-zoomind", {"screen": "zoomind"}),
    ("zoom-125-panels", {"screen": "panels", "zoom": "125"}),
    ("zoom-150-panels", {"screen": "panels", "zoom": "150"}),
    ("zoom-150-copy", {"screen": "copy", "zoom": "150"}),
    ("en-panels", {"screen": "panels", "lang": "en"}),
    ("en-copy", {"screen": "copy", "lang": "en"}),
    ("en-delete", {"screen": "delete", "lang": "en"}),
    ("en-menu", {"screen": "menu", "lang": "en"}),
    ("en-settings", {"screen": "settings", "lang": "en"}),
    ("en-jobslist", {"screen": "jobslist", "lang": "en"}),
    ("kit-dark", {"screen": "kit", "theme": "dark"}),
    ("width-1024-panels", {"screen": "panels", "width": "1024"}),
    ("width-800-panels", {"screen": "panels", "width": "800"}),
    ("width-1024-copy", {"screen": "copy", "width": "1024"}),
    ("width-800-settings", {"screen": "settings", "width": "800"}),
    ("theme-hcblack", {"theme": "hcblack"}),
    ("theme-hcdesert", {"theme": "hcdesert"}),
    ("theme-hcblack-copy", {"theme": "hcblack", "screen": "copy"}),
    ("screen-jobs", {"screen": "progress"}),
    ("screen-search", {"screen": "search"}),
    ("screen-viewer", {"screen": "viewer"}),
    ("screen-viewer-md", {"screen": "viewer-md"}),
    ("screen-editor", {"screen": "editor"}),
    ("screen-editor-md", {"screen": "editor-md"}),
    ("screen-settings", {"screen": "settings"}),
    ("screen-settings-panels", {"screen": "settings-panels"}),
    ("screen-settings-keys", {"screen": "settings-keys"}),
    ("screen-settings-editor", {"screen": "settings-editor"}),
    ("context-admin", {"context": "admin"}),
    ("density-20-color-icons", {"density": "20", "icons": "color"}),
]

with sync_playwright() as p:
    b = p.chromium.launch()
    pg = b.new_page(viewport={"width": 1280, "height": 760}, device_scale_factor=1)
    for name, props in SHOTS:
        if ONLY and name not in ONLY:
            continue
        pg.set_content("<!doctype html><html><head><meta charset='utf-8'></head><body></body></html>")
        pg.evaluate(JS, [template, props])
        pg.screenshot(path=os.path.join(OUT, name + ".png"))
    b.close()
print(json.dumps([n for n, _ in SHOTS if not ONLY or n in ONLY]))
