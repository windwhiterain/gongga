// The page's bootstrap: one WebSocket at /ws, the input gestures, and the
// one-render-per-animation-frame loop.
// Contract: crates/client/src/render_wire.rs (frames and intents),
// crates/tui/src/run.rs apply_frame (control frames), crates/tui/src/lib.rs (keys).
import { ClientModel } from './model.js';
import { View } from './view.js';
import * as intents from './intents.js';

const socketUrl = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws';

let socket = null;
let ended = false;
const queued = [];

function send(intent) {
  if (ended) return;
  const text = JSON.stringify(intent);
  if (socket !== null && socket.readyState === WebSocket.OPEN) socket.send(text);
  else queued.push(text);
}

const model = new ClientModel(send);
const view = new View(model, send);
model.onChange = () => view.scheduleRender();
view.mount();

const input = document.getElementById('input');

function connect() {
  socket = new WebSocket(socketUrl);
  socket.addEventListener('open', () => {
    for (const text of queued) socket.send(text);
    queued.length = 0;
  });
  socket.addEventListener('message', (event) => {
    if (ended || typeof event.data !== 'string') return;
    let frame;
    try {
      frame = JSON.parse(event.data);
    } catch {
      return;
    }
    if (frame === null || typeof frame !== 'object' || Array.isArray(frame)) return;
    model.applyFrame(frame);
    view.scheduleRender();
  });
  socket.addEventListener('close', (event) => {
    if (ended) return;
    ended = true;
    view.end('connection closed', event.reason && event.reason.length > 0 ? event.reason : 'the client ended');
  });
  // The close event carries the reason; an error is never shown twice.
  socket.addEventListener('error', () => {});
}

function quit() {
  if (ended) return;
  send(intents.quit());
  ended = true;
  view.end('closed', 'you asked to quit');
}

function prevent(event) {
  event.preventDefault();
}

// A user edit: the box is the source of the text and of the caret.
input.addEventListener('input', () => {
  model.inputChanged(input.value, input.selectionStart ?? 0);
  view.scheduleRender();
});

// A caret move with no text change is still a Complete request.
document.addEventListener('selectionchange', () => {
  if (document.activeElement !== input) return;
  model.caretChanged(input.selectionStart ?? 0);
  view.scheduleRender();
});

input.addEventListener('keydown', (event) => {
  // An IME owns the keyboard while it composes; Enter commits the composition there.
  if (event.isComposing) return;
  const key = event.key;
  const shift = event.shiftKey;
  const alt = event.altKey;

  if (key === 'Enter' && !shift && !alt) {
    prevent(event);
    model.submit();
    view.focusInput();
    view.scheduleRender();
    return;
  }
  // Shift+Enter and Alt+Enter are the box's own break.
  if (key === 'Enter') return;

  if (key === 'Tab') {
    prevent(event);
    if (model.completionVisible()) model.acceptCompletionAt(model.completion.selected);
    else model.recomplete();
    view.scheduleRender();
    return;
  }
  if (key === 'Escape') {
    model.closeCompletion();
    view.scheduleRender();
    return;
  }
  if (key === 'ArrowUp' || key === 'ArrowDown') {
    if (model.completionVisible()) {
      prevent(event);
      model.moveCompletion(key === 'ArrowUp' ? -1 : 1);
      view.scheduleRender();
      return;
    }
    if (shift) {
      prevent(event);
      model.recallHistory(key === 'ArrowUp' ? -1 : 1);
      view.scheduleRender();
      return;
    }
  }
  if (key === 'Delete' && shift && input.value.length === 0) {
    prevent(event);
    model.deleteStickySession();
  }
});

window.addEventListener('keydown', (event) => {
  const key = event.key;
  const control = event.ctrlKey || event.metaKey;
  if (control && (key === 'q' || key === 'Q')) {
    prevent(event);
    quit();
    return;
  }
  if (control && key === 'ArrowDown') {
    prevent(event);
    view.pinStickyToNewest();
    return;
  }
  if (key === 'PageUp') {
    prevent(event);
    view.pageSticky(-1);
    return;
  }
  if (key === 'PageDown') {
    prevent(event);
    view.pageSticky(1);
  }
});

connect();
