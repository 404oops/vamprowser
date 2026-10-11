//! Let web upload fields and page-owned drop zones keep their drops. Plain
//! files dropped on the rest of the page are browser navigations.

/// Runs in every frame: embedded uploaders often live in iframes, which
/// receive the drag events instead of the main frame. Wry's `window.ipc`
/// exists only in the main frame, so subframes post to the handler directly.
pub const SCRIPT: &str = r#"(() => {
  const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc;
  if (!handler) return;
  let last = null;
  const report = value => {
    if (value === last) return;
    last = value;
    handler.postMessage('file-drop-target:' + (value ? '1' : '0'));
  };
  const accepts = event => {
    const path = event.composedPath ? event.composedPath() : [event.target];
    return event.defaultPrevented || path.some(node => {
      if (!node || node.nodeType !== 1) return false;
      return node.isContentEditable ||
        (node.tagName === 'INPUT' && node.type === 'file') ||
        (node.tagName === 'LABEL' && node.control && node.control.type === 'file');
    });
  };
  // Entering a frame always reports, since another frame may have spoken last.
  addEventListener('dragenter', () => { last = null; report(false); }, true);
  addEventListener('dragover', event => report(accepts(event)));
  addEventListener('dragend', () => { last = null; report(false); }, true);
})()"#;
