// Scroll a session's transcript to a message from elsewhere in the UI (e.g. the context meter).
// The mounted Transcript registers itself per session; nothing happens when none is mounted.
const handlers = new Map<string, (messageId: string) => void>();

export function registerTranscriptScroll(sessionId: string, fn: (messageId: string) => void): () => void {
  handlers.set(sessionId, fn);
  return () => {
    if (handlers.get(sessionId) === fn) handlers.delete(sessionId);
  };
}

export function scrollToMessage(sessionId: string, messageId: string): boolean {
  const fn = handlers.get(sessionId);
  if (!fn) return false;
  fn(messageId);
  return true;
}
