// UI -> client intents, exactly as serde writes them.
// Contract: crates/client/src/render_wire.rs UiIntent and
// crates/protocol/src/message.rs Operation. Tuple pairs are arrays, never objects.
export const complete = (line, cursor) => ({ intent: 'complete', line, cursor });

export const submit = (line) => ({ intent: 'submit', line });

export const quit = () => ({ intent: 'quit' });

export const older = (windows) => ({ intent: 'older', windows });

export const newer = (windows) => ({ intent: 'newer', windows });

export const held = (windows) => ({ intent: 'held', windows });

export const room = (windows) => ({ intent: 'room', windows });

export const columns = (windows) => ({ intent: 'columns', windows });

export const renderSession = (session, show, solo) => ({
  intent: 'operate',
  operation: { op: 'render_session', session, show, solo },
});

export const stickySession = (session) => ({
  intent: 'operate',
  operation: { op: 'sticky_session', session },
});

export const deleteSession = (session) => ({
  intent: 'operate',
  operation: { op: 'delete_session', session },
});
