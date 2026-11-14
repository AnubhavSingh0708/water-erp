// offline.html: retry button + automatic return when the server answers.
const go = () => { location.reload(); };
document.getElementById('retry').addEventListener('click', go);
window.addEventListener('online', go);
setInterval(() => { fetch('/api/health', { cache: 'no-store' }).then((r) => { if (r.ok) go(); }).catch(() => {}); }, 8000);
