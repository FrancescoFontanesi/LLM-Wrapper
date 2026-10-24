// Is there something in this page that the user has typed or attached and not sent?
// Answers a boolean; anything it cannot read counts as "yes" (keep the page).
// Looks at the top document only: text fields and text areas with a value, editable elements with
// text (the composers of the chat sites are `contenteditable`), password fields, and file inputs
// holding a file.
(function () {
  try {
    var fields = document.querySelectorAll(
      'textarea, input, [contenteditable=""], [contenteditable="true"], [contenteditable="plaintext-only"]');
    var texty = ['text', 'search', 'email', 'url', 'tel', 'password', 'number'];
    for (var i = 0; i < fields.length; i++) {
      var e = fields[i];
      if (e.tagName === 'INPUT') {
        var type = (e.getAttribute('type') || 'text').toLowerCase();
        if (type === 'file') { if (e.files && e.files.length) return true; continue; }
        if (texty.indexOf(type) < 0) continue;
        if (e.value && e.value.trim()) return true;
      } else if (e.tagName === 'TEXTAREA') {
        if (e.value && e.value.trim()) return true;
      } else if ((e.innerText || '').trim()) {
        return true;
      }
    }
    return false;
  } catch (x) {
    return true;
  }
})()
