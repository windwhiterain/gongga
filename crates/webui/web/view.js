// The drawing half: one DOM update per animation frame, pane measurements, and
// the scroll rules the terminal front-end follows.
// Contract: crates/tui/src/panes.rs (scroll, pin, Older/Newer), crates/tui/src/render.rs
// (session marks), crates/client/src/render_wire.rs Room / Columns.

import { createBlockElement, updateBlockElement } from './blocks.js';
import * as intents from './intents.js';

const ROW_HEIGHT = 20;
const MIN_COLUMNS = 20;
const PANE_HEADER_ESTIMATE = 52;

function make(tagName, className) {
  const element = document.createElement(tagName);
  if (className) element.className = className;
  return element;
}

function emptyBlocksState() {
  return { arrayRef: null, version: -1, count: 0, first: null, last: null, mounted: [] };
}

function sameRooms(left, right) {
  if (left.length !== right.length) return false;
  for (let at = 0; at < left.length; at += 1) {
    if (left[at][0] !== right[at][0] || left[at][1] !== right[at][1]) return false;
  }
  return true;
}

function sameColumns(left, right) {
  if (left.length !== right.length) return false;
  for (let at = 0; at < left.length; at += 1) {
    if (left[at][0] !== right[at][0] || left[at][1] !== right[at][1]) return false;
  }
  return true;
}

// Contract: crates/tui/src/render.rs status_mark -- shape says running, ink says
// how much attention the row asks for.
function markGlyph(rank) {
  if (rank === 1) return '\u25cf';
  if (rank === 3) return '\u2715';
  return '\u25cb';
}

function markKind(rank) {
  if (rank === 1) return 'result';
  if (rank === 2) return 'tool_call';
  if (rank === 3) return 'error';
  return 'result';
}

export class View {
  constructor(model, send) {
    this.model = model;
    this.send = send;

    this.listRows = document.getElementById('list-rows');
    this.listEmpty = document.getElementById('list-empty');
    this.panesList = document.getElementById('panes-list');
    this.panesEmpty = document.getElementById('panes-empty');
    this.quotaEl = document.getElementById('quota');
    this.commandEl = document.getElementById('command');
    this.commandBlocks = document.getElementById('command-blocks');
    this.completionEl = document.getElementById('completion');
    this.completionTitle = document.getElementById('completion-title');
    this.completionRows = document.getElementById('completion-rows');
    this.inputEl = document.getElementById('input');
    this.statusEl = document.getElementById('status');
    this.serverEl = document.getElementById('server');
    this.ownEl = document.getElementById('own');
    this.toastEl = document.getElementById('toast');
    this.overlayEl = document.getElementById('overlay');
    this.overlayState = document.getElementById('overlay-state');
    this.overlayReason = document.getElementById('overlay-reason');
    this.ruler = document.getElementById('ruler');
    this.listScroll = document.querySelector('.list-scroll');
    this.listHint = document.getElementById('list-hint');
    this.commandScroll = document.getElementById('command-scroll');

    this.panes = new Map();
    this.commandState = emptyBlocksState();
    this.proven = new Map();
    this.advance = 0;
    this.lastRooms = [];
    this.lastColumns = [];
    this.scheduled = false;
    this.ended = false;

    this.renderedSessionsRev = -1;
    this.renderedQuotaRev = -1;
    this.renderedInputRev = -1;
    this.renderedCompletionRev = -1;
    this.renderedToast = null;

    this.resizeObserver = null;
  }

  mount() {
    // The advance is measured on the first frame: a stylesheet still loading has not
    // given the ruler the face the rows will be drawn in.
    this.resizeObserver = new ResizeObserver(() => this.scheduleRender());
    this.resizeObserver.observe(this.panesList);
    this.wireList();
    this.wirePanes();
    this.wireCompletion();
    this.wireHints();
    this.scheduleRender();
  }

  scheduleRender() {
    if (this.scheduled) return;
    this.scheduled = true;
    requestAnimationFrame(() => {
      this.scheduled = false;
      this.render();
    });
  }

  focusInput() {
    this.inputEl.focus();
  }

  render() {
    const anchors = this.captureAnchors();
    this.syncDirtyBlocks();
    this.syncPanes();
    this.syncCommand();
    this.syncSessions();
    this.syncQuota();
    this.syncHeader();
    this.syncCompletion();
    this.syncInput();
    this.syncToast();
    this.syncEmpty();
    this.settleScroll(anchors);
    this.measure();
    this.trimPanes();
    this.askPinned();
    this.updateHints();
    this.flushCompletion();
  }

  // Blocks

  syncDirtyBlocks() {
    for (const block of this.model.dirty) {
      if (block.el !== null) updateBlockElement(block.el, block);
    }
    this.model.dirty.clear();
  }

  syncBlocks(state, container, blocks, version) {
    if (state.arrayRef !== blocks || state.version !== version) {
      this.rebuildBlocks(state, container, blocks, version);
      return;
    }
    if (blocks.length === state.count) return;
    if (blocks.length < state.count) {
      this.rebuildBlocks(state, container, blocks, version);
      return;
    }
    if (state.count === 0) {
      this.appendBlocks(state, container, blocks, 0);
      return;
    }
    if (blocks[0] === state.first) {
      this.appendBlocks(state, container, blocks, state.count);
      return;
    }
    if (blocks[blocks.length - state.count] === state.first) {
      this.prependBlocks(state, container, blocks, blocks.length - state.count);
      return;
    }
    this.rebuildBlocks(state, container, blocks, version);
  }

  appendBlocks(state, container, blocks, from) {
    for (let at = from; at < blocks.length; at += 1) {
      const element = createBlockElement(blocks[at]);
      blocks[at].el = element;
      container.append(element);
      state.mounted.push(blocks[at]);
    }
    this.rememberShape(state, blocks);
  }

  prependBlocks(state, container, blocks, count) {
    const anchor = container.firstChild;
    for (let at = 0; at < count; at += 1) {
      const element = createBlockElement(blocks[at]);
      blocks[at].el = element;
      container.insertBefore(element, anchor);
    }
    state.mounted = blocks.slice(0, count).concat(state.mounted);
    this.rememberShape(state, blocks);
  }

  rebuildBlocks(state, container, blocks, version) {
    for (const block of state.mounted) block.el = null;
    container.replaceChildren();
    state.mounted = [];
    state.arrayRef = blocks;
    this.appendBlocks(state, container, blocks, 0);
    state.version = version;
  }

  rememberShape(state, blocks) {
    state.arrayRef = blocks;
    state.count = blocks.length;
    state.first = blocks.length === 0 ? null : blocks[0];
    state.last = blocks.length === 0 ? null : blocks[blocks.length - 1];
  }

  // A page above the reader moves the rows under the pointer; the browser only
  // moves a scroll offset when the pane is at its newest row.
  captureAnchors() {
    const anchors = new Map();
    for (const [name, entry] of this.panes) {
      const anchor = entry.body.firstElementChild;
      anchors.set(name, {
        anchor,
        top: anchor === null ? 0 : anchor.getBoundingClientRect().top,
        height: entry.body.scrollHeight,
      });
    }
    return anchors;
  }

  settleScroll(anchors) {
    const pin = this.model.pinSticky;
    this.model.pinSticky = false;
    for (const pane of this.model.panes) {
      const entry = this.panes.get(pane.name);
      if (entry === undefined) continue;
      const body = entry.body;
      const bottom = Math.max(0, body.scrollHeight - body.clientHeight);
      if (pin && pane.name === this.model.sticky) {
        body.scrollTop = bottom;
        entry.state.follow = true;
        entry.state.wheelOlderAsked = false;
      } else if (entry.state.follow) {
        // A following pane sits on its newest row, however the content changed:
        // a frame that arrived, or a trim that dropped rows under it.
        body.scrollTop = bottom;
      } else if (!entry.state.follow && anchors !== undefined) {
        const before = anchors.get(pane.name);
        if (before !== undefined) {
          let grew = 0;
          if (before.anchor !== null && before.anchor.isConnected && body.contains(before.anchor)) {
            grew = before.anchor.getBoundingClientRect().top - before.top;
          } else {
            grew = body.scrollHeight - before.height;
          }
          if (grew > 0) body.scrollTop += grew;
        }
      }
      entry.state.seenBottom = bottom;
    }
  }

  // Pane measurements

  measureAdvance() {
    const width = this.ruler.getBoundingClientRect().width;
    return width > 1 ? width / 10 : 8;
  }

  measureBox(entry) {
    const row = entry.body.querySelector('.doc-row');
    if (row !== null && row.clientWidth > 0) return row.clientWidth;
    const body = entry.body.querySelector('.body');
    if (body !== null && body.clientWidth > 0) return body.clientWidth;
    this.panePadding(entry);
    return Math.max(0, entry.body.clientWidth - entry.padX);
  }

  // Padding is read once the pane is in the document: a detached element matches
  // none of the stylesheet, so its computed box is not the box it will draw in.
  panePadding(entry) {
    if (entry.padX === null) {
      const style = getComputedStyle(entry.body);
      entry.padX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight) || 0;
      entry.padY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) || 0;
    }
    return entry.padY;
  }

  paneCapacity(entry) {
    const viewport = entry.body.clientHeight - this.panePadding(entry);
    return viewport > 0 ? Math.max(1, Math.floor(viewport / ROW_HEIGHT)) : 0;
  }

  predictedRoom() {
    for (const entry of this.panes.values()) {
      const capacity = this.paneCapacity(entry);
      if (capacity > 0) return capacity;
    }
    const height = this.panesList.clientHeight - PANE_HEADER_ESTIMATE - 16;
    return Math.max(1, Math.floor(height / ROW_HEIGHT));
  }

  measure() {
    if (this.advance <= 0) this.advance = this.measureAdvance();
    const rooms = [];
    const measured = [];
    for (const pane of this.model.panes) {
      const entry = this.panes.get(pane.name);
      if (entry === undefined) continue;
      const capacity = this.paneCapacity(entry);
      if (capacity > 0) rooms.push([pane.name, capacity]);
      const box = this.measureBox(entry);
      if (box <= 0) continue;
      let columns = Math.max(MIN_COLUMNS, Math.floor(box / this.advance));
      const proven = this.proven.get(pane.name);
      if (proven !== undefined && Math.abs(proven.box - box) < 0.5) {
        columns = Math.min(columns, proven.columns);
      }
      measured.push({ name: pane.name, box, columns });
    }
    if (!sameRooms(rooms, this.lastRooms)) {
      this.lastRooms = rooms;
      if (rooms.length !== 0) this.send(intents.room(rooms));
    }
    const columns = measured.map((one) => [one.name, one.columns]);
    if (!sameColumns(columns, this.lastColumns)) {
      this.lastColumns = columns;
      if (columns.length !== 0) this.send(intents.columns(columns));
      return;
    }
    // A document row is one line. A row that takes two was measured too wide, so one
    // column comes off the pane; the accepted width is remembered per box so the
    // estimate cannot widen it back on the next pass.
    const narrowed = new Set();
    for (const one of measured) {
      if (one.columns <= MIN_COLUMNS) continue;
      const entry = this.panes.get(one.name);
      if (entry === undefined) continue;
      if (entry.state.lastBox !== 0 && Math.abs(entry.state.lastBox - one.box) > 0.5) continue;
      const rows = entry.body.querySelectorAll('.doc-row');
      for (const row of rows) {
        if (row.getBoundingClientRect().height <= ROW_HEIGHT + 0.5) continue;
        // A row holding more characters than the stated width was wrapped at an
        // older, wider statement and says nothing about this one.
        if (row.textContent.length > one.columns) continue;
        narrowed.add(one.name);
        break;
      }
    }
    for (const one of measured) {
      const entry = this.panes.get(one.name);
      if (entry !== undefined) entry.state.lastBox = one.box;
    }
    if (narrowed.size === 0) return;
    for (const one of measured) {
      if (narrowed.has(one.name)) {
        this.proven.set(one.name, { box: one.box, columns: one.columns - 1 });
      }
    }
    this.lastColumns = measured.map((one) => [
      one.name,
      narrowed.has(one.name) ? one.columns - 1 : one.columns,
    ]);
    this.send(intents.columns(this.lastColumns));
  }


  // TUI panes.rs trim: applied by the frame that drew the pane, because only drawing
  // knows how tall the pane is and how far it is scrolled. Dropped rows stay in the
  // transcript; Held is what tells the client where this window now stops.
  trimPanes() {
    if (this.model.keepRows === 0) return;
    let changed = false;
    for (const pane of this.model.panes) {
      const entry = this.panes.get(pane.name);
      if (entry === undefined) continue;
      if (this.trimOne(pane.name, entry.body, this.paneCapacity(entry), entry)) changed = true;
    }
    if (this.model.commandBlocks !== null && this.commandScroll !== null) {
      const capacity = Math.max(1, Math.floor(this.commandScroll.clientHeight / ROW_HEIGHT));
      if (this.trimOne(this.model.commandWindow, this.commandScroll, capacity, null)) changed = true;
    }
    const ranges = this.model.takeHeld();
    if (ranges.length !== 0) this.send(intents.held(ranges));
    if (changed) this.scheduleRender();
  }

  // One container's budget: what it draws, plus keep_rows, plus what the reader
  // scrolled through. The heights are the drawn ones, so nothing here guesses.
  trimOne(name, body, capacity, entry) {
    if (capacity <= 0) return false;
    const keep = this.model.keepRows;
    const bottom = Math.max(0, body.scrollHeight - body.clientHeight);
    const scrollRows = Math.max(0, Math.round((bottom - body.scrollTop) / ROW_HEIGHT));
    // A window inside its budget needs no per-block measurement. The newest end is
    // still trimmed when the reader scrolled past the margin, so that is not optional.
    const totalRows = Math.round(body.scrollHeight / ROW_HEIGHT);
    if (scrollRows <= keep && totalRows <= keep + capacity + scrollRows) return false;
    const rowsOf = new Map();
    const pixelsOf = new Map();
    for (const element of body.querySelectorAll('.block')) {
      const block = element._block;
      if (block === undefined) continue;
      const style = getComputedStyle(element);
      const outer = element.getBoundingClientRect().height
        + (parseFloat(style.marginTop) || 0) + (parseFloat(style.marginBottom) || 0);
      pixelsOf.set(block, outer);
      rowsOf.set(block, Math.max(1, Math.round(outer / ROW_HEIGHT)));
    }
    const result = this.model.trimWindow(name, capacity, scrollRows, rowsOf, pixelsOf);
    if (result === null) return false;
    // Rows dropped above the reader shrink the height above them, so the rows on
    // screen stay the same rows. A pane on its newest row is left to the browser's
    // own clamp, which keeps it there when the content shrinks.
    if (result.oldestPixels > 0 && scrollRows > 2) {
      body.scrollTop = Math.max(0, body.scrollTop - result.oldestPixels);
    }
    if (entry !== null) entry.state.seenBottom = -1;
    return true;
  }

  // A frame that cannot fill its pane is the one asking for the page above it.
  askPinned() {
    for (const pane of this.model.panes) {
      const entry = this.panes.get(pane.name);
      if (entry === undefined) continue;
      if (entry.body.scrollTop <= 0) {
        if (!entry.state.pinnedAsked) {
          entry.state.pinnedAsked = true;
          this.model.requestOlder([pane.name]);
        }
      } else {
        entry.state.pinnedAsked = false;
      }
    }
  }

  // Panes

  createEntry(name) {
    const el = make('article', 'pane');
    el.dataset.name = name;
    const header = make('header', 'pane-header');
    const heading = make('div', 'pane-heading');
    const nameEl = make('span', 'pane-name');
    const stickyEl = make('span', 'chip');
    stickyEl.textContent = 'sticky';
    stickyEl.hidden = true;
    heading.append(nameEl, stickyEl);
    const titleEl = make('div', 'pane-title');
    header.append(heading, titleEl);
    const body = make('div', 'pane-body');
    el.append(header, body);
    const entry = {
      name,
      el,
      nameEl,
      titleEl,
      stickyEl,
      body,
      blocksState: emptyBlocksState(),
      padX: null,
      padY: null,
      state: {
        follow: true,
        seenBottom: -1,
        wheelOlderAsked: false,
        wheelNewerAsked: false,
        pinnedAsked: false,
        lastBox: 0,
      },
    };
    body.addEventListener('scroll', () => this.onPaneScroll(entry), { passive: true });
    body.addEventListener('wheel', (event) => this.onPaneWheel(entry, event), { passive: false });
    const hint = make('button', 'bottom-hint pane-hint');
    hint.type = 'button';
    hint.textContent = '\u2193 bottom \u00b7 Ctrl+\u2193';
    hint.hidden = true;
    hint.addEventListener('click', () => {
      this.pinSticky(entry.name);
      this.scheduleRender();
    });
    entry.hint = hint;
    el.append(hint);
    this.resizeObserver.observe(body);
    return entry;
  }

  releaseEntry(entry) {
    for (const block of entry.blocksState.mounted) block.el = null;
    entry.blocksState.mounted = [];
    this.resizeObserver.unobserve(entry.body);
    entry.el.remove();
  }

  syncPanes() {
    const wanted = new Set(this.model.panes.map((pane) => pane.name));
    for (const [name, entry] of Array.from(this.panes)) {
      if (wanted.has(name)) continue;
      this.releaseEntry(entry);
      this.panes.delete(name);
    }
    for (let index = 0; index < this.model.panes.length; index += 1) {
      const pane = this.model.panes[index];
      let entry = this.panes.get(pane.name);
      if (entry === undefined) {
        entry = this.createEntry(pane.name);
        this.panes.set(pane.name, entry);
      }
      const current = this.panesList.children[index];
      if (current !== entry.el) this.panesList.insertBefore(entry.el, current ?? null);
      entry.nameEl.textContent = pane.name;
      entry.titleEl.textContent = pane.title;
      entry.titleEl.hidden = pane.title.length === 0;
      entry.stickyEl.hidden = !pane.sticky;
      entry.el.classList.toggle('sticky', pane.sticky);
      this.syncBlocks(entry.blocksState, entry.body, pane.blocks, this.model.windowVersion(pane.name));
    }
  }

  syncCommand() {
    const blocks = this.model.commandBlocks;
    this.commandEl.hidden = blocks === null;
    if (blocks === null) return;
    this.syncBlocks(
      this.commandState,
      this.commandBlocks,
      blocks,
      this.model.windowVersion(this.model.commandWindow),
    );
  }

  onPaneScroll(entry) {
    const body = entry.body;
    const bottom = Math.max(0, body.scrollHeight - body.clientHeight);
    entry.state.follow = body.scrollTop >= bottom - 2;
    if (body.scrollTop > 0) entry.state.wheelOlderAsked = false;
    if (body.scrollTop < bottom - 2) entry.state.wheelNewerAsked = false;
  }

  // TUI lib.rs wheel_rows: a notch is three rows, one with a gesture modifier. The
  // wheel moves what it is over -- an opened item's box, a pane, the list -- and a box
  // only when its own content holds the pointer: SPEC's box_edge columns are the
  // pane's, and a body that fits is no scroll target. See crates/tui/src/log.rs box_at.
  onPaneWheel(entry, event) {
    event.preventDefault();
    const step = event.altKey ? 1 : 3;
    const direction = event.deltaY < 0 ? -1 : 1;
    const box = this.boxUnder(entry, event);
    if (box !== null) {
      const before = box.scrollTop;
      const limit = Math.max(0, box.scrollHeight - box.clientHeight);
      box.scrollTop = Math.min(Math.max(0, box.scrollTop + direction * step * ROW_HEIGHT), limit);
      if (box.scrollTop !== before) {
        entry.state.wheelOlderAsked = false;
        entry.state.wheelNewerAsked = false;
        this.scheduleRender();
      }
      // The cell decides once: a box at an end still holds the gesture.
      return;
    }
    const body = entry.body;
    const before = body.scrollTop;
    body.scrollTop += direction * step * ROW_HEIGHT;
    if (body.scrollTop !== before) {
      entry.state.wheelOlderAsked = false;
      entry.state.wheelNewerAsked = false;
      this.scheduleRender();
    }
    this.afterWheel(entry, direction);
  }

  // What a wheel at an end of a pane asks for: one page per visit to that end.
  afterWheel(entry, direction) {
    const body = entry.body;
    const bottom = Math.max(0, body.scrollHeight - body.clientHeight);
    if (direction < 0 && body.scrollTop <= 0 && !entry.state.wheelOlderAsked) {
      entry.state.wheelOlderAsked = true;
      this.model.requestOlder([entry.name]);
    } else if (direction > 0 && body.scrollTop >= bottom - 2 && !entry.state.wheelNewerAsked) {
      entry.state.wheelNewerAsked = true;
      this.model.requestNewer([entry.name]);
    }
  }

  // The opened box holding the pointer, or null when the pane takes the gesture.
  boxUnder(entry, event) {
    const target = event.target && event.target.closest ? event.target.closest('.box-body') : null;
    if (target === null || !entry.body.contains(target)) return null;
    if (target.scrollHeight <= target.clientHeight + 1) return null;
    const block = target.closest('.block');
    if (block === null) return null;
    // SPEC's box_edge: the columns each side of a box belong to the window's wheel.
    const rect = block.getBoundingClientRect();
    const edge = Math.max(0, this.model.boxEdge) * this.advance;
    if (event.clientX < rect.left + edge || event.clientX > rect.right - edge) return null;
    return target;
  }

  // SPEC's hot-key: a view away from its bottom shows the way back, and only then.
  updateHints() {
    for (const entry of this.panes.values()) {
      if (entry.hint === undefined) continue;
      const bottom = entry.body.scrollHeight - entry.body.clientHeight;
      entry.hint.hidden = bottom <= 2 || entry.body.scrollTop >= bottom - 2;
    }
    if (this.listHint !== null && this.listScroll !== null) {
      const bottom = this.listScroll.scrollHeight - this.listScroll.clientHeight;
      this.listHint.hidden = bottom <= 2 || this.listScroll.scrollTop >= bottom - 2;
    }
  }

  wireHints() {
    if (this.listScroll !== null) {
      this.listScroll.addEventListener('wheel', (event) => this.onListWheel(event), { passive: false });
    }
    if (this.listHint !== null) {
      this.listHint.addEventListener('click', () => {
        if (this.listScroll !== null) this.listScroll.scrollTop = this.listScroll.scrollHeight;
        this.scheduleRender();
      });
    }
  }

  // The list is its own scroll, in whole rows like every other view.
  onListWheel(event) {
    if (this.listScroll === null) return;
    event.preventDefault();
    const step = event.altKey ? 1 : 3;
    const direction = event.deltaY < 0 ? -1 : 1;
    this.listScroll.scrollTop += direction * step * ROW_HEIGHT;
    this.scheduleRender();
  }

  pinSticky(name) {
    const entry = this.panes.get(name);
    if (entry === undefined) return;
    const body = entry.body;
    body.scrollTop = Math.max(0, body.scrollHeight - body.clientHeight);
    entry.state.follow = true;
    entry.state.wheelOlderAsked = false;
    entry.state.wheelNewerAsked = false;
  }

  pinStickyToNewest() {
    this.pinSticky(this.model.sticky);
  }

  // TUI lib.rs page_height: a page is the pane's own rows, not a fraction of it.
  pageSticky(direction) {
    const entry = this.panes.get(this.model.sticky);
    if (entry === undefined) return;
    const body = entry.body;
    const capacity = Math.max(1, this.paneCapacity(entry));
    const bottom = Math.max(0, body.scrollHeight - body.clientHeight);
    body.scrollTop = Math.min(Math.max(0, body.scrollTop + direction * capacity * ROW_HEIGHT), bottom);
    entry.state.follow = body.scrollTop >= bottom - 2;
    entry.state.wheelOlderAsked = false;
    if (body.scrollTop < bottom - 2) entry.state.wheelNewerAsked = false;
    this.scheduleRender();
  }

  // Remaining view state

  syncSessions() {
    const model = this.model;
    if (this.renderedSessionsRev !== model.sessionsRev) {
      this.renderedSessionsRev = model.sessionsRev;
      const fragment = document.createDocumentFragment();
      for (const row of model.sessions) {
        const el = make('div', 'list-row' + (row.shown ? ' shown' : ''));
        el.dataset.name = row.name;
        const fold = make('span', 'list-fold' + (row.group ? '' : ' leaf'));
        fold.textContent = row.group ? (row.collapsed ? '\u203a' : '\u2304') : '';
        const mark = make('span', 'list-mark kind-' + markKind(row.mark));
        mark.textContent = markGlyph(row.mark);
        const main = make('div', 'list-text');
        const label = make('span', 'list-label');
        label.textContent = row.label;
        label.style.paddingLeft = row.depth * 12 + 'px';
        main.append(label);
        if (row.title.length !== 0) {
          const title = make('span', 'list-title');
          title.textContent = row.title;
          title.style.paddingLeft = row.depth * 12 + 'px';
          main.append(title);
        }
        el.append(fold, mark, main);
        fragment.append(el);
      }
      this.listRows.replaceChildren(fragment);
    }
    this.listEmpty.hidden = model.sessions.length !== 0;
  }

  syncQuota() {
    const model = this.model;
    if (this.renderedQuotaRev !== model.quotaRev) {
      this.renderedQuotaRev = model.quotaRev;
      const fragment = document.createDocumentFragment();
      for (const row of model.quota) {
        const chip = make('div', 'quota-chip kind-' + row.kind);
        chip.textContent = row.text;
        fragment.append(chip);
      }
      this.quotaEl.replaceChildren(fragment);
    }
    this.quotaEl.hidden = model.quota.length === 0;
  }

  syncHeader() {
    this.statusEl.textContent = this.model.statusText;
    this.statusEl.className = 'status kind-' + this.model.statusKind;
    this.serverEl.textContent = this.model.serverText;
    this.ownEl.hidden = !this.model.serverOwn;
  }

  syncCompletion() {
    const model = this.model;
    const visible = model.completionVisible();
    this.completionEl.hidden = !visible;
    if (!visible) return;
    const completion = model.completion;
    if (this.renderedCompletionRev !== model.completionRev) {
      this.renderedCompletionRev = model.completionRev;
      const subject = completion.subject === null ? '' : completion.subject;
      this.completionTitle.textContent = subject;
      this.completionTitle.hidden = subject.length === 0;
      const fragment = document.createDocumentFragment();
      for (let at = 0; at < completion.items.length; at += 1) {
        const item = completion.items[at];
        const row = make('div', 'completion-row' + (at === completion.selected ? ' selected' : ''));
        row.dataset.index = String(at);
        const label = make('div', 'completion-label');
        label.textContent = item.label;
        const detail = make('div', 'completion-detail');
        detail.textContent = item.detail;
        row.append(label, detail);
        fragment.append(row);
      }
      this.completionRows.replaceChildren(fragment);
      const selected = this.completionRows.querySelector('.completion-row.selected');
      if (selected !== null) selected.scrollIntoView({ block: 'nearest' });
    }
  }

  syncInput() {
    const model = this.model;
    if (this.renderedInputRev === model.inputRev) return;
    this.renderedInputRev = model.inputRev;
    this.inputEl.value = model.inputText;
    const caret = Math.min(model.inputCaret, this.inputEl.value.length);
    this.inputEl.setSelectionRange(caret, caret);
  }

  syncToast() {
    if (this.renderedToast === this.model.toast) return;
    this.renderedToast = this.model.toast;
    this.toastEl.textContent = this.model.toast;
  }

  syncEmpty() {
    this.panesEmpty.hidden = this.model.panes.length !== 0;
  }

  flushCompletion() {
    const request = this.model.takeCompletionRequest();
    if (request !== null) this.send(request);
  }

  // Gestures over the drawn rows

  wireList() {
    this.listRows.addEventListener('click', (event) => {
      const rowEl = event.target.closest('.list-row');
      if (rowEl === null) return;
      const row = this.model.sessions.find((one) => one.name === rowEl.dataset.name);
      if (row === undefined) return;
      const foldHit = event.target.closest('.list-fold');
      if ((foldHit !== null && row.group) || !row.session) {
        this.model.toggleGroup(row.name);
        this.scheduleRender();
        return;
      }
      // The client owns the selection: name the gesture and the session rows
      // the list currently draws, and let window_select resolve the span.
      const gesture = event.shiftKey || event.altKey
        ? 'range'
        : event.ctrlKey || event.metaKey
          ? 'toggle'
          : 'plain';
      if (gesture !== 'range' && !row.shown) {
        this.send(intents.room([[row.name, this.predictedRoom()]]));
      }
      const visible = this.model.sessions
        .filter((one) => one.session)
        .map((one) => one.name);
      this.model.clickSession(row.name, gesture, visible);
      this.scheduleRender();
    });
  }

  wirePanes() {
    this.panesList.addEventListener('click', (event) => {
      const header = event.target.closest('.pane-header');
      if (header !== null) {
        const card = header.closest('.pane');
        if (card !== null) this.model.clickPaneHeader(card.dataset.name);
        this.scheduleRender();
        return;
      }
      const blockEl = event.target.closest('.block');
      if (blockEl === null || blockEl._block === undefined) return;
      const block = blockEl._block;
      if (!block.foldable) return;
      if (block.boxed) {
        // SPEC's box_edge: a press in the margin is the window's, not the box's.
        const rect = blockEl.getBoundingClientRect();
        const edge = Math.max(0, this.model.boxEdge) * this.advance;
        if (event.clientX < rect.left + edge || event.clientX > rect.right - edge) return;
      }
      this.model.toggleBlock(block);
      this.scheduleRender();
    });
  }

  wireCompletion() {
    this.completionRows.addEventListener('mousedown', (event) => event.preventDefault());
    this.completionRows.addEventListener('click', (event) => {
      const rowEl = event.target.closest('.completion-row');
      if (rowEl === null) return;
      this.model.acceptCompletionAt(Number(rowEl.dataset.index));
      this.scheduleRender();
    });
  }

  end(state, reason) {
    if (this.ended) return;
    this.ended = true;
    this.model.closeCompletion();
    this.inputEl.disabled = true;
    this.overlayState.textContent = state;
    this.overlayReason.textContent = reason;
    this.overlayEl.hidden = false;
    this.scheduleRender();
  }
}
