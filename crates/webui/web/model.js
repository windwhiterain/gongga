// The whole view state a stateless frame front-end has to keep.
// Contract: crates/tui/src/run.rs apply_frame (which frames are control frames),
// crates/tui/src/tree.rs rows (the session list), crates/client/src/lib.rs
// Layout / Listed / QuotaRow, and docs/notes/webui-web.md for this page's limits.
//
// # Deliberate limit
//
// This page holds every row a window ever drew: it never trims blocks, so it never
// sends Held and never asks Newer for rows a budget dropped. A terminal's keep_rows
// is a memory concern of that process; the Render frame's keep_rows is therefore read
// and ignored. Older and Newer pages are still applied as frames, and Newer is still
// sent when the operator pushes against a pane's newest row. A Row frame names the
// transcript position that report would be in, so it is read and not kept. See
// docs/notes/tui-log.md.

import { Block, headLabel, headLineKind, lineKindOf, sessionRows } from './blocks.js';
import { applyCompletion, byteLength, indexToByte } from './cursor.js';
import * as intents from './intents.js';

function text(value, fallback) {
  return typeof value === 'string' ? value : fallback;
}

function count(value, fallback) {
  return Number.isInteger(value) ? value : fallback;
}

function listedOf(item) {
  return {
    name: text(item && item.name, ''),
    status: text(item && item.status, ''),
    title: text(item && item.title, ''),
  };
}

function headOf(head) {
  const source = head && typeof head === 'object' ? head : {};
  const fields = Array.isArray(source.fields) ? source.fields : [];
  return {
    kind: text(source.kind, 'call'),
    name: text(source.name, ''),
    description: text(source.description, ''),
    fields: fields.map((field) => ({
      label: text(field && field.label, ''),
      value: text(field && field.value, ''),
      summary: !!(field && field.summary),
    })),
  };
}

function spansOf(rows) {
  if (!Array.isArray(rows)) return [];
  return rows.map((row) => {
    if (!Array.isArray(row)) return [];
    return row.map((span) => ({
      text: text(span && span.text, ''),
      class: text(span && span.class, 'text'),
    }));
  });
}

export class ClientModel {
  constructor(send) {
    this.send = send;
    this.onChange = null;

    // Windows hold the blocks they drew, by name; a block is found by its key.
    this.windows = new Map();
    this.windowVersions = new Map();
    this.byKey = new Map();
    this.open = new Map();
    this.dirty = new Set();

    // The session list is a view of the layout: folded groups and unseen stops.
    this.folded = new Set();
    this.unseen = new Set();
    this.layout = { listed: [], shown: [], command: '', quota: [] };
    this.sessions = [];
    this.sessionsRev = 0;

    this.panes = [];
    this.commandWindow = '';
    this.commandBlocks = null;
    this.quota = [];
    this.quotaRev = 0;

    this.blockSize = 0;
    this.boxEdge = 2;
    this.keepRows = 0;
    this.sticky = '';

    this.statusText = 'starting';
    this.statusKind = 'system';
    this.serverText = '';
    this.serverOwn = false;

    this.inputText = '';
    this.inputCaret = 0;
    this.inputRev = 0;
    this.pendingComplete = false;
    this.forceComplete = false;
    this.lastAskedLine = null;
    this.lastAskedCaret = -1;

    this.completion = null;
    this.completionRev = 0;
    this.history = [];
    this.historyPosition = 0;

    this.toast = '';
    this.toastTimer = 0;
    this.pinSticky = false;
  }

  notify() {
    if (this.onChange !== null) this.onChange();
  }

  // Windows and blocks

  window(name) {
    let blocks = this.windows.get(name);
    if (blocks === undefined) {
      blocks = [];
      this.windows.set(name, blocks);
    }
    return blocks;
  }

  windowVersion(name) {
    return this.windowVersions.get(name) ?? 0;
  }

  bumpWindowVersion(name) {
    this.windowVersions.set(name, this.windowVersion(name) + 1);
  }

  dress(block) {
    block.capRows = this.blockSize;
    block.edgeColumns = this.boxEdge;
  }

  touch(block) {
    block.rev += 1;
    this.dirty.add(block);
  }

  addBlock(block) {
    this.dress(block);
    this.window(block.window).push(block);
    if (block.key.length !== 0) this.remember(block);
  }

  remember(block) {
    let list = this.byKey.get(block.key);
    if (list === undefined) {
      list = [];
      this.byKey.set(block.key, list);
    }
    list.push(block);
  }

  sealedPastBlock(name, row) {
    const block = new Block(
      name,
      text(row && row.key, ''),
      lineKindOf(text(row && row.kind, 'system')),
      text(row && row.tag, ''),
      text(row && row.text, ''),
    );
    block.sealed = true;
    this.dress(block);
    if (block.key.length !== 0) this.remember(block);
    return block;
  }

  findBlock(key) {
    const live = this.open.get(key);
    if (live !== undefined && !live.dropped) return live;
    const list = this.byKey.get(key);
    if (list === undefined) return null;
    for (let at = list.length - 1; at >= 0; at -= 1) {
      if (!list[at].dropped) return list[at];
    }
    return null;
  }

  dropOpen(name) {
    for (const [key, block] of this.open) {
      if (block.window === name) this.open.delete(key);
    }
  }

  // Frames

  applyFrame(frame) {
    const type = text(frame && frame.type, '');
    switch (type) {
      case 'line':
        this.applyLine(frame);
        break;
      case 'stream':
        this.applyStream(frame);
        break;
      case 'status':
        this.statusKind = 'system';
        this.statusText = text(frame.text, '');
        break;
      case 'note':
        this.statusKind = lineKindOf(text(frame.kind, 'system'));
        this.statusText = text(frame.text, '');
        break;
      case 'layout':
        this.applyLayout(frame.layout);
        break;
      case 'render':
        this.applyRender(frame);
        break;
      case 'document':
        this.applyDocument(frame);
        break;
      case 'rewrite':
        this.applyRewrite(frame);
        break;
      case 'tool_call':
        this.applyToolCall(frame);
        break;
      case 'relabel':
        this.applyRelabel(frame);
        break;
      case 'forget':
        this.applyForget(text(frame.window, ''));
        break;
      case 'clear':
        this.applyClear(text(frame.window, ''));
        break;
      case 'rewind':
        this.applyRewind(text(frame.window, ''));
        break;
      case 'older':
        this.applyOlder(frame);
        break;
      case 'newer':
        this.applyNewer(frame);
        break;
      case 'sticky_session':
        this.applySticky(text(frame.name, ''));
        break;
      case 'stopped':
        this.applyStopped(text(frame.session, ''), !!frame.failed);
        break;
      case 'draft':
        this.closeCompletion();
        this.setInput(text(frame.text, ''), text(frame.text, '').length);
        break;
      case 'server':
        this.serverText = text(frame.label, '');
        this.serverOwn = !!frame.ours;
        break;
      case 'repoint':
        this.applyRepoint();
        break;
      case 'vocabulary':
        this.forceComplete = true;
        break;
      case 'completion':
        this.applyCompletionFrame(frame);
        break;
      // A row's place in the transcript is only needed to report what this page
      // holds; it holds everything, so there is nothing to report (deliberate limit).
      case 'row':
        break;
      default:
        break;
    }
  }

  applyLine(frame) {
    const block = new Block(
      text(frame.window, ''),
      '',
      lineKindOf(text(frame.kind, 'system')),
      '',
      text(frame.text, ''),
    );
    block.sealed = true;
    this.addBlock(block);
  }

  applyStream(frame) {
    const name = text(frame.window, '');
    const key = text(frame.key, '');
    const kind = lineKindOf(text(frame.kind, 'system'));
    const prefix = text(frame.prefix, '');
    const body = text(frame.text, '');
    const done = !!frame.done;

    let live = this.open.get(key);
    if (live !== undefined) {
      if (live.tag !== prefix) {
        if (done && prefix.length !== 0) {
          live.tag = prefix;
          this.touch(live);
        } else {
          live.sealed = true;
          this.touch(live);
          this.open.delete(key);
          live = undefined;
        }
      }
    }
    if (live === undefined) {
      // A blank chunk opens nothing; inside a block it is still real text.
      if (body.trim().length === 0 && prefix.length === 0) return;
      live = new Block(name, key, kind, prefix, body);
      this.addBlock(live);
      this.open.set(key, live);
    } else {
      live.text += body;
      this.touch(live);
    }
    if (done) {
      live.sealed = true;
      this.touch(live);
      this.open.delete(key);
    }
  }

  applyDocument(frame) {
    const name = text(frame.window, '');
    const key = text(frame.key, '');
    let block = this.findBlock(key);
    if (block === null) {
      block = new Block(name, key, lineKindOf(text(frame.kind, 'system')), text(frame.tag, ''), '');
      this.addBlock(block);
    }
    block.kind = lineKindOf(text(frame.kind, 'system'));
    block.tag = text(frame.tag, '');
    block.rows = spansOf(frame.rows);
    block.head = null;
    block.sealed = !!frame.done;
    if (block.sealed) this.open.delete(key);
    else this.open.set(key, block);
    this.touch(block);
  }

  applyRewrite(frame) {
    const block = this.findBlock(text(frame.key, ''));
    if (block === null) return;
    block.text = text(frame.text, '');
    block.rows = null;
    this.touch(block);
  }

  applyToolCall(frame) {
    const name = text(frame.window, '');
    const key = text(frame.key, '');
    const head = headOf(frame.head);
    const kind = headLineKind(head.kind);
    const tag = headLabel(head);
    let block = this.findBlock(key);
    if (block === null) {
      block = new Block(name, key, kind, tag, '');
      this.addBlock(block);
    }
    block.kind = kind;
    block.tag = tag;
    block.head = head;
    block.text = '';
    block.rows = null;
    block.sealed = true;
    this.open.delete(key);
    this.touch(block);
  }

  applyRelabel(frame) {
    const block = this.findBlock(text(frame.key, ''));
    if (block === null) return;
    block.tag = text(frame.tag, '');
    block.kind = lineKindOf(text(frame.kind, 'system'));
    this.touch(block);
  }

  applyRender(frame) {
    this.blockSize = Math.max(0, count(frame.block_size, 0));
    this.boxEdge = Math.max(0, count(frame.box_edge, 2));
    this.keepRows = Math.max(0, count(frame.keep_rows, 0));
    for (const blocks of this.windows.values()) {
      for (const block of blocks) {
        block.capRows = this.blockSize;
        block.edgeColumns = this.boxEdge;
        this.touch(block);
      }
    }
  }

  applyLayout(source) {
    const layout = source && typeof source === 'object' ? source : {};
    this.layout = {
      listed: Array.isArray(layout.listed) ? layout.listed.map(listedOf) : [],
      shown: Array.isArray(layout.shown) ? layout.shown.filter((name) => typeof name === 'string') : [],
      command: text(layout.command, ''),
      quota: Array.isArray(layout.quota) ? layout.quota : [],
    };
    for (const name of this.layout.shown) this.unseen.delete(name);
    this.rebuildSessions();
    this.rebuildPanes();
    this.rebuildQuota();
    this.commandWindow = this.layout.command;
    this.commandBlocks = this.commandWindow.length === 0 ? null : this.window(this.commandWindow);
  }

  rebuildQuota() {
    this.quota = this.layout.quota.map((row) => ({
      provider: text(row && row.provider, ''),
      request: count(row && row.request, 0),
      text: text(row && row.text, ''),
      kind: lineKindOf(text(row && row.kind, 'system')),
    }));
    this.quotaRev += 1;
  }

  rebuildPanes() {
    const titles = new Map(this.layout.listed.map((row) => [row.name, row.title]));
    const existing = new Map(this.panes.map((pane) => [pane.name, pane]));
    this.panes = this.layout.shown
      .filter((name) => name.length !== 0)
      .map((name) => {
        const pane = existing.get(name) ?? { name };
        pane.blocks = this.window(name);
        pane.title = titles.get(name) ?? '';
        pane.sticky = name === this.sticky;
        return pane;
      });
  }

  applySticky(name) {
    if (name === this.sticky) return;
    this.sticky = name;
    for (const pane of this.panes) pane.sticky = pane.name === name;
    this.pinSticky = true;
  }

  applyStopped(session, failed) {
    if (!this.layout.shown.includes(session) && !this.unseen.has(session)) {
      this.unseen.add(session);
      this.rebuildSessions();
    }
    this.setToast(failed ? 'the turn failed' : 'the turn ended');
  }

  applyClear(name) {
    const blocks = this.windows.get(name);
    if (blocks !== undefined) {
      for (const block of blocks) block.dropped = true;
      blocks.length = 0;
    }
    this.bumpWindowVersion(name);
    this.dropOpen(name);
  }

  applyForget(name) {
    const blocks = this.windows.get(name);
    if (blocks !== undefined) {
      for (const block of blocks) block.dropped = true;
      blocks.length = 0;
      this.bumpWindowVersion(name);
      this.windows.delete(name);
    }
    this.dropOpen(name);
    this.unseen.delete(name);
  }

  applyRewind(name) {
    const blocks = this.windows.get(name);
    if (blocks !== undefined) {
      let at = blocks.length;
      while (at > 0 && !blocks[at - 1].sealed) {
        blocks[at - 1].dropped = true;
        at -= 1;
      }
      if (at !== blocks.length) {
        blocks.length = at;
        this.bumpWindowVersion(name);
      }
    }
    this.dropOpen(name);
  }

  applyRepoint() {
    for (const [name, blocks] of this.windows) {
      for (const block of blocks) block.dropped = true;
      blocks.length = 0;
      this.bumpWindowVersion(name);
    }
    this.byKey.clear();
    this.open.clear();
    this.commandBlocks = this.commandWindow.length === 0 ? null : this.window(this.commandWindow);
  }

  applyOlder(frame) {
    const name = text(frame.window, '');
    const rows = Array.isArray(frame.rows) ? frame.rows : [];
    const page = rows.map((row) => this.sealedPastBlock(name, row));
    if (page.length !== 0) this.window(name).unshift(...page);
  }

  applyNewer(frame) {
    const name = text(frame.window, '');
    const rows = Array.isArray(frame.rows) ? frame.rows : [];
    const page = rows.map((row) => this.sealedPastBlock(name, row));
    if (page.length !== 0) this.window(name).push(...page);
  }

  applyCompletionFrame(frame) {
    const line = text(frame.line, '');
    const items = Array.isArray(frame.items)
      ? frame.items.map((item) => ({
          label: text(item && item.label, ''),
          detail: text(item && item.detail, ''),
          new_text: text(item && item.new_text, ''),
          snippet: !!(item && item.snippet),
          range:
            item && Array.isArray(item.range) && Number.isInteger(item.range[0]) && Number.isInteger(item.range[1])
              ? [item.range[0], item.range[1]]
              : null,
        }))
      : [];
    this.completion = {
      line,
      cursor: Math.min(Math.max(0, count(frame.cursor, 0)), byteLength(line)),
      start: count(frame.start, 0),
      end: count(frame.end, 0),
      subject: typeof frame.subject === 'string' ? frame.subject : null,
      items,
      selected: 0,
    };
    this.completionRev += 1;
  }

  // The list is a projection of the layout; blocks.js owns how a tree makes rows.
  // Contract: crates/tui/src/tree.rs rows.
  rebuildSessions() {
    this.sessions = sessionRows(
      this.layout.listed,
      this.folded,
      this.unseen,
      this.layout.shown,
    );
    this.sessionsRev += 1;
  }
  // The input, its history and the completion popup.

  inputChanged(value, caret) {
    this.inputText = value;
    this.inputCaret = Math.min(Math.max(0, caret | 0), value.length);
    this.pendingComplete = true;
  }

  caretChanged(caret) {
    const at = Math.min(Math.max(0, caret | 0), this.inputText.length);
    if (at === this.inputCaret) return;
    this.inputCaret = at;
    this.pendingComplete = true;
  }

  setInput(value, caret) {
    this.inputText = value;
    this.inputCaret = Math.min(Math.max(0, caret | 0), value.length);
    this.inputRev += 1;
    this.pendingComplete = true;
  }

  takeCompletionRequest() {
    if (!this.pendingComplete && !this.forceComplete) return null;
    const force = this.forceComplete;
    this.pendingComplete = false;
    this.forceComplete = false;
    const line = this.inputText;
    const caret = this.inputCaret;
    if (!force && line === this.lastAskedLine && caret === this.lastAskedCaret) return null;
    this.lastAskedLine = line;
    this.lastAskedCaret = caret;
    return intents.complete(line, indexToByte(line, caret));
  }

  recomplete() {
    this.forceComplete = true;
  }

  completionVisible() {
    return (
      this.completion !== null &&
      this.completion.line === this.inputText &&
      this.completion.items.length !== 0
    );
  }

  closeCompletion() {
    if (this.completion === null) return;
    this.completion = null;
    this.completionRev += 1;
  }

  moveCompletion(delta) {
    if (!this.completionVisible()) return;
    const countItems = this.completion.items.length;
    this.completion.selected =
      (((this.completion.selected + delta) % countItems) + countItems) % countItems;
    this.completionRev += 1;
  }

  acceptCompletionAt(index) {
    if (!this.completionVisible()) return;
    const completion = this.completion;
    if (index < 0 || index >= completion.items.length) return;
    const applied = applyCompletion(
      this.inputText,
      completion.start,
      completion.end,
      completion.items[index],
    );
    this.closeCompletion();
    if (applied !== null) this.setInput(applied.text, applied.caret);
  }

  submit() {
    const line = this.inputText.trim();
    if (line.length === 0) return;
    this.history.push(this.inputText);
    this.historyPosition = this.history.length;
    this.send(intents.submit(line));
    this.closeCompletion();
    this.setInput('', 0);
    this.pinSticky = true;
  }

  recallHistory(direction) {
    if (this.history.length === 0) return;
    this.historyPosition = Math.min(Math.max(0, this.historyPosition + direction), this.history.length);
    const value = this.historyPosition === this.history.length ? '' : this.history[this.historyPosition];
    this.setInput(value, value.length);
  }

  // Gestures whose intent is not the input's.

  clickSession(session, toggle) {
    if (toggle) {
      const shown = this.layout.shown.includes(session);
      this.send(intents.renderSession(session, !shown, false));
    } else {
      this.send(intents.renderSession(session, true, true));
    }
  }

  clickPaneHeader(session) {
    this.send(intents.stickySession(session));
  }

  deleteStickySession() {
    if (this.sticky.length === 0) return;
    this.send(intents.deleteSession(this.sticky));
  }

  requestOlder(windows) {
    if (windows.length !== 0) this.send(intents.older(windows));
  }

  requestNewer(windows) {
    if (windows.length !== 0) this.send(intents.newer(windows));
  }

  toggleGroup(name) {
    if (!this.folded.delete(name)) this.folded.add(name);
    this.rebuildSessions();
  }

  toggleBlock(block) {
    block.expanded = !block.expanded;
    this.touch(block);
  }

  setToast(value) {
    this.toast = value;
    if (this.toastTimer !== 0) clearTimeout(this.toastTimer);
    this.toastTimer = 0;
    if (value.length === 0) return;
    this.toastTimer = setTimeout(() => {
      this.toast = '';
      this.toastTimer = 0;
      this.notify();
    }, 4000);
  }
}
